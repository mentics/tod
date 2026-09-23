//! The journey writer thread and the process-wide [`Recorder`] handle.
//!
//! Mirrors the process-global pattern in `tod_core::logging`: a `OnceLock`
//! holds the installed handle, and `record` is a silent no-op until
//! `install` has run (so call sites never need to check whether a recorder
//! exists).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tod_journey::{Actor, Event, JourneyKey, JourneyWriter};

/// A writer idle this long has its file handle closed; it reopens (cheaply,
/// recovering any tail) the next time something is recorded for it.
const IDLE_CLOSE: Duration = Duration::from_secs(10 * 60);
/// How often the writer thread checks for idle writers when nothing else
/// woke it.
const IDLE_CHECK: Duration = Duration::from_secs(30);

enum Msg {
    Append {
        key: JourneyKey,
        actor: Actor,
        event: Event,
    },
    Compact {
        key: JourneyKey,
    },
}

/// Cheaply cloneable handle to the journey writer thread.
#[derive(Clone)]
pub struct Recorder {
    tx: mpsc::Sender<Msg>,
}

static RECORDER: OnceLock<Recorder> = OnceLock::new();

/// Installs `recorder` as the process-wide handle used by [`record`]. Only
/// the first call takes effect (matches `tod_core::logging`'s init-once
/// pattern); later calls are ignored rather than erroring, since a second
/// call would only happen if the app tried to start the recorder twice.
pub fn install(recorder: Recorder) {
    let _ = RECORDER.set(recorder);
}

/// Records one event to `key`'s journey. A silent no-op when no recorder has
/// been installed (tests, `tod-cli`, or any process that never calls
/// [`crate::journey::start`]).
pub fn record(key: JourneyKey, actor: Actor, event: Event) {
    if let Some(recorder) = RECORDER.get() {
        recorder.record(key, actor, event);
    }
}

impl Recorder {
    /// Same as the free function [`record`], for callers that already hold
    /// a handle (e.g. the change-feed thread).
    pub fn record(&self, key: JourneyKey, actor: Actor, event: Event) {
        let _ = self.tx.send(Msg::Append { key, actor, event });
    }

    /// Asks the writer thread to compact `key`'s journey (tail -> `.zst`)
    /// and enforce the storage cap. Fire-and-forget: the caller does not
    /// wait for it to happen.
    pub fn compact(&self, key: JourneyKey) {
        let _ = self.tx.send(Msg::Compact { key });
    }
}

/// Starts the writer thread and returns a handle to it. Compacts every
/// journey with a non-empty tail at startup (spec: a journey should not sit
/// indefinitely with an uncompacted tail from a previous run).
pub fn spawn(journeys_dir: PathBuf, storage_cap_mb: u64) -> Recorder {
    let (tx, rx) = mpsc::channel::<Msg>();
    let recorder = Recorder { tx };

    let cap_bytes = storage_cap_mb.saturating_mul(1024 * 1024);
    std::thread::Builder::new()
        .name("tod-journey-writer".into())
        .spawn(move || {
            let mut writers: HashMap<String, (JourneyWriter, Instant)> = HashMap::new();
            compact_existing_journeys_at_startup(&journeys_dir, &mut writers, cap_bytes);

            loop {
                match rx.recv_timeout(IDLE_CHECK) {
                    Ok(Msg::Append { key, actor, event }) => {
                        if let Some((writer, last_used)) = open_writer(&journeys_dir, &mut writers, key) {
                            writer.append(actor, event);
                            *last_used = Instant::now();
                        }
                    }
                    Ok(Msg::Compact { key }) => {
                        if let Some((writer, last_used)) = open_writer(&journeys_dir, &mut writers, key) {
                            if let Err(err) = writer.compact() {
                                tracing::warn!("journey: compaction failed for {}: {err}", key.stem());
                            }
                            *last_used = Instant::now();
                        }
                        if let Err(err) =
                            tod_journey::retention::enforce_cap(&journeys_dir, cap_bytes, key)
                        {
                            tracing::warn!("journey: retention enforcement failed: {err}");
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        writers.retain(|_, (_, last_used)| last_used.elapsed() < IDLE_CLOSE);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .expect("failed to spawn tod-journey writer thread");

    recorder
}

fn open_writer<'a>(
    journeys_dir: &std::path::Path,
    writers: &'a mut HashMap<String, (JourneyWriter, Instant)>,
    key: JourneyKey,
) -> Option<&'a mut (JourneyWriter, Instant)> {
    let stem = key.stem();
    if !writers.contains_key(&stem) {
        match JourneyWriter::open(journeys_dir, key) {
            Ok(writer) => {
                writers.insert(stem.clone(), (writer, Instant::now()));
            }
            Err(err) => {
                tracing::warn!("journey: failed to open journey for {stem}: {err}");
                return None;
            }
        }
    }
    writers.get_mut(&stem)
}

fn compact_existing_journeys_at_startup(
    journeys_dir: &std::path::Path,
    writers: &mut HashMap<String, (JourneyWriter, Instant)>,
    cap_bytes: u64,
) {
    let Ok(entries) = std::fs::read_dir(journeys_dir) else {
        return;
    };
    // Every journey key present is named by a `<stem>.journey.tail` file
    // that is non-empty.
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".journey.tail") else {
            continue;
        };
        let non_empty = std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false);
        if !non_empty {
            continue;
        }
        let key = if stem == "project" {
            JourneyKey::Project
        } else {
            match uuid::Uuid::parse_str(stem) {
                Ok(id) => JourneyKey::Node(id),
                Err(_) => continue,
            }
        };
        if let Some((writer, _)) = open_writer(journeys_dir, writers, key) {
            let _ = writer.compact();
        }
    }
    if let Err(err) = tod_journey::retention::enforce_cap(journeys_dir, cap_bytes, JourneyKey::Project)
    {
        tracing::warn!("journey: startup retention enforcement failed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_with_no_recorder_installed_is_a_noop() {
        // No `install` call in this test process (or, if another test in
        // this binary already installed one, this still must not panic or
        // block).
        record(
            JourneyKey::Project,
            Actor::App,
            Event::SettingsChanged {
                key: "x".into(),
                value: "y".into(),
            },
        );
    }
}
