//! Holding the sandbox awake while the supervisor works: a lease on the
//! relay's hold (`POST /hold`, `POST /release` on its loopback port; see
//! `doc/cloud-sandboxes/relay-protocol.md`), renewed by a thread well before
//! it lapses. A supervisor that dies or hangs stops renewing, and the sandbox
//! can sleep once the lease runs out.

use crate::progress::Progress;
use anyhow::{Result, bail};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

/// The lease the supervisor takes, and how often it renews it.
pub const LEASE_SECS: u64 = 120;
pub const RENEW_EVERY: Duration = Duration::from_secs(40);
/// The relay's hold reason for the supervisor.
pub const REASON: &str = "supervisor";

/// Where holds are taken.
pub trait Holder: Send + Sync + 'static {
    fn hold(&self, reason: &str, secs: u64) -> Result<()>;
    fn release(&self, reason: &str) -> Result<()>;
}

/// The relay on this sandbox, e.g. `http://127.0.0.1:2222`.
pub struct RelayHolder {
    base: String,
    agent: ureq::Agent,
}

impl RelayHolder {
    pub fn new(base: impl Into<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            // Loopback: never through the sandbox's proxy.
            .proxy(None)
            .build()
            .into();
        Self { base: base.into().trim_end_matches('/').to_string(), agent }
    }

    fn post(&self, path: &str) -> Result<()> {
        let url = format!("{}{path}", self.base);
        let resp = self.agent.post(&url).send_empty()?;
        if resp.status() != 200 {
            bail!("POST {url}: {}", resp.status());
        }
        Ok(())
    }
}

impl Holder for RelayHolder {
    fn hold(&self, reason: &str, secs: u64) -> Result<()> {
        self.post(&format!("/hold?reason={reason}&secs={secs}"))
    }

    fn release(&self, reason: &str) -> Result<()> {
        self.post(&format!("/release?reason={reason}"))
    }
}

/// Holds while alive: takes the lease, renews it every [`RENEW_EVERY`], and
/// releases it when dropped.
pub struct HoldGuard {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl HoldGuard {
    pub fn take(holder: std::sync::Arc<dyn Holder>) -> Self {
        Self::take_with(holder, LEASE_SECS, RENEW_EVERY)
    }

    pub fn take_with(holder: std::sync::Arc<dyn Holder>, lease_secs: u64, renew: Duration) -> Self {
        Self::take_gated(holder, lease_secs, renew, None)
    }

    /// Like [`HoldGuard::take_with`], but renewed only while `gate` (the
    /// progress marker and the age it may reach) is fresh: a wedged
    /// supervisor stops holding the sandbox awake within one lease.
    pub fn take_gated(
        holder: std::sync::Arc<dyn Holder>,
        lease_secs: u64,
        renew: Duration,
        gate: Option<(Progress, Duration)>,
    ) -> Self {
        if let Err(err) = holder.hold(REASON, lease_secs) {
            tracing::warn!("could not take the hold: {err:#}");
        }
        let (tx, rx) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("tod-supervisor-hold".into())
            .spawn(move || {
                loop {
                    match rx.recv_timeout(renew) {
                        Err(RecvTimeoutError::Timeout) => {
                            if let Some((progress, limit)) = &gate
                                && !progress.fresh(*limit)
                            {
                                // Let the lease lapse: nothing has moved for
                                // `limit`, and a supervisor that cannot say it is
                                // working does not keep the sandbox awake.
                                tracing::warn!(
                                    stalled_secs = progress.age().as_secs(),
                                    "no progress; not renewing the hold, it lapses with its lease"
                                );
                                continue;
                            }
                            if let Err(err) = holder.hold(REASON, lease_secs) {
                                tracing::warn!("could not renew the hold: {err:#}");
                            }
                        }
                        _ => break,
                    }
                }
                if let Err(err) = holder.release(REASON) {
                    tracing::warn!("could not release the hold: {err:#}");
                }
            })
            .ok();
        Self { stop: Some(tx), thread }
    }
}

impl Drop for HoldGuard {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl Holder for Recorder {
        fn hold(&self, reason: &str, secs: u64) -> Result<()> {
            self.0.lock().unwrap().push(format!("hold {reason} {secs}"));
            Ok(())
        }
        fn release(&self, reason: &str) -> Result<()> {
            self.0.lock().unwrap().push(format!("release {reason}"));
            Ok(())
        }
    }

    #[test]
    fn holds_renews_and_releases() {
        let rec = Arc::new(Recorder::default());
        let guard = HoldGuard::take_with(rec.clone(), 3, Duration::from_millis(20));
        std::thread::sleep(Duration::from_millis(90));
        drop(guard);
        let calls = rec.0.lock().unwrap().clone();
        assert_eq!(calls.first().unwrap(), "hold supervisor 3");
        assert!(calls.iter().filter(|c| c.starts_with("hold")).count() >= 2, "{calls:?}");
        assert_eq!(calls.last().unwrap(), "release supervisor");
    }

    /// A wedged supervisor (no progress mark) must not keep renewing: the
    /// lease lapses, and renewal resumes when it moves again.
    #[test]
    fn a_stalled_supervisor_stops_renewing_its_hold() {
        let rec = Arc::new(Recorder::default());
        let progress = Progress::in_memory();
        progress.backdate(Duration::from_secs(600));
        let guard = HoldGuard::take_gated(
            rec.clone(),
            3,
            Duration::from_millis(20),
            Some((progress.clone(), Duration::from_secs(60))),
        );
        std::thread::sleep(Duration::from_millis(120));
        let holds = |rec: &Recorder| rec.0.lock().unwrap().iter().filter(|c| c.starts_with("hold")).count();
        assert_eq!(holds(&rec), 1, "only the first take, no renewals while stalled");
        progress.mark();
        std::thread::sleep(Duration::from_millis(120));
        assert!(holds(&rec) >= 2, "renews again once it progresses");
        drop(guard);
    }
}
