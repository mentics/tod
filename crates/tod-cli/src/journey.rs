//! `tod-cli journey` — read a recorded journey: a node's, or the user's recent
//! activity (the project journey). Read-only, and local: it reads the files
//! the app records under `<data root>/journeys`, never the relay.

use crate::Invocation;
use crate::args::Args;
use tod_journey::timeline::{self, Window};
use tod_journey::{JourneyKey, JourneyReader};
use tod_store::waits::parse_duration;

pub(crate) const USAGE: &str = "\
tod-cli journey — what happened on a node, or in the user's recent activity

<TARGET> is a node slug or UUID, or `project` for the user's own recent
activity across the app. A reference given as `journey:node/<SLUG>` or
`journey:project` names the same thing.

COMMANDS:
    show      <TARGET> [--since <DURATION>] [--last <N>] [--full]

`show` prints a timeline, one line per record: time, gap since the previous
record, seq, actor, event. `--since 2h` keeps the last two hours (`30s`, `5m`,
`2h`, `1d`), `--last 200` the last 200 records; use them, since a journey can
be long. `--full` prints the content of resolved references in full.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.is_empty() || rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    let command = rest.remove(0);
    let args = Args::parse(&rest)?;
    match command.as_str() {
        "show" => show(&inv, &args),
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}

/// The journey a `<TARGET>` (or `journey:` reference) names.
fn key(inv: &Invocation, raw: &str) -> anyhow::Result<JourneyKey> {
    let target = tod_core::journey::reference::target_of(raw);
    if target == "project" {
        return Ok(JourneyKey::Project);
    }
    Ok(JourneyKey::Node(crate::node::resolve(inv, target)?))
}

fn show(inv: &Invocation, args: &Args) -> anyhow::Result<String> {
    let raw = args
        .positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("`show` needs a target: a node slug or UUID, or `project`"))?;
    let key = key(inv, raw)?;
    let since_micros = args
        .get("--since")
        .map(|raw| {
            let secs = parse_duration(raw)?;
            let now = chrono::Utc::now().timestamp_micros();
            anyhow::Ok(now - secs.saturating_mul(1_000_000))
        })
        .transpose()?;
    let last = args
        .get("--last")
        .map(|raw| {
            raw.parse::<usize>()
                .map_err(|_| anyhow::anyhow!("--last takes a number of records, not `{raw}`"))
        })
        .transpose()?;
    let window = Window { since_micros, last, full: args.has("--full") };

    let reader = JourneyReader::open(&inv.data_root.join("journeys"), key)?;
    let records = reader.all();
    if records.is_empty() {
        anyhow::bail!(
            "no journey recorded for {} (journeys are recorded only while \
             \"Record journeys on this computer\" is on)",
            key.stem()
        );
    }
    if inv.json {
        let selected: Vec<_> = records
            .iter()
            .filter(|r| since_micros.is_none_or(|s| r.at >= s))
            .collect();
        let skip = last.map_or(0, |n| selected.len().saturating_sub(n));
        return Ok(serde_json::to_string(&selected[skip..])?);
    }
    let text = timeline::render(records, window);
    if text.trim().is_empty() {
        return Ok("(no records in that window)".to_string());
    }
    Ok(text.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_journey::{Actor, Event, JourneyWriter};

    fn invocation(root: &std::path::Path, rest: &[&str]) -> Invocation {
        Invocation {
            data_root: root.to_path_buf(),
            json: false,
            rest: rest.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn shows_the_project_journey_and_honours_last() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("journeys");
        std::fs::create_dir_all(&dir).unwrap();
        let mut writer = JourneyWriter::open(&dir, JourneyKey::Project).unwrap();
        for state in ["a", "b", "c"] {
            writer.append(Actor::User, Event::Milestone { state: state.into() });
        }
        drop(writer);

        let all = run(invocation(root.path(), &["show", "journey:project"])).unwrap();
        assert_eq!(all.lines().count(), 3, "{all}");
        let last = run(invocation(root.path(), &["show", "project", "--last", "1"])).unwrap();
        assert!(last.contains("milestone: c") && !last.contains("milestone: b"), "{last}");
        let none = run(invocation(root.path(), &["show", "project", "--since", "1s"]));
        // Recorded just now, so still inside a 1s window or an empty-window note.
        assert!(none.is_ok());
    }

    #[test]
    fn a_missing_journey_says_how_recording_is_turned_on() {
        let root = tempfile::tempdir().unwrap();
        let err = run(invocation(root.path(), &["show", "project"])).unwrap_err();
        assert!(err.to_string().contains("Record journeys"), "{err}");
    }
}
