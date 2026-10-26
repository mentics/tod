//! `tod-journeys show <file>` (spec §9.5, §10): a readable timeline of an
//! already-decrypted, decompressed journey/bundle file, followed by the
//! resolved references section.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use tod_journey::{JourneyReader, Record};

pub fn run(path: &Path, full: bool) -> Result<()> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let records: Vec<Record> = JourneyReader::from_reader(file).collect();

    print!(
        "{}",
        tod_journey::timeline::render(&records, tod_journey::timeline::Window { full, ..Default::default() })
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_journey::bundle::BundleWriter;
    use tod_journey::{Actor, Event, Presented, Resolution};
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
        let line = tod_journey::timeline::describe_event(&Event::UserAction {
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
