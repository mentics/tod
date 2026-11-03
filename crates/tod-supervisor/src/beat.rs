//! Telling the app that this supervisor is working on the node: the
//! `cloud_activity` row (`tod_store::cloud_activity`), written while the
//! autopilot runs and pushed up, so the app can show the node as working
//! instead of guessing from what has or has not changed.
//!
//! [`Beating::start`] records the run and pushes it at once, then a thread
//! refreshes and pushes it every [`BEAT_EVERY`]. [`Beating::finish`] (or
//! dropping it) marks the row idle, with why the wake stopped when it did
//! (`cloud_activity.stopped`), and pushes that: the app sees the run end,
//! even one that began and ended between two of its syncs.
//!
//! The thread records each beat in the local copy on a connection of its own
//! ([`crate::replica::BeatWriter`]) and pushes only if it can take the
//! replica without waiting: a sync (push and pull, with long HTTP timeouts)
//! holds that lock, and a beat that waited behind it would let the row go
//! stale ([`tod_store::cloud_activity::STALE_AFTER_MS`]) while the supervisor
//! is fine. A skipped push is not lost: the sync that held the lock pushes
//! the copy's changes, the beat among them, and the next beat tries again.
//! A supervisor that dies leaves the row to go stale, which the app shows
//! as the loss it is.

use crate::progress::Progress;
use crate::replica::{BeatWriter, Replica};
use anyhow::{Context, Result};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::JoinHandle;
use std::time::Duration;
use uuid::Uuid;

/// How often the row is refreshed (`tod_store::cloud_activity::BEAT_EVERY_MS`).
pub const BEAT_EVERY: Duration = Duration::from_millis(tod_store::cloud_activity::BEAT_EVERY_MS as u64);

/// The row, held for as long as this lives.
pub struct Beating {
    replica: Arc<Mutex<Replica>>,
    node: Uuid,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Beating {
    /// Records that the supervisor is working on `node` and keeps saying so
    /// every `every`. Fails, without a thread, if the first beat cannot be
    /// recorded and sent: work that cannot prove it is running must not
    /// claim to be, and the caller reports why.
    pub fn start(replica: Arc<Mutex<Replica>>, node: Uuid, every: Duration) -> Result<Self> {
        Self::start_gated(replica, node, every, None)
    }

    /// Like [`Beating::start`], but a beat is recorded and sent only while
    /// `gate` (the progress marker and the age it may reach) is fresh: a
    /// supervisor that is wedged must not read as working. Its row goes
    /// stale and the app says it stopped responding.
    pub fn start_gated(
        replica: Arc<Mutex<Replica>>,
        node: Uuid,
        every: Duration,
        gate: Option<(Progress, Duration)>,
    ) -> Result<Self> {
        beat_once(&replica, node).context("telling the app this node is being worked on")?;
        let (stop, stopped) = mpsc::channel::<()>();
        let beating = replica.clone();
        let writer = replica.lock().unwrap_or_else(|e| e.into_inner()).beat_writer(node);
        let thread = std::thread::Builder::new()
            .name("cloud-heartbeat".into())
            .spawn(move || {
                // `recv_timeout` is the sleep: dropping the sender wakes it.
                let mut skipped = 0u32;
                while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
                    if stalled(&gate) {
                        tracing::warn!("cloud heartbeat: no progress; not claiming the node is being worked on");
                        continue;
                    }
                    match beat_without_waiting(&beating, &writer) {
                        Ok(true) => skipped = 0,
                        Ok(false) => {
                            skipped += 1;
                            // One skip is normal (a sync is under way); several
                            // mean the sync is slow, and the app will see the
                            // row stale until it ends.
                            if skipped >= 3 {
                                tracing::warn!(skipped, "cloud heartbeat: a sync has held the local copy for {skipped} beats; the row is recorded but not sent until it ends");
                            } else {
                                tracing::info!("cloud heartbeat: a sync holds the local copy; this beat is recorded and goes with its push");
                            }
                        }
                        Err(err) => {
                            // The row goes stale and the app says the supervisor
                            // stopped responding: the failure is not lost.
                            tracing::warn!("cloud heartbeat: {err:#}");
                        }
                    }
                }
            })
            .context("start the cloud heartbeat")?;
        Ok(Self { replica, node, stop: Some(stop), thread: Some(thread) })
    }

    /// The run is over: stops the beat, marks the row idle (with `stopped`,
    /// why the wake ended needing someone, when it did), and sends that.
    /// `state` is how the run ended as the node's run state.
    pub fn finish(mut self, stopped: Option<String>, state: &tod_store::run_state::Draft) -> Result<()> {
        self.end(stopped.as_deref(), state)
    }

    fn end(&mut self, stopped: Option<&str>, state: &tod_store::run_state::Draft) -> Result<()> {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("the cloud heartbeat thread panicked");
        }
        let mut replica = self.replica.lock().unwrap_or_else(|e| e.into_inner());
        replica.end_cloud_beat(self.node, stopped, state).context("telling the app this node is no longer being worked on")
    }
}

impl Drop for Beating {
    fn drop(&mut self) {
        if self.stop.is_some()
            && let Err(err) = self.end(Some(UNEXPECTED_END), &tod_core::run_state::runner_failed(UNEXPECTED_END))
        {
            // The row stays and goes stale: shown as lost, not hidden.
            tracing::warn!("cloud heartbeat: {err:#}");
        }
    }
}

/// The reason left when the run ends by being dropped, not finished: the
/// supervisor returned early with an error (its log has it).
const UNEXPECTED_END: &str = "the supervisor ended its run without finishing it; see the supervisor's log in the log panel, then run it again";

/// Whether the gate says the supervisor has made no progress for too long to
/// claim it is working.
fn stalled(gate: &Option<(Progress, Duration)>) -> bool {
    gate.as_ref().is_some_and(|(progress, limit)| !progress.fresh(*limit))
}

/// Records a beat and sends it unless a sync holds the replica. Whether it
/// was sent.
fn beat_without_waiting(replica: &Arc<Mutex<Replica>>, writer: &BeatWriter) -> Result<bool> {
    if !writer.record()? {
        anyhow::bail!("the node is not in the supervisor's copy of the database; the heartbeat cannot be recorded");
    }
    let mut replica = match replica.try_lock() {
        Ok(replica) => replica,
        Err(TryLockError::WouldBlock) => return Ok(false),
        Err(TryLockError::Poisoned(e)) => e.into_inner(),
    };
    replica.push().context("sending the heartbeat (and the copy's other changes) to the orchestrator")?;
    Ok(true)
}

fn beat_once(replica: &Arc<Mutex<Replica>>, node: Uuid) -> Result<()> {
    let mut replica = replica.lock().unwrap_or_else(|e| e.into_inner());
    replica.cloud_beat(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stalled_supervisor_does_not_beat_and_an_ungated_one_always_does() {
        assert!(!stalled(&None));
        let progress = Progress::in_memory();
        let gate = Some((progress.clone(), Duration::from_secs(60)));
        assert!(!stalled(&gate));
        progress.backdate(Duration::from_secs(61));
        assert!(stalled(&gate), "wedged: the beat is skipped, so the row goes stale");
        progress.mark();
        assert!(!stalled(&gate));
    }
}
