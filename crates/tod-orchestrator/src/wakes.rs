//! The development account's wake timer (`tod_core::scheduler::OrchestratorScheduler`
//! is its client; design: `doc/cloud-sandboxes/autonomous-nodes.md`, "The
//! supervisor and waiting" and "Development account").
//!
//! - `POST /wakes` `{id, user, node, sandbox, at}` (`at` in ms since the
//!   epoch) records or replaces a wake; `DELETE /wakes/<id>` drops one (an
//!   unknown id is fine: it may already have fired). Both need `X-Tod-User`,
//!   and a wake belongs to that user.
//! - Wakes are rows in `<base>/wakes.json` (the orchestrator's own state,
//!   not any user's database), rewritten on every change and reloaded on
//!   start, so a restart loses none.
//! - When one is due, the timer pokes the node's sandbox relay
//!   (`POST <sandbox url>/port/2222/poke`, `doc/cloud-sandboxes/relay-protocol.md`)
//!   and drops the row. A failed poke is retried a minute later. A wake is
//!   only a poke, so a late or duplicate one is harmless.
//! - While any wake is pending the orchestrator holds its own sandbox awake
//!   ([`KeepAwake`]); [`LocalRelayAwake`] does it by poking its own relay
//!   (`127.0.0.1:2222/poke`) every 45 s, each poke taking the relay's 60 s
//!   leased hold. The relay has no lease call of its own yet, so this is the
//!   way available; a side effect is that each such poke also runs the
//!   relay's `--supervisor-cmd`, which on the orchestrator's sandbox should
//!   be a no-op (e.g. `--supervisor-cmd true`).
//!
//! Poking needs Blaxel credentials to reach other sandboxes: [`RelayPoker`]
//! reads `TOD_ORCHESTRATOR_BLAXEL_WORKSPACE` and `TOD_ORCHESTRATOR_BLAXEL_TOKEN`.
//! Without them a poke fails (and is retried) with a message saying so.

use crate::http::{Request, Response};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub const FILE_NAME: &str = "wakes.json";
/// A failed poke is tried again this much later.
pub const RETRY_MS: i64 = 60_000;
/// The timer looks again at least this often, whatever it is waiting for.
const MAX_SLEEP: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wake {
    pub id: String,
    pub user: String,
    pub node: String,
    pub sandbox: String,
    /// When, in ms since the epoch.
    pub at: i64,
}

/// Wakes a sandbox.
pub trait Poker: Send + Sync {
    fn poke(&self, wake: &Wake) -> Result<()>;
}

/// Keeps the orchestrator's own sandbox up while wakes are pending.
pub trait KeepAwake: Send + Sync {
    /// Called by the timer on every pass: `pending` is whether any wake is
    /// still waiting. Implementations renew their hold, or let it lapse.
    fn tick(&self, pending: bool);
}

pub struct NoKeepAwake;
impl KeepAwake for NoKeepAwake {
    fn tick(&self, _pending: bool) {}
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub struct Wakes {
    path: PathBuf,
    rows: Mutex<Vec<Wake>>,
    changed: Condvar,
    poker: Box<dyn Poker>,
}

impl Wakes {
    /// Loads `<base>/wakes.json` (missing is empty).
    pub fn load(base: &std::path::Path, poker: Box<dyn Poker>) -> Result<Arc<Self>> {
        let path = base.join(FILE_NAME);
        let rows = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        Ok(Arc::new(Self { path, rows: Mutex::new(rows), changed: Condvar::new(), poker }))
    }

    pub fn list(&self) -> Vec<Wake> {
        self.rows.lock().unwrap().clone()
    }

    fn save(&self, rows: &[Wake]) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(rows)?).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).with_context(|| format!("write {}", self.path.display()))
    }

    /// Records or replaces (by id) a wake.
    pub fn put(&self, wake: Wake) -> Result<()> {
        let mut rows = self.rows.lock().unwrap();
        rows.retain(|w| w.id != wake.id);
        rows.push(wake);
        self.save(&rows)?;
        self.changed.notify_all();
        Ok(())
    }

    /// Drops `id` if `user` owns it. Returns whether one was dropped.
    pub fn remove(&self, user: &str, id: &str) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        let before = rows.len();
        rows.retain(|w| !(w.id == id && w.user == user));
        let removed = rows.len() != before;
        if removed {
            self.save(&rows)?;
            self.changed.notify_all();
        }
        Ok(removed)
    }

    /// Pokes every wake due at `now` and drops it; a failed poke is moved
    /// [`RETRY_MS`] later. Pokes run without the lock held. Returns the ids poked.
    pub fn fire_due(&self, now: i64) -> Vec<String> {
        let due: Vec<Wake> = self.rows.lock().unwrap().iter().filter(|w| w.at <= now).cloned().collect();
        let mut fired = Vec::new();
        for wake in due {
            match self.poker.poke(&wake) {
                Ok(()) => {
                    let mut rows = self.rows.lock().unwrap();
                    // Only if it was not replaced by a later time meanwhile.
                    rows.retain(|w| w != &wake);
                    let _ = self.save(&rows).map_err(|e| eprintln!("tod-orchestrator: wakes: {e:#}"));
                    fired.push(wake.id);
                }
                Err(err) => {
                    eprintln!("tod-orchestrator: poke {} for wake {}: {err:#}", wake.sandbox, wake.id);
                    let mut rows = self.rows.lock().unwrap();
                    if let Some(row) = rows.iter_mut().find(|w| **w == wake) {
                        row.at = now + RETRY_MS;
                    }
                    let _ = self.save(&rows).map_err(|e| eprintln!("tod-orchestrator: wakes: {e:#}"));
                }
            }
        }
        fired
    }

    /// Runs the timer forever: fires due wakes, ticks `awake`, and sleeps
    /// until the next wake, a change, or [`MAX_SLEEP`].
    pub fn run_timer(self: &Arc<Self>, clock: &dyn Fn() -> i64, awake: &dyn KeepAwake) {
        loop {
            self.fire_due(clock());
            let rows = self.rows.lock().unwrap();
            awake.tick(!rows.is_empty());
            let next = rows.iter().map(|w| w.at).min();
            let sleep = match next {
                Some(at) => Duration::from_millis((at - clock()).max(0) as u64).min(MAX_SLEEP),
                None => MAX_SLEEP,
            };
            // A pending wake keeps the tick at most 45 s apart for `awake`.
            let sleep = if next.is_some() { sleep.min(Duration::from_secs(45)) } else { sleep };
            let _ = self.changed.wait_timeout(rows, sleep).unwrap();
        }
    }

    /// Starts [`Wakes::run_timer`] on its own thread with the real clock.
    pub fn spawn_timer(self: &Arc<Self>, awake: Box<dyn KeepAwake>) -> Result<()> {
        let wakes = self.clone();
        std::thread::Builder::new()
            .name("tod-orchestrator-wakes".into())
            .spawn(move || wakes.run_timer(&now_ms, awake.as_ref()))
            .context("spawn the wake timer")?;
        Ok(())
    }

    /// `POST /wakes` and `DELETE /wakes/<id>`, for `user` (already checked).
    pub fn handle(&self, request: &Request, user: &str, id: Option<&str>) -> Response {
        match (request.method.as_str(), id) {
            ("POST", None) => {
                let wake: Wake = match serde_json::from_slice(&request.body) {
                    Ok(w) => w,
                    Err(err) => return Response::text(400, format!("bad wake: {err}")),
                };
                if wake.user != user {
                    return Response::text(400, format!("the wake's user {:?} is not {user:?}", wake.user));
                }
                if wake.id.is_empty() || wake.sandbox.is_empty() {
                    return Response::text(400, "a wake needs an id and a sandbox");
                }
                match self.put(wake) {
                    Ok(()) => Response::text(200, "ok"),
                    Err(err) => Response::text(500, format!("{err:#}")),
                }
            }
            ("DELETE", Some(id)) => match self.remove(user, id) {
                Ok(_) => Response::text(200, "ok"),
                Err(err) => Response::text(500, format!("{err:#}")),
            },
            _ => Response::text(405, "POST /wakes or DELETE /wakes/<id>"),
        }
    }
}

/// Pokes a sandbox's relay through Blaxel's port proxy.
pub struct RelayPoker {
    blaxel: Option<tod_sandbox::blaxel::Blaxel>,
}

impl RelayPoker {
    pub fn from_env() -> Self {
        let blaxel = match (
            std::env::var("TOD_ORCHESTRATOR_BLAXEL_WORKSPACE"),
            std::env::var("TOD_ORCHESTRATOR_BLAXEL_TOKEN"),
        ) {
            (Ok(ws), Ok(token)) if !ws.is_empty() && !token.is_empty() => {
                Some(tod_sandbox::blaxel::Blaxel::new(ws, token))
            }
            _ => None,
        };
        Self { blaxel }
    }
}

impl Poker for RelayPoker {
    fn poke(&self, wake: &Wake) -> Result<()> {
        let Some(bx) = &self.blaxel else {
            bail!("TOD_ORCHESTRATOR_BLAXEL_WORKSPACE and TOD_ORCHESTRATOR_BLAXEL_TOKEN are not set");
        };
        let info = bx.get(&wake.sandbox)?.with_context(|| format!("no sandbox named {}", wake.sandbox))?;
        let url = info.url.with_context(|| format!("{} has no URL", wake.sandbox))?;
        let url = format!("{}/port/{}/poke", url.trim_end_matches('/'), tod_sandbox::blaxel::RELAY_PORT);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        let resp = agent
            .post(&url)
            .header("Authorization", &format!("Bearer {}", bx.token()))
            .send_empty()
            .context("poke")?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            bail!("poke {url}: {status}");
        }
        Ok(())
    }
}

/// Holds the orchestrator's own sandbox awake by poking its own relay.
pub struct LocalRelayAwake {
    pub addr: String,
}

impl Default for LocalRelayAwake {
    fn default() -> Self {
        Self { addr: format!("127.0.0.1:{}", tod_sandbox::blaxel::RELAY_PORT) }
    }
}

impl KeepAwake for LocalRelayAwake {
    fn tick(&self, pending: bool) {
        if !pending {
            return; // the last poke's 60 s lease lapses on its own
        }
        let result = std::net::TcpStream::connect(&self.addr).and_then(|mut s| {
            s.set_write_timeout(Some(Duration::from_secs(5)))?;
            write!(s, "POST /poke HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        });
        if let Err(err) = result {
            eprintln!("tod-orchestrator: cannot hold awake through {}: {err}", self.addr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct FakePoker {
        poked: Mutex<Vec<String>>,
        fail: AtomicBool,
    }
    impl Poker for Arc<FakePoker> {
        fn poke(&self, wake: &Wake) -> Result<()> {
            if self.fail.load(Ordering::SeqCst) {
                bail!("down");
            }
            self.poked.lock().unwrap().push(wake.sandbox.clone());
            Ok(())
        }
    }

    fn wake(id: &str, at: i64) -> Wake {
        Wake { id: id.into(), user: "alice".into(), node: "n".into(), sandbox: format!("sb-{id}"), at }
    }

    fn base() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tod-orch-wakes-{}", now_ms() ^ std::process::id() as i64));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fires_due_wakes_retries_failures_and_reloads() {
        let dir = base();
        let poker = Arc::new(FakePoker::default());
        let wakes = Wakes::load(&dir, Box::new(poker.clone())).unwrap();
        wakes.put(wake("a", 100)).unwrap();
        wakes.put(wake("b", 500)).unwrap();
        wakes.put(wake("a", 200)).unwrap(); // replaces
        assert_eq!(wakes.list().len(), 2);

        assert!(wakes.fire_due(150).is_empty());
        assert_eq!(wakes.fire_due(200), ["a"]);
        assert_eq!(*poker.poked.lock().unwrap(), ["sb-a"]);

        poker.fail.store(true, Ordering::SeqCst);
        assert!(wakes.fire_due(600).is_empty());
        assert_eq!(wakes.list()[0].at, 600 + RETRY_MS, "a failed poke is retried later");

        // Reloaded from disk as a restart would.
        let again = Wakes::load(&dir, Box::new(poker.clone())).unwrap();
        assert_eq!(again.list(), wakes.list());
        assert!(!again.remove("bob", "b").unwrap(), "another user's wake is not theirs to drop");
        assert!(again.remove("alice", "b").unwrap());
        assert!(Wakes::load(&dir, Box::new(poker)).unwrap().list().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_timer_thread_fires_a_due_wake_and_holds_awake_meanwhile() {
        struct Awake(Mutex<Vec<bool>>);
        impl KeepAwake for Arc<Awake> {
            fn tick(&self, pending: bool) {
                self.0.lock().unwrap().push(pending);
            }
        }
        let dir = base();
        let poker = Arc::new(FakePoker::default());
        let wakes = Wakes::load(&dir, Box::new(poker.clone())).unwrap();
        let awake = Arc::new(Awake(Mutex::new(Vec::new())));
        // Due 50 ms after the timer starts.
        let start = now_ms();
        wakes.put(wake("t", start + 50)).unwrap();
        wakes.spawn_timer(Box::new(awake.clone())).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while poker.poked.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(*poker.poked.lock().unwrap(), ["sb-t"]);
        let ticks = awake.0.lock().unwrap().clone();
        assert_eq!(ticks.first(), Some(&true), "held while the wake was pending");
        let _ = std::fs::remove_dir_all(dir);
    }
}
