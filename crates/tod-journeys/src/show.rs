//! `tod-journeys show <file>` (spec §9.5, §10): a readable timeline of an
//! already-decrypted, decompressed journey/bundle file, followed by the
//! resolved references section.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use tod_journey::{Actor, Event, JourneyReader, Presented, Record, Resolution};

pub fn run(path: &Path, full: bool) -> Result<()> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let records: Vec<Record> = JourneyReader::from_reader(file).collect();

    let mut resolved: Vec<&Record> = Vec::new();
    let mut prev_at: Option<i64> = None;
    for record in &records {
        if let Event::Resolved { .. } = &record.event {
            resolved.push(record);
            continue;
        }
        print_timeline_line(record, prev_at);
        prev_at = Some(record.at);
    }

    if !resolved.is_empty() {
        println!();
        println!("Resolved references:");
        for record in resolved {
            print_resolved_line(record, full);
        }
    }

    Ok(())
}

fn print_timeline_line(record: &Record, prev_at: Option<i64>) {
    let when = format_time(record.at);
    let gap = prev_at.map(|p| format_gap(record.at - p)).unwrap_or_default();
    println!(
        "{when} {:>8}  seq={:<6} {:<14} {}",
        gap,
        record.seq,
        describe_actor(&record.actor),
        describe_event(&record.event)
    );
}

fn format_time(at_micros: i64) -> String {
    chrono::DateTime::from_timestamp_micros(at_micros)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| at_micros.to_string())
}

/// Renders a duration (microseconds) the way a human would read a gap
/// between events, e.g. "+2m14s". Zero or negative (first record, or clock
/// oddities) prints as empty via the caller's default.
fn format_gap(delta_micros: i64) -> String {
    if delta_micros <= 0 {
        return String::new();
    }
    let total_secs = delta_micros / 1_000_000;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("+{hours}h{mins}m{secs}s")
    } else if mins > 0 {
        format!("+{mins}m{secs}s")
    } else if total_secs > 0 {
        format!("+{secs}s")
    } else {
        let millis = delta_micros / 1000;
        format!("+{millis}ms")
    }
}

fn describe_actor(actor: &Actor) -> String {
    match actor {
        Actor::User => "user".to_string(),
        Actor::App => "app".to_string(),
        Actor::Agent { conversation } => format!("agent:{conversation}"),
    }
}

/// Finds the primary button in a `Presented` snapshot, if any.
fn primary_action(presented: &Presented) -> Option<&str> {
    presented
        .actions
        .iter()
        .find(|a| a.primary)
        .map(|a| a.id.as_str())
}

fn describe_event(event: &Event) -> String {
    match event {
        Event::None => "-".to_string(),
        Event::UserAction { action, surface, presented, .. } => {
            let mut line = format!("user action: {action}");
            if !surface.is_empty() {
                line.push_str(&format!(" ({surface})"));
            }
            if let Some(primary) = primary_action(presented) {
                if primary != action {
                    line.push_str(&format!(" [primary: {primary}]"));
                }
            }
            line
        }
        Event::Transition { from, to } => format!("transition {from} -> {to}"),
        Event::GateResult { from, to, criteria, .. } => {
            let failed = criteria.iter().filter(|c| c.outcome != "pass").count();
            if failed > 0 {
                format!("gate {from} -> {to} ({failed} failing criteria)")
            } else {
                format!("gate {from} -> {to}")
            }
        }
        Event::Validity { regression } => match regression {
            Some(r) => format!("validity regressed: {}", r.target),
            None => "validity holds".to_string(),
        },
        Event::ProtocolDecision { protocol, decision, .. } => {
            format!("{protocol} decision: {decision:?}")
        }
        Event::AgentTurn { phase, .. } => format!("agent turn: {phase:?}"),
        Event::SessionRotated { reason, .. } => format!("session rotated: {reason}"),
        Event::DataChanged { rows } => format!("data changed ({} rows)", rows.len()),
        Event::Milestone { state } => format!("milestone: {state}"),
        Event::Report { note, .. } => format!("report: {note}"),
        Event::Submission { bundle, status } => format!("submission {bundle}: {status}"),
        Event::Nav { what } => format!("nav: {what:?}"),
        Event::SettingsChanged { key, value } => format!("setting {key} = {value}"),
        Event::Manifest { manifest } => format!("manifest for bundle {}", manifest.bundle),
        Event::Settings { .. } => "settings snapshot".to_string(),
        Event::Resolved { reference, .. } => {
            format!("resolved {}:{}", reference.kind, reference.id)
        }
    }
}

fn print_resolved_line(record: &Record, full: bool) {
    let Event::Resolved { reference, content } = &record.event else {
        return;
    };
    match content {
        Resolution::Missing => {
            println!("  {}:{} — missing", reference.kind, reference.id);
        }
        Resolution::Withheld { id, size, .. } => {
            println!("  {}:{} — withheld ({size} bytes)", reference.kind, id);
        }
        Resolution::Found { data } => {
            if full {
                let text = String::from_utf8_lossy(&data.bytes);
                println!(
                    "  {}:{} — {} ({} bytes)\n{text}",
                    reference.kind,
                    reference.id,
                    data.mime,
                    data.bytes.len()
                );
            } else {
                println!(
                    "  {}:{} — {} ({} bytes)",
                    reference.kind,
                    reference.id,
                    data.mime,
                    data.bytes.len()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_journey::bundle::BundleWriter;
    use tod_journey::{PresentedAction, Reference};

    fn write_bundle(events: Vec<(Actor, Event)>) -> std::path::PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.journey");
        let mut w = BundleWriter::new().unwrap();
        for (actor, event) in events {
            w.append(actor, event).unwrap();
        }
        let compressed = w.finish().unwrap();
        let plain = tod_journey::bundle::decompress(&compressed).unwrap();
        std::fs::write(&path, &plain).unwrap();
        // Keep tempdir alive by leaking it; the test just needs the file to
        // exist for the duration of the call.
        std::mem::forget(dir);
        path
    }

    #[test]
    fn prints_one_line_per_record_without_erroring() {
        let path = write_bundle(vec![(Actor::User, Event::Milestone { state: "sent".into() })]);
        run(&path, false).unwrap();
    }

    #[test]
    fn marks_a_non_primary_user_action() {
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
        let path = write_bundle(vec![(
            Actor::User,
            Event::UserAction {
                action: "fix".into(),
                source: "panel".into(),
                surface: "lifecycle".into(),
                presented,
            },
        )]);
        // Capture stdout is awkward in a plain test; instead exercise the
        // pure formatter directly.
        let line = describe_event(&Event::UserAction {
            action: "fix".into(),
            source: "panel".into(),
            surface: "lifecycle".into(),
            presented: Presented {
                actions: vec![PresentedAction {
                    id: "gate check".into(),
                    label: "Gate check".into(),
                    primary: true,
                    disabled: false,
                }],
                focused: None,
                notices: vec![],
            },
        });
        assert!(line.contains("[primary: gate check]"), "line was: {line}");
        run(&path, false).unwrap();
    }

    #[test]
    fn resolved_references_render_abbreviated_and_full() {
        let path = write_bundle(vec![(
            Actor::App,
            Event::Resolved {
                reference: Reference {
                    kind: "transcript".into(),
                    id: "t1".into(),
                    from_seq: None,
                    to_seq: None,
                },
                content: Resolution::Found {
                    data: tod_journey::Blob { mime: "text/plain".into(), bytes: b"hello".to_vec() },
                },
            },
        )]);
        run(&path, false).unwrap();
        run(&path, true).unwrap();
    }
}
