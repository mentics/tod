//! `tod-journeys show <file>` (spec §9.5): a minimal one-line-per-record
//! timeline of an already-decrypted, decompressed journey/bundle file. The
//! fuller analysis tool is a later step; this just needs to be readable.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use tod_journey::{Actor, Event, JourneyReader};

pub fn run(path: &Path) -> Result<()> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    for record in JourneyReader::from_reader(file) {
        let when = chrono::DateTime::from_timestamp_micros(record.at)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| record.at.to_string());
        println!(
            "{when}  seq={:<6} {:<14} {}",
            record.seq,
            describe_actor(&record.actor),
            describe_event(&record.event)
        );
    }
    Ok(())
}

fn describe_actor(actor: &Actor) -> String {
    match actor {
        Actor::User => "user".to_string(),
        Actor::App => "app".to_string(),
        Actor::Agent { conversation } => format!("agent:{conversation}"),
    }
}

fn describe_event(event: &Event) -> String {
    match event {
        Event::None => "-".to_string(),
        Event::UserAction { action, .. } => format!("user action: {action}"),
        Event::Transition { from, to } => format!("transition {from} -> {to}"),
        Event::GateResult { from, to, .. } => format!("gate {from} -> {to}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tod_journey::bundle::BundleWriter;

    #[test]
    fn prints_one_line_per_record_without_erroring() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.journey");

        let mut w = BundleWriter::new().unwrap();
        w.append(Actor::User, Event::Milestone { state: "sent".into() }).unwrap();
        let compressed = w.finish().unwrap();
        let plain = tod_journey::bundle::decompress(&compressed).unwrap();
        std::fs::write(&path, &plain).unwrap();

        run(&path).unwrap();
    }
}
