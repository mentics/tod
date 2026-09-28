//! Holding the sandbox awake while work runs with no client connected.
//!
//! The provider only counts connections through its proxy, and freezes every
//! process once none is left. Its local process API (`127.0.0.1:8080`, no auth
//! from inside) can start a process with `keepAlive`, which disables standby
//! while it runs: the relay runs one `sleep` that way while any reason to stay
//! awake exists, and kills it when the last one ends.
//!
//! A reason is either **lease-less** (`set`, the existing `busy:<session>` /
//! `awake:<session>` / `agent:<name>` reasons in `server.rs`): it holds
//! exactly as long as it is `set(reason, true)`, and its own liveness check
//! (the foreground-job poll, the agent's pending-request tracking) is what
//! keeps it valid, so the relay renews the underlying process for it on its
//! own, in a rolling window, as long as it is still set. Or it is **leased**
//! (`lease`, taken by an external caller such as the supervisor): it holds
//! until a deadline, and ends there unless renewed with a later one before
//! then — a caller that dies or hangs stops renewing, and the hold ends
//! within its lease, never longer. The `keepAlive` process's `timeout` is set
//! to the shortest lease still open (or `max_secs` when only lease-less
//! reasons are open), replacing the old fixed 4-hour cap.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

/// How much slack there is before a running `keepAlive` process is renewed:
/// small enough that a lease reliably ends close to its deadline, large
/// enough that the process is not restarted every tick.
const RENEW_MARGIN: Duration = Duration::from_secs(20);

/// Pure lease/hold bookkeeping: which reasons are open and when each ends.
/// No thread, clock, or process behind it, so it is covered directly by
/// tests with an explicit `now` in place of a real clock.
#[derive(Default)]
struct HoldState {
    /// reason -> lease deadline, or `None` for a lease-less hold.
    reasons: HashMap<String, Option<Instant>>,
    /// When the current stretch of holding began: set when the first reason
    /// opens, cleared when the last one ends. `GET /holds` reports it, so the
    /// watchdog can tell how long the sandbox has been kept awake.
    since: Option<Instant>,
}

/// One open reason, as `GET /holds` reports it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReasonReport {
    pub reason: String,
    /// Seconds left on its lease; `None` for a lease-less reason.
    pub lease_secs_left: Option<u64>,
}

/// What `GET /holds` returns. Times are relative (seconds before or after
/// the moment of the request), since the relay's clock is monotonic.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HoldsReport {
    /// How long ago the current stretch of holding began; `None` when
    /// nothing holds the sandbox.
    pub held_for_secs: Option<u64>,
    pub reasons: Vec<ReasonReport>,
}

impl HoldState {
    fn set(&mut self, reason: &str, on: bool) {
        if on {
            self.reasons.insert(reason.to_string(), None);
        } else {
            self.reasons.remove(reason);
        }
    }

    /// Sets or renews a leased hold, ending at `deadline` unless renewed
    /// again (with a later deadline) before then.
    fn lease(&mut self, reason: &str, deadline: Instant) {
        self.reasons.insert(reason.to_string(), Some(deadline));
    }

    /// Drops leases that have reached `now`. Lease-less reasons never
    /// expire this way. Returns whether anything was dropped.
    fn expire(&mut self, now: Instant) -> bool {
        let before = self.reasons.len();
        self.reasons.retain(|_, deadline| deadline.map(|d| d > now).unwrap_or(true));
        self.reasons.len() != before
    }

    fn is_empty(&self) -> bool {
        self.reasons.is_empty()
    }

    /// Starts or ends the holding stretch to match the reasons open now.
    fn touch(&mut self, now: Instant) {
        if self.reasons.is_empty() {
            self.since = None;
        } else if self.since.is_none() {
            self.since = Some(now);
        }
    }

    fn clear(&mut self) {
        self.reasons.clear();
        self.since = None;
    }

    fn report(&self, now: Instant) -> HoldsReport {
        let mut reasons: Vec<ReasonReport> = self
            .reasons
            .iter()
            .map(|(r, d)| ReasonReport {
                reason: r.clone(),
                lease_secs_left: d.map(|d| d.saturating_duration_since(now).as_secs()),
            })
            .collect();
        reasons.sort_by(|a, b| a.reason.cmp(&b.reason));
        HoldsReport { held_for_secs: self.since.map(|s| now.saturating_duration_since(s).as_secs()), reasons }
    }

    /// The `keepAlive` process's `timeout`, in seconds, for the reasons open
    /// right now: the shortest lease left, or `max_secs` when no lease is
    /// open (only lease-less reasons, or none open at all). A lease longer
    /// than `max_secs` is still capped by it.
    fn timeout_secs(&self, now: Instant, max_secs: u64) -> u64 {
        let shortest_lease =
            self.reasons.values().filter_map(|d| *d).map(|d| d.saturating_duration_since(now).as_secs()).min();
        shortest_lease.map(|s| s.min(max_secs)).unwrap_or(max_secs)
    }
}

pub struct Hold {
    state: Arc<Mutex<HoldState>>,
    worker: Sender<()>,
}

impl Hold {
    /// `max_secs` bounds a lease-less hold, and caps a lease longer than it:
    /// past that the provider ends the `sleep`, and the sandbox may sleep
    /// even if work is still running.
    pub fn new(max_secs: u64) -> Self {
        let state = Arc::new(Mutex::new(HoldState::default()));
        let (tx, rx) = channel::<()>();
        let worker_state = state.clone();
        std::thread::spawn(move || {
            let mut current: Option<(String, Instant)> = None;
            let mut n = 0u64;
            loop {
                // Wake on a `set`/`lease` call, or once a second regardless,
                // to notice an unrenewed lease passing its deadline or a
                // lease-less hold coming close to its rolling renewal.
                let _ = rx.recv_timeout(Duration::from_secs(1));
                while rx.try_recv().is_ok() {}
                reconcile(&worker_state, &mut current, &mut n, max_secs);
            }
        });
        Self { state, worker: tx }
    }

    /// Sets or clears a lease-less hold (the existing `busy:` / `awake:` /
    /// `agent:` reasons).
    pub fn set(&self, reason: &str, on: bool) {
        {
            let mut s = self.state.lock().unwrap();
            s.set(reason, on);
            s.touch(Instant::now());
        }
        let _ = self.worker.send(());
    }

    /// Sets or renews a leased hold for `secs` from now. Not calling this
    /// again before it elapses ends the hold.
    pub fn lease(&self, reason: &str, secs: u64) {
        let now = Instant::now();
        {
            let mut s = self.state.lock().unwrap();
            s.lease(reason, now + Duration::from_secs(secs));
            s.touch(now);
        }
        let _ = self.worker.send(());
    }

    /// Ends a hold (leased or not) now.
    pub fn release(&self, reason: &str) {
        self.set(reason, false);
    }

    /// Ends every hold now, leased or not (the watchdog's `POST
    /// /release-all`). A lease-less reason whose work is still running is
    /// set again the next time that work changes state.
    pub fn release_all(&self) {
        self.state.lock().unwrap().clear();
        let _ = self.worker.send(());
    }

    /// What is holding the sandbox, and since when (`GET /holds`).
    pub fn report(&self) -> HoldsReport {
        let now = Instant::now();
        let mut s = self.state.lock().unwrap();
        s.expire(now);
        s.touch(now);
        s.report(now)
    }
}

/// Brings the running `keepAlive` process (if any) in line with the current
/// reasons: starts one if none is running and a reason is open, kills it once
/// none is, and restarts it with a fresh timeout when it is close to ending
/// (within `RENEW_MARGIN`) but a reason still needs more time than that.
fn reconcile(state: &Mutex<HoldState>, current: &mut Option<(String, Instant)>, next_id: &mut u64, max_secs: u64) {
    let now = Instant::now();
    let (empty, desired_secs) = {
        let mut s = state.lock().unwrap();
        s.expire(now);
        s.touch(now);
        (s.is_empty(), s.timeout_secs(now, max_secs))
    };
    if empty {
        if let Some((name, _)) = current.take() {
            let _ = api("DELETE", &format!("/process/{name}/kill"), None);
            eprintln!("hold: released ({name})");
        }
        return;
    }
    let needs_restart = match current {
        None => true,
        Some((_, expiry)) => {
            let remaining = expiry.saturating_duration_since(now);
            remaining <= RENEW_MARGIN && Duration::from_secs(desired_secs) > remaining
        }
    };
    if !needs_restart {
        return;
    }
    if let Some((name, _)) = current.take() {
        let _ = api("DELETE", &format!("/process/{name}/kill"), None);
    }
    *next_id += 1;
    let name = format!("tod-relay-hold-{}-{next_id}", std::process::id());
    let body = hold_process_body(&name, desired_secs);
    match api("POST", "/process", Some(&body)) {
        Ok(_) => {
            eprintln!("hold: awake ({name}, timeout {desired_secs}s)");
            *current = Some((name, now + Duration::from_secs(desired_secs)));
        }
        Err(e) => eprintln!("hold: could not start: {e}"),
    }
}

/// The process API request for a hold's `keepAlive` process. The API runs
/// the command through `sh -c` and reaps only its own children, so `exec`
/// makes `sleep` that child: killing it at release leaves no `<defunct>`
/// `sleep` (orphaned to PID 1, which never reaps it) behind for every hold.
fn hold_process_body(name: &str, timeout_secs: u64) -> String {
    format!(r#"{{"command":"exec sleep 2147483","name":"{name}","keepAlive":true,"timeout":{timeout_secs}}}"#)
}

fn api(method: &str, path: &str, body: Option<&str>) -> std::io::Result<String> {
    let mut s = TcpStream::connect(("127.0.0.1", 8080))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let body = body.unwrap_or("");
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    if !out.starts_with("HTTP/1.1 2") && !out.starts_with("HTTP/1.0 2") {
        return Err(std::io::Error::other(out.lines().next().unwrap_or("no response").to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn hold_process_is_the_sleep_itself() {
        let body = hold_process_body("tod-relay-hold-1-1", 60);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["command"], "exec sleep 2147483");
        assert_eq!(v["name"], "tod-relay-hold-1-1");
        assert_eq!(v["keepAlive"], true);
        assert_eq!(v["timeout"], 60);
    }

    #[test]
    fn lease_less_hold_has_no_deadline_and_uses_max_secs() {
        let now = Instant::now();
        let mut s = HoldState::default();
        s.set("busy:1", true);
        assert!(!s.is_empty());
        assert_eq!(s.timeout_secs(now, 3600), 3600);
        // Never expires on its own.
        assert!(!s.expire(now + secs(10_000)));
        assert!(!s.is_empty());
        s.set("busy:1", false);
        assert!(s.is_empty());
    }

    #[test]
    fn lease_ends_when_not_renewed() {
        let now = Instant::now();
        let mut s = HoldState::default();
        s.lease("supervisor", now + secs(600));
        assert_eq!(s.timeout_secs(now, 3600), 600);
        // Not yet due.
        assert!(!s.expire(now + secs(599)));
        assert!(!s.is_empty());
        // Past the deadline, unrenewed: gone.
        assert!(s.expire(now + secs(601)));
        assert!(s.is_empty());
    }

    #[test]
    fn renewing_a_lease_extends_it() {
        let now = Instant::now();
        let mut s = HoldState::default();
        s.lease("supervisor", now + secs(60));
        s.lease("supervisor", now + secs(30) + secs(600)); // renewed before it ran out
        assert!(!s.expire(now + secs(60)));
        assert!(!s.is_empty());
        assert_eq!(s.timeout_secs(now + secs(60), 3600), 630 - 60);
    }

    #[test]
    fn timeout_is_the_shortest_open_lease() {
        let now = Instant::now();
        let mut s = HoldState::default();
        s.lease("a", now + secs(600));
        s.lease("b", now + secs(120));
        assert_eq!(s.timeout_secs(now, 3600), 120);
    }

    #[test]
    fn a_lease_longer_than_max_secs_is_capped() {
        let now = Instant::now();
        let mut s = HoldState::default();
        s.lease("supervisor", now + secs(100_000));
        assert_eq!(s.timeout_secs(now, 3600), 3600);
    }

    #[test]
    fn mixing_lease_and_lease_less_takes_the_shorter() {
        let now = Instant::now();
        let mut s = HoldState::default();
        s.set("busy:1", true);
        s.lease("supervisor", now + secs(30));
        assert_eq!(s.timeout_secs(now, 3600), 30);
        s.set("busy:1", false);
        s.set("busy:1", true);
        assert_eq!(s.timeout_secs(now, 3600), 30);
    }

    #[test]
    fn report_tracks_the_holding_stretch() {
        let now = Instant::now();
        let mut s = HoldState::default();
        assert_eq!(s.report(now).held_for_secs, None);
        s.set("busy:1", true);
        s.touch(now);
        s.lease("ext:supervisor", now + secs(120));
        s.touch(now + secs(10));
        let r = s.report(now + secs(30));
        assert_eq!(r.held_for_secs, Some(30));
        assert_eq!(
            r.reasons,
            vec![
                ReasonReport { reason: "busy:1".into(), lease_secs_left: None },
                ReasonReport { reason: "ext:supervisor".into(), lease_secs_left: Some(90) },
            ]
        );
        s.set("busy:1", false);
        s.expire(now + secs(200));
        s.touch(now + secs(200));
        assert_eq!(s.report(now + secs(200)).held_for_secs, None);
        s.set("a", true);
        s.touch(now + secs(300));
        assert_eq!(s.report(now + secs(301)).held_for_secs, Some(1));
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.report(now + secs(302)).held_for_secs, None);
    }

    #[test]
    fn removing_the_last_reason_empties_state() {
        let now = Instant::now();
        let mut s = HoldState::default();
        s.set("a", true);
        s.lease("b", now + secs(10));
        s.set("a", false);
        assert!(!s.is_empty());
        assert!(s.expire(now + secs(11)));
        assert!(s.is_empty());
    }
}
