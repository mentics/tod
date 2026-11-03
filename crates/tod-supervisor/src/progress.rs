//! The progress marker: proof that the supervisor is getting somewhere.
//!
//! A supervisor that is wedged (a hung call, a lock never released) keeps its
//! threads, so a hold renewed by a timer and a heartbeat sent by a timer
//! would say "working" for as long as the process lives: hours. They are
//! gated on this instead. [`Progress::mark`] is called wherever work really
//! moved (a startup phase done, a boundary of the autopilot, the agent's
//! reply changing); the hold's renewal ([`crate::hold`]) and the heartbeat
//! ([`crate::beat`]) go on only while the last mark is younger than a limit,
//! so a stalled supervisor's lease lapses and its row goes stale within
//! minutes, as for a dead one.
//!
//! The mark is also a file, `<state_dir>/progress`, whose modification time
//! is the mark: the relay reads it to kill a supervisor it started that has
//! stopped making progress (`tod-relay`'s `liveness`). The file name is part
//! of that contract.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The marker file's name in the state directory (`tod-relay` reads it too).
pub const PROGRESS_FILE: &str = "progress";

/// The least time between two writes of the file.
const WRITE_EVERY: Duration = Duration::from_secs(1);

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(PROGRESS_FILE)
}

#[derive(Clone)]
pub struct Progress {
    inner: Arc<Inner>,
}

struct Inner {
    file: Option<PathBuf>,
    state: Mutex<State>,
}

struct State {
    last: Instant,
    written: Option<Instant>,
    write_failed: bool,
}

impl Progress {
    /// A marker kept in `file` as well as in memory, marked now.
    pub fn new(file: PathBuf) -> Self {
        let progress = Self::build(Some(file));
        progress.mark();
        progress
    }

    /// A marker with no file (tests, callers with nothing to gate), marked now.
    pub fn in_memory() -> Self {
        Self::build(None)
    }

    fn build(file: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                file,
                state: Mutex::new(State { last: Instant::now(), written: None, write_failed: false }),
            }),
        }
    }

    /// Work moved. A file that cannot be written is a warning (once per
    /// streak): the relay will see the marker stale and say why it killed.
    pub fn mark(&self) {
        self.mark_at(Instant::now());
    }

    fn mark_at(&self, now: Instant) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.last = now;
        let Some(file) = &self.inner.file else { return };
        if state.written.is_some_and(|w| now.saturating_duration_since(w) < WRITE_EVERY) {
            return;
        }
        let stamp =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        match std::fs::write(file, stamp.to_string()) {
            Ok(()) => {
                state.written = Some(now);
                state.write_failed = false;
            }
            Err(err) => {
                if !state.write_failed {
                    tracing::warn!(
                        "cannot write the progress marker {}: {err}; the relay will take this supervisor for stalled",
                        file.display()
                    );
                }
                state.write_failed = true;
            }
        }
    }

    /// How long since the last mark.
    pub fn age(&self) -> Duration {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).last.elapsed()
    }

    /// Whether the last mark is younger than `limit`.
    pub fn fresh(&self, limit: Duration) -> bool {
        self.age() < limit
    }

    #[cfg(test)]
    pub(crate) fn backdate(&self, by: Duration) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.last -= by;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_until_the_limit_then_stale_until_marked_again() {
        let p = Progress::in_memory();
        assert!(p.fresh(Duration::from_secs(60)));
        p.backdate(Duration::from_secs(120));
        assert!(!p.fresh(Duration::from_secs(60)));
        p.mark();
        assert!(p.fresh(Duration::from_secs(60)));
    }

    #[test]
    fn the_mark_is_a_file_whose_modification_time_is_the_mark() {
        let dir = crate::lock::tests::tempfile_dir("progress");
        let file = path(&dir);
        let p = Progress::new(file.clone());
        let first = std::fs::metadata(&file).unwrap().modified().unwrap();
        // Within WRITE_EVERY: no rewrite.
        p.mark();
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), first);
        p.mark_at(Instant::now() + WRITE_EVERY + Duration::from_millis(10));
        assert!(std::fs::metadata(&file).unwrap().modified().unwrap() >= first);
    }
}
