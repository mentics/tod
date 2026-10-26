//! A readable timeline of a journey: one line per record, with the gap since
//! the previous one, and the references resolved into a bundle listed after.
//! Shared by `tod-cli journey show`, `tod-journeys show`, and the app's
//! journey viewer, so all three read a journey the same way.

use crate::record::{Actor, Event, Presented, Record, Resolution};

/// What part of a journey to render.
#[derive(Debug, Clone, Copy, Default)]
pub struct Window {
    /// Only records at or after this time (unix microseconds).
    pub since_micros: Option<i64>,
    /// Only the last this many records (after `since_micros`).
    pub last: Option<usize>,
    /// Print resolved references' content, not just their sizes.
    pub full: bool,
}

/// One timeline line for `record`; `prev_at` is the previous record's time.
pub fn line(record: &Record, prev_at: Option<i64>) -> String {
    let when = format_time(record.at);
    let gap = prev_at.map(|p| format_gap(record.at - p)).unwrap_or_default();
    format!(
        "{when} {:>8}  seq={:<6} {:<14} {}",
        gap,
        record.seq,
        describe_actor(&record.actor),
        describe_event(&record.event)
    )
}

/// The timeline of `records` within `window`, then any resolved references.
pub fn render(records: &[Record], window: Window) -> String {
    let mut selected: Vec<&Record> = records
        .iter()
        .filter(|r| window.since_micros.is_none_or(|since| r.at >= since))
        .collect();
    if let Some(last) = window.last {
        let skip = selected.len().saturating_sub(last);
        selected.drain(..skip);
    }

    let mut out = String::new();
    let mut resolved: Vec<&Record> = Vec::new();
    let mut prev_at: Option<i64> = None;
    for record in selected {
        if let Event::Resolved { .. } = &record.event {
            resolved.push(record);
            continue;
        }
        out.push_str(&line(record, prev_at));
        out.push('\n');
        prev_at = Some(record.at);
    }
    if !resolved.is_empty() {
        out.push_str("\nResolved references:\n");
        for record in resolved {
            out.push_str(&resolved_line(record, window.full));
            out.push('\n');
        }
    }
    out
}

pub fn format_time(at_micros: i64) -> String {
    chrono::DateTime::from_timestamp_micros(at_micros)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| at_micros.to_string())
}

/// Renders a duration (microseconds) the way a human would read a gap
/// between events, e.g. "+2m14s". Zero or negative (first record, or clock
/// oddities) prints as empty via the caller's default.
pub fn format_gap(delta_micros: i64) -> String {
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

pub fn describe_actor(actor: &Actor) -> String {
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

pub fn describe_event(event: &Event) -> String {
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
        Event::NextActorChanged { from, to, detail } => {
            let from = if from.is_empty() { "(new)" } else { from.as_str() };
            if detail.is_empty() {
                format!("next actor {from} -> {to}")
            } else {
                format!("next actor {from} -> {to}: {detail}")
            }
        }
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

/// One "Resolved references" entry (empty for any other record).
pub fn resolved_line(record: &Record, full: bool) -> String {
    let Event::Resolved { reference, content } = &record.event else {
        return String::new();
    };
    match content {
        Resolution::Missing => format!("  {}:{} — missing", reference.kind, reference.id),
        Resolution::Withheld { id, size, .. } => {
            format!("  {}:{} — withheld ({size} bytes)", reference.kind, id)
        }
        Resolution::Found { data } => {
            if full {
                let text = String::from_utf8_lossy(&data.bytes);
                format!(
                    "  {}:{} — {} ({} bytes)\n{text}",
                    reference.kind,
                    reference.id,
                    data.mime,
                    data.bytes.len()
                )
            } else {
                format!(
                    "  {}:{} — {} ({} bytes)",
                    reference.kind,
                    reference.id,
                    data.mime,
                    data.bytes.len()
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(seq: u64, at: i64, event: Event) -> Record {
        Record { seq, at, actor: Actor::User, event }
    }

    #[test]
    fn window_limits_by_time_and_count() {
        let records = vec![
            rec(1, 1_000_000, Event::Milestone { state: "a".into() }),
            rec(2, 2_000_000, Event::Milestone { state: "b".into() }),
            rec(3, 3_000_000, Event::Milestone { state: "c".into() }),
        ];
        let all = render(&records, Window::default());
        assert_eq!(all.lines().count(), 3);
        let since = render(&records, Window { since_micros: Some(2_000_000), ..Default::default() });
        assert!(!since.contains("milestone: a") && since.contains("milestone: b"));
        let last = render(&records, Window { last: Some(1), ..Default::default() });
        assert!(last.contains("milestone: c") && !last.contains("milestone: b"));
    }
}
