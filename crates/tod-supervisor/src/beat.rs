//! Telling the app that this supervisor is working on the node: the
//! `cloud_activity` row (`tod_store::cloud_activity`), written while the
//! autopilot runs and pushed up, so the app can show the node as working
//! instead of guessing from what has or has not changed.
//!
//! [`Beating::start`] records the run and pushes it at once, then a thread
//! refreshes and pushes it every [`BEAT_EVERY`]. [`Beating::finish`] (or
//! dropping it) deletes the row and pushes that: the app sees the run end.
//! A supervisor that dies leaves the row to go stale, which the app shows
//! as the loss it is.

use crate::replica::Replica;
use anyhow::{Context, Result};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
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
        beat_once(&replica, node).context("telling the app this node is being worked on")?;
        let (stop, stopped) = mpsc::channel::<()>();
        let beating = replica.clone();
        let thread = std::thread::Builder::new()
            .name("cloud-heartbeat".into())
            .spawn(move || {
                // `recv_timeout` is the sleep: dropping the sender wakes it.
                while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
                    if let Err(err) = beat_once(&beating, node) {
                        // The row goes stale and the app says the supervisor
                        // stopped responding: the failure is not lost.
                        tracing::warn!("cloud heartbeat: {err:#}");
                    }
                }
            })
            .context("start the cloud heartbeat")?;
        Ok(Self { replica, node, stop: Some(stop), thread: Some(thread) })
    }

    /// The run is over: stops the beat, deletes the row, and sends that.
    pub fn finish(mut self) -> Result<()> {
        self.end()
    }

    fn end(&mut self) -> Result<()> {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("the cloud heartbeat thread panicked");
        }
        let mut replica = self.replica.lock().unwrap_or_else(|e| e.into_inner());
        replica.end_cloud_beat(self.node).context("telling the app this node is no longer being worked on")
    }
}

impl Drop for Beating {
    fn drop(&mut self) {
        if self.stop.is_some()
            && let Err(err) = self.end()
        {
            // The row stays and goes stale: shown as lost, not hidden.
            tracing::warn!("cloud heartbeat: {err:#}");
        }
    }
}

fn beat_once(replica: &Arc<Mutex<Replica>>, node: Uuid) -> Result<()> {
    let mut replica = replica.lock().unwrap_or_else(|e| e.into_inner());
    replica.cloud_beat(node)
}
