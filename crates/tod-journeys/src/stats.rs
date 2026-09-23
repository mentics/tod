//! `tod-journeys stats <dir>` (spec §10): recurring patterns across all
//! received journey/bundle files in a directory.
//!
//! Time-in-state accounting: each `Transition { from, to }` closes out the
//! time spent in `from` (the gap since the previous `Transition` in the same
//! file is attributed to the state the node was leaving, i.e. keyed by
//! `from`) — a node's very first transition has no prior transition to
//! measure from, so it contributes nothing. This undercounts time still
//! being spent in whatever state a file ends in, but that open interval has
//! no end event to bound it, so there is nothing else honest to report.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use tod_journey::{Decision, Event, JourneyReader, Record};

#[derive(Default)]
struct Stats {
    /// (surface, lifecycle state) -> (not-primary count, total count).
    not_primary: BTreeMap<(String, String), (u64, u64)>,
    /// action -> (next action -> count).
    next_action: BTreeMap<String, BTreeMap<String, u64>>,
    /// lifecycle state -> total microseconds spent in it.
    time_in_state: BTreeMap<String, i64>,
    /// criterion id -> failure count.
    gate_failures: BTreeMap<String, u64>,
    /// (protocol, stop reason) -> count.
    protocol_stops: BTreeMap<(String, String), u64>,
}

pub fn run(dir: &Path) -> Result<()> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().map(|e| e == "journey").unwrap_or(false)
        })
        .collect();
    files.sort();

    let mut stats = Stats::default();
    for path in &files {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let records: Vec<Record> = JourneyReader::from_reader(file).collect();
        accumulate(&records, &mut stats);
    }

    print_report(&files, &stats);
    Ok(())
}

fn accumulate(records: &[Record], stats: &mut Stats) {
    let mut lifecycle_state = String::new();
    let mut last_transition_at: Option<i64> = None;
    let mut last_action: Option<String> = None;

    for record in records {
        match &record.event {
            Event::Transition { from, to } => {
                if let Some(prev_at) = last_transition_at {
                    let delta = record.at - prev_at;
                    if delta > 0 {
                        *stats.time_in_state.entry(from.clone()).or_insert(0) += delta;
                    }
                }
                lifecycle_state = to.clone();
                last_transition_at = Some(record.at);
            }
            Event::UserAction { action, surface, presented, .. } => {
                let key = (surface.clone(), lifecycle_state.clone());
                let entry = stats.not_primary.entry(key).or_insert((0, 0));
                entry.1 += 1;
                let primary = presented.actions.iter().find(|a| a.primary).map(|a| a.id.as_str());
                if primary.is_some() && primary != Some(action.as_str()) {
                    entry.0 += 1;
                }

                if let Some(prev) = &last_action {
                    *stats
                        .next_action
                        .entry(prev.clone())
                        .or_default()
                        .entry(action.clone())
                        .or_insert(0) += 1;
                }
                last_action = Some(action.clone());
            }
            Event::GateResult { criteria, .. } => {
                for c in criteria {
                    if c.outcome != "pass" {
                        *stats.gate_failures.entry(c.id.clone()).or_insert(0) += 1;
                    }
                }
            }
            Event::ProtocolDecision { protocol, decision, .. } => {
                if let Decision::Stop { stop, .. } = decision {
                    *stats
                        .protocol_stops
                        .entry((protocol.clone(), stop.clone()))
                        .or_insert(0) += 1;
                }
            }
            _ => {}
        }
    }
}

fn print_report(files: &[std::path::PathBuf], stats: &Stats) {
    println!("Scanned {} journey file(s).", files.len());

    println!("\nNon-primary action clicks (surface, lifecycle state -> not-primary/total):");
    if stats.not_primary.is_empty() {
        println!("  (none)");
    }
    for ((surface, state), (not_primary, total)) in &stats.not_primary {
        let surface = if surface.is_empty() { "-" } else { surface };
        let state = if state.is_empty() { "-" } else { state };
        println!("  {surface} / {state}: {not_primary}/{total}");
    }

    println!("\nAction most often followed by:");
    if stats.next_action.is_empty() {
        println!("  (none)");
    }
    for (action, followers) in &stats.next_action {
        if let Some((next, count)) = followers.iter().max_by_key(|(_, c)| **c) {
            println!("  {action} -> {next} ({count}x)");
        }
    }

    println!("\nTime spent in each lifecycle state:");
    if stats.time_in_state.is_empty() {
        println!("  (none)");
    }
    for (state, micros) in &stats.time_in_state {
        println!("  {state}: {}", format_duration(*micros));
    }

    println!("\nGate failures per criterion:");
    if stats.gate_failures.is_empty() {
        println!("  (none)");
    }
    for (criterion, count) in &stats.gate_failures {
        println!("  {criterion}: {count}");
    }

    println!("\nProtocol stop reasons:");
    if stats.protocol_stops.is_empty() {
        println!("  (none)");
    }
    for ((protocol, stop), count) in &stats.protocol_stops {
        println!("  {protocol} / {stop}: {count}");
    }
}

fn format_duration(micros: i64) -> String {
    let total_secs = micros / 1_000_000;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{hours}h{mins}m{secs}s")
    } else if mins > 0 {
        format!("{mins}m{secs}s")
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_journey::bundle::BundleWriter;
    use tod_journey::{Actor, Presented, PresentedAction};

    #[test]
    fn counts_a_fix_then_gate_check_sequence_where_gate_check_was_primary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.journey");

        let presented = Presented {
            actions: vec![PresentedAction {
                id: "gate check".into(),
                label: "Gate check".into(),
                primary: true,
                disabled: false,
            }],
            focused: None,
            notices: vec![],
        };

        let mut w = BundleWriter::new().unwrap();
        w.append(
            Actor::User,
            Event::UserAction {
                action: "fix".into(),
                source: "panel".into(),
                surface: "lifecycle".into(),
                presented: presented.clone(),
            },
        )
        .unwrap();
        w.append(
            Actor::User,
            Event::UserAction {
                action: "gate check".into(),
                source: "panel".into(),
                surface: "lifecycle".into(),
                presented,
            },
        )
        .unwrap();
        let compressed = w.finish().unwrap();
        let plain = tod_journey::bundle::decompress(&compressed).unwrap();
        std::fs::write(&path, &plain).unwrap();

        let file = File::open(&path).unwrap();
        let records: Vec<Record> = JourneyReader::from_reader(file).collect();
        let mut stats = Stats::default();
        accumulate(&records, &mut stats);

        let key = ("lifecycle".to_string(), String::new());
        assert_eq!(stats.not_primary.get(&key), Some(&(1, 2)));

        let followers = stats.next_action.get("fix").unwrap();
        assert_eq!(followers.get("gate check"), Some(&1));

        // Exercise the full `run` path too, matching the plan's "hand-built
        // bundle...counted by stats" requirement end to end.
        run(dir.path()).unwrap();
    }
}
