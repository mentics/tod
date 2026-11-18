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
//!   ([`KeepAwake`]); [`LocalRelayAwake`] does it by renewing a lease on
//!   its own relay's hold (`POST 127.0.0.1:2222/hold?reason=wakes&secs=120`)
//!   on every tick, and `/release?reason=wakes` once nothing is pending.
//!
//! A poke that finds the sandbox gone ([`SandboxGone`]: Blaxel has no such
//! sandbox, or terminated) is not retried; a 404 from a sandbox Blaxel still has is retried: the wake is dropped and
//! the lost handler ([`Wakes::set_lost_handler`]; the server marks the node's
//! `cloud_nodes.lost_at`) is told, so the app replaces the sandbox on its
//! next sync. Only the app can: the user's tokens are only on their machine.
//!
//! **The orchestrator sleeps between wakes.** It is billed while awake, so
//! with a wake pending it does not hold itself up until the wake is due. Past
//! [`HOLD_WITHIN`] it makes a one-shot Blaxel schedule on its own sandbox
//! ([`SelfWake`], `orchestrator-wake`) that calls `POST /tick` at the earliest
//! due time, releases its hold, and goes to sleep; the schedule wakes the
//! sandbox (standby included), the tick takes a hold and the timer fires what
//! is due. A new earliest wake replaces the schedule; no wake left removes it.
//! When it cannot arm one (no Blaxel account or sandbox name in its
//! environment, or Blaxel refusing) it says so and stays awake as before,
//! rather than sleep through a wake.
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

/// A poke's error when the sandbox no longer exists.
#[derive(Debug)]
pub struct SandboxGone(pub String);

impl std::fmt::Display for SandboxGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sandbox {} is gone", self.0)
    }
}

impl std::error::Error for SandboxGone {}

/// Whether a poke failed because the sandbox is gone.
pub fn is_gone(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.downcast_ref::<SandboxGone>().is_some())
}

/// Told about a wake whose sandbox is gone.
pub type LostHandler = Box<dyn Fn(&Wake) + Send + Sync>;

/// What a due wake does, as its gate sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Poke the sandbox: there is work for it, or the gate cannot tell.
    Poke,
    /// Nothing to do yet: look again at `at` (ms) instead, under wake id
    /// `id` (the wait it replaced is gone), and leave the sandbox asleep.
    Later { id: String, at: i64 },
    /// The wait this wake was for no longer belongs to its node's state
    /// (and has been ended): drop the wake and poke nothing.
    Drop,
}

/// Looks at a due wake before its sandbox is poked, so a wait the
/// orchestrator can answer itself (a pull request still unmerged) wakes
/// nothing. See [`crate::github_gate`].
pub trait WakeGate: Send + Sync {
    fn check(&self, wake: &Wake) -> Verdict;
}

/// Wakes a sandbox.
pub trait Poker: Send + Sync {
    fn poke(&self, wake: &Wake) -> Result<()>;
}

/// A wake due within this long (or overdue) keeps the orchestrator awake for
/// it; a later one lets it sleep, woken by a schedule. A Blaxel schedule fires
/// 10 to 50 seconds after its time, so for a nearer wake sleeping would only
/// make it late.
pub const HOLD_WITHIN: Duration = Duration::from_secs(90);

/// Keeps the orchestrator's own sandbox up for the wakes that are due soon,
/// and lets it sleep for the rest.
pub trait KeepAwake: Send + Sync {
    /// Called by the timer on every pass: `next_due` is the earliest pending
    /// wake (ms), `None` when there is none, and `now` the time (ms).
    /// Implementations renew their hold, or let it lapse.
    fn tick(&self, next_due: Option<i64>, now: i64);

    /// Holds the sandbox awake for a lease from now: a wake's tick has come
    /// in and the timer is about to do its work.
    fn hold(&self) {}
}

pub struct NoKeepAwake;
impl KeepAwake for NoKeepAwake {
    fn tick(&self, _next_due: Option<i64>, _now: i64) {}
}

/// Wakes the orchestrator's own sandbox at a time (a Blaxel schedule), so it
/// can sleep until then.
pub trait SelfWake: Send + Sync {
    /// Wake the sandbox at `at_ms`, replacing any earlier time.
    fn arm(&self, at_ms: i64) -> Result<()>;
    /// Drop the wake.
    fn disarm(&self) -> Result<()>;
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
    lost: std::sync::OnceLock<LostHandler>,
    gate: std::sync::OnceLock<Box<dyn WakeGate>>,
    /// What keeps the sandbox up, once the timer runs (for [`Self::tick`]).
    awake: std::sync::OnceLock<Arc<dyn KeepAwake>>,
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
        Ok(Arc::new(Self { path, rows: Mutex::new(rows), changed: Condvar::new(), poker, lost: std::sync::OnceLock::new(), gate: std::sync::OnceLock::new(), awake: std::sync::OnceLock::new() }))
    }

    /// Who hears of a sandbox found gone (set once; later calls are ignored).
    pub fn set_lost_handler(&self, handler: LostHandler) {
        let _ = self.lost.set(handler);
    }

    /// The gate every due wake passes before its sandbox is poked (set once;
    /// later calls are ignored). A wake poked by [`Self::poke_now`] (a
    /// webhook, an answer) is not gated: something happened.
    pub fn set_gate(&self, gate: Box<dyn WakeGate>) {
        let _ = self.gate.set(gate);
    }

    fn report_lost(&self, wake: &Wake) {
        eprintln!("tod-orchestrator: node {} lost its sandbox {}", wake.node, wake.sandbox);
        if let Some(handler) = self.lost.get() {
            handler(wake);
        }
    }

    pub fn list(&self) -> Vec<Wake> {
        self.rows.lock().unwrap().clone()
    }

    fn save(&self, rows: &[Wake]) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        // Written and synced before the rename, so a crash leaves the old
        // file or the new one, never half of either.
        let mut file = std::fs::File::create(&tmp).with_context(|| format!("write {}", tmp.display()))?;
        file.write_all(&serde_json::to_vec_pretty(rows)?).with_context(|| format!("write {}", tmp.display()))?;
        file.sync_all().with_context(|| format!("sync {}", tmp.display()))?;
        drop(file);
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

    /// Records `new` (all of one node) only when `user` has no wake at all
    /// for `node`, under one lock and one save, so a rebuild never adds a
    /// second wake beside one that exists. Returns how many were added.
    pub fn put_if_node_has_none(&self, user: &str, node: &str, new: Vec<Wake>) -> Result<usize> {
        let mut rows = self.rows.lock().unwrap();
        if new.is_empty() || rows.iter().any(|w| w.user == user && w.node == node) {
            return Ok(0);
        }
        let added = new.len();
        rows.extend(new);
        self.save(&rows)?;
        self.changed.notify_all();
        Ok(added)
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
            let verdict = self.gate.get().map(|g| g.check(&wake));
            if verdict == Some(Verdict::Drop) {
                let mut rows = self.rows.lock().unwrap();
                rows.retain(|w| w != &wake);
                let _ = self.save(&rows).map_err(|e| eprintln!("tod-orchestrator: wakes: {e:#}"));
                continue;
            }
            if let Some(Verdict::Later { id, at }) = verdict {
                let mut rows = self.rows.lock().unwrap();
                // Only if it was not replaced or dropped meanwhile.
                if rows.iter().any(|w| w == &wake) {
                    rows.retain(|w| w != &wake && w.id != id);
                    rows.push(Wake { id, at: at.max(now + 1000), ..wake });
                    let _ = self.save(&rows).map_err(|e| eprintln!("tod-orchestrator: wakes: {e:#}"));
                }
                continue;
            }
            match self.poker.poke(&wake) {
                Ok(()) => {
                    let mut rows = self.rows.lock().unwrap();
                    // Only if it was not replaced by a later time meanwhile.
                    rows.retain(|w| w != &wake);
                    let _ = self.save(&rows).map_err(|e| eprintln!("tod-orchestrator: wakes: {e:#}"));
                    fired.push(wake.id);
                }
                Err(err) if is_gone(&err) => {
                    self.report_lost(&wake);
                    let mut rows = self.rows.lock().unwrap();
                    rows.retain(|w| w != &wake);
                    let _ = self.save(&rows).map_err(|e| eprintln!("tod-orchestrator: wakes: {e:#}"));
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

    /// Pokes `wake`'s sandbox now; if that fails, records it to be retried
    /// [`RETRY_MS`] later by the timer.
    pub fn poke_now(&self, mut wake: Wake) {
        if let Err(err) = self.poker.poke(&wake) {
            if is_gone(&err) {
                self.report_lost(&wake);
                return;
            }
            eprintln!("tod-orchestrator: poke {} ({}): {err:#}; retrying later", wake.sandbox, wake.id);
            wake.at = now_ms() + RETRY_MS;
            let _ = self.put(wake).map_err(|e| eprintln!("tod-orchestrator: wakes: {e:#}"));
        }
    }

    /// Runs the timer forever: fires due wakes, ticks `awake`, and sleeps
    /// until the next wake, a change, or [`MAX_SLEEP`].
    pub fn run_timer(self: &Arc<Self>, clock: &dyn Fn() -> i64, awake: &dyn KeepAwake) {
        loop {
            self.fire_due(clock());
            let next = self.earliest();
            // Without the lock: arming a schedule is a call to Blaxel.
            awake.tick(next, clock());
            let rows = self.rows.lock().unwrap();
            // A wake put or dropped meanwhile is looked at now, not a minute on.
            if rows.iter().map(|w| w.at).min() != next {
                continue;
            }
            let sleep = match next {
                Some(at) => Duration::from_millis((at - clock()).max(0) as u64).min(MAX_SLEEP),
                None => MAX_SLEEP,
            };
            // A pending wake keeps the tick at most 45 s apart for `awake`.
            let sleep = if next.is_some() { sleep.min(Duration::from_secs(45)) } else { sleep };
            let _ = self.changed.wait_timeout(rows, sleep).unwrap();
        }
    }

    fn earliest(&self) -> Option<i64> {
        self.rows.lock().unwrap().iter().map(|w| w.at).min()
    }

    /// Starts [`Wakes::run_timer`] on its own thread with the real clock.
    pub fn spawn_timer(self: &Arc<Self>, awake: Box<dyn KeepAwake>) -> Result<()> {
        let awake: Arc<dyn KeepAwake> = Arc::from(awake);
        let _ = self.awake.set(awake.clone());
        let wakes = self.clone();
        std::thread::Builder::new()
            .name("tod-orchestrator-wakes".into())
            .spawn(move || wakes.run_timer(&now_ms, awake.as_ref()))
            .context("spawn the wake timer")?;
        Ok(())
    }

    /// `POST /tick`, from the schedule [`SelfWake`] made: the sandbox has
    /// just been woken for a wake that is due. Holds it awake before the
    /// request returns (it would otherwise be free to sleep again before the
    /// timer's pokes are done) and has the timer look now.
    pub fn tick(&self) {
        if let Some(awake) = self.awake.get() {
            awake.hold();
        }
        self.changed.notify_all();
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
    creds: Option<(String, String)>,
}

impl RelayPoker {
    pub fn from_env() -> Self {
        let creds = match (
            std::env::var("TOD_ORCHESTRATOR_BLAXEL_WORKSPACE"),
            std::env::var("TOD_ORCHESTRATOR_BLAXEL_TOKEN"),
        ) {
            (Ok(ws), Ok(token)) if !ws.is_empty() && !token.is_empty() => Some((ws, token)),
            _ => None,
        };
        Self { creds }
    }

    /// The one HTTP client for a poke: both the Blaxel control-plane lookup
    /// and the relay request. It checks servers against the host's CA store
    /// ([`crate::notify::system_roots`]), never `ureq`'s bundled roots: those
    /// rejected the control plane's chain from the deployed orchestrator
    /// (`UnknownIssuer`), so every poke failed and no answer or wake ever
    /// reached a node.
    fn agent() -> Result<ureq::Agent> {
        Ok(ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(60)))
            .tls_config(crate::notify::system_roots()?)
            .build()
            .into())
    }
}

/// What a poke's 404 means once Blaxel has been asked whether the sandbox
/// exists: gone if not, else an ordinary failure the wake is retried after.
pub(crate) fn after_poke_404(sandbox_exists: bool, sandbox: &str, url: &str) -> anyhow::Error {
    if sandbox_exists {
        anyhow::anyhow!("poke {url}: 404, but {sandbox} exists (its workload is waking, or its relay lacks /poke); retrying")
    } else {
        anyhow::Error::new(SandboxGone(sandbox.to_string())).context(format!("poke {url}: 404"))
    }
}

impl Poker for RelayPoker {
    fn poke(&self, wake: &Wake) -> Result<()> {
        let Some((workspace, token)) = &self.creds else {
            bail!("TOD_ORCHESTRATOR_BLAXEL_WORKSPACE and TOD_ORCHESTRATOR_BLAXEL_TOKEN are not set");
        };
        let agent = Self::agent()?;
        let bx = tod_sandbox::blaxel::Blaxel::with_agent(workspace.clone(), token.clone(), agent.clone());
        let Some(info) = bx.get(&wake.sandbox)? else {
            return Err(SandboxGone(wake.sandbox.clone()).into());
        };
        if matches!(info.status.as_str(), "FAILED" | "TERMINATED" | "DELETING") {
            return Err(SandboxGone(wake.sandbox.clone()).into());
        }
        let url = info.url.with_context(|| format!("{} has no URL", wake.sandbox))?;
        let url = format!("{}/port/{}/poke", url.trim_end_matches('/'), tod_sandbox::blaxel::RELAY_PORT);
        let resp = agent
            .post(&url)
            .header("Authorization", &format!("Bearer {}", bx.token()))
            .send_empty()
            .context("poke")?;
        let status = resp.status().as_u16();
        if status == 404 {
            // Blaxel answers 404 while a standby sandbox's workload is still
            // waking, and an older relay answers it for a route it lacks:
            // neither is a sandbox that is gone. Only Blaxel having no such
            // sandbox is.
            let exists = bx.get(&wake.sandbox)?.is_some();
            return Err(after_poke_404(exists, &wake.sandbox, &url));
        }
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

impl LocalRelayAwake {
    fn post(&self, path: &str) -> std::io::Result<()> {
        let mut s = std::net::TcpStream::connect(&self.addr)?;
        s.set_write_timeout(Some(Duration::from_secs(5)))?;
        write!(s, "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    }

    /// A lease on the relay's hold, for two minutes (it lapses on its own).
    fn hold(&self) -> std::io::Result<()> {
        self.post("/hold?reason=wakes&secs=120")
    }

    fn release(&self) -> std::io::Result<()> {
        self.post("/release?reason=wakes")
    }
}

/// What [`SleepingAwake`] has done, so it talks to the relay and Blaxel only
/// when something changes.
#[derive(Default)]
struct Standing {
    held: bool,
    /// The time the schedule is set for (ms), if one is.
    armed: Option<i64>,
    /// Whether the reason it cannot sleep has been said already.
    told: bool,
}

/// The real [`KeepAwake`]: holds the relay for a wake due soon and, for a
/// later one, arms a schedule on its own sandbox and lets go, so the
/// sandbox sleeps (module docs).
pub struct SleepingAwake {
    relay: LocalRelayAwake,
    waker: Result<Box<dyn SelfWake>, String>,
    standing: Mutex<Standing>,
}

impl SleepingAwake {
    pub fn new(relay: LocalRelayAwake, waker: Result<Box<dyn SelfWake>, String>) -> Self {
        Self { relay, waker, standing: Mutex::new(Standing::default()) }
    }

    /// With the schedule from the environment ([`BlaxelSelfWake::from_env`]).
    pub fn from_env(base: &std::path::Path, port: u16) -> Self {
        let waker = BlaxelSelfWake::from_env(base, port).map(|w| Box::new(w) as Box<dyn SelfWake>).map_err(|e| format!("{e:#}"));
        Self::new(LocalRelayAwake::default(), waker)
    }

    fn take_hold(&self, standing: &mut Standing) {
        match self.relay.hold() {
            Ok(()) => standing.held = true,
            Err(err) => eprintln!("tod-orchestrator: cannot hold awake through {}: {err}", self.relay.addr),
        }
    }
}

impl KeepAwake for SleepingAwake {
    fn hold(&self) {
        let mut standing = self.standing.lock().unwrap_or_else(|e| e.into_inner());
        self.take_hold(&mut standing);
    }

    fn tick(&self, next_due: Option<i64>, now: i64) {
        let mut standing = self.standing.lock().unwrap_or_else(|e| e.into_inner());
        match next_due {
            None => {
                if standing.held {
                    match self.relay.release() {
                        Ok(()) => standing.held = false,
                        Err(err) => eprintln!("tod-orchestrator: cannot release the hold through {}: {err}", self.relay.addr),
                    }
                }
                if standing.armed.is_some() {
                    match self.waker.as_ref().map(|w| w.disarm()) {
                        Ok(Ok(())) | Err(_) => standing.armed = None,
                        Ok(Err(err)) => eprintln!("tod-orchestrator: cannot remove its wake schedule: {err:#}"),
                    }
                }
            }
            Some(at) if at - now <= HOLD_WITHIN.as_millis() as i64 => self.take_hold(&mut standing),
            Some(at) => {
                let armed = match &self.waker {
                    Ok(_) if standing.armed == Some(at) => true,
                    Ok(waker) => match waker.arm(at) {
                        Ok(()) => {
                            standing.armed = Some(at);
                            standing.told = false;
                            true
                        }
                        Err(err) => {
                            eprintln!("tod-orchestrator: cannot arm its wake schedule ({err:#}); staying awake for the wake due in {}s", (at - now) / 1000);
                            false
                        }
                    },
                    Err(why) => {
                        if !standing.told {
                            eprintln!("tod-orchestrator: cannot sleep between wakes ({why}); staying awake while one is pending");
                            standing.told = true;
                        }
                        false
                    }
                };
                if armed {
                    if standing.held {
                        match self.relay.release() {
                            Ok(()) => standing.held = false,
                            Err(err) => eprintln!("tod-orchestrator: cannot release the hold through {}: {err}", self.relay.addr),
                        }
                    }
                } else {
                    // Never sleep through a wake that cannot be woken for.
                    self.take_hold(&mut standing);
                }
            }
        }
    }
}

/// The name of the orchestrator's own wake schedule.
pub const SELF_WAKE_NAME: &str = "orchestrator-wake";
/// The environment variable naming the orchestrator's own sandbox.
pub const SANDBOX_ENV: &str = "TOD_ORCHESTRATOR_SANDBOX";
/// The header a tick carries its secret in.
pub const TICK_HEADER: &str = "X-Tod-Tick";
const TICK_FILE: &str = "tick-token";

/// The secret a `POST /tick` must carry, kept in `<base>/tick-token` (made
/// on first use, so a schedule made before a restart still works after it).
/// The route is on a public port, and a tick holds the sandbox awake.
pub fn tick_token(base: &std::path::Path) -> Result<String> {
    let path = base.join(TICK_FILE);
    match std::fs::read_to_string(&path) {
        Ok(t) if !t.trim().is_empty() => Ok(t.trim().to_string()),
        Ok(_) => bail!("{} is empty", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
            std::fs::create_dir_all(base).with_context(|| format!("create {}", base.display()))?;
            std::fs::write(&path, &token).with_context(|| format!("write {}", path.display()))?;
            Ok(token)
        }
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Whether `given` is the tick secret (compared without stopping at the
/// first difference).
pub fn tick_allowed(token: &str, given: Option<&str>) -> bool {
    let Some(given) = given else { return false };
    given.len() == token.len() && given.bytes().zip(token.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

/// What the schedule runs in the orchestrator's sandbox: a tick on loopback.
pub fn tick_command(port: u16, token: &str) -> String {
    format!("curl -fsS -m 20 -X POST -H '{TICK_HEADER}: {token}' http://127.0.0.1:{port}/tick")
}

/// The body of the schedule that wakes the orchestrator at `at_ms` (never
/// sooner than five seconds after `now_ms`). `keepAlive` holds the sandbox
/// only while the tick runs; the tick then takes the orchestrator's own hold.
pub fn self_wake_body(port: u16, token: &str, at_ms: i64, now_ms: i64) -> serde_json::Value {
    tod_sandbox::blaxel::schedule_body(SELF_WAKE_NAME, &tick_command(port, token), at_ms.max(now_ms + 5_000), true, 120)
}

/// [`SelfWake`] through Blaxel's schedules on the orchestrator's own sandbox.
pub struct BlaxelSelfWake {
    blaxel: tod_sandbox::blaxel::Blaxel,
    sandbox: String,
    port: u16,
    token: String,
}

impl BlaxelSelfWake {
    pub fn new(blaxel: tod_sandbox::blaxel::Blaxel, sandbox: String, port: u16, token: String) -> Self {
        Self { blaxel, sandbox, port, token }
    }

    /// From the environment the orchestrator is deployed with: the Blaxel
    /// account (`TOD_ORCHESTRATOR_BLAXEL_*`) and its own sandbox's name
    /// ([`SANDBOX_ENV`]).
    pub fn from_env(base: &std::path::Path, port: u16) -> Result<Self> {
        let var = |name: &str| {
            std::env::var(name).ok().filter(|v| !v.trim().is_empty()).with_context(|| format!("{name} is not set"))
        };
        let blaxel = tod_sandbox::blaxel::Blaxel::with_agent(
            var("TOD_ORCHESTRATOR_BLAXEL_WORKSPACE")?,
            var("TOD_ORCHESTRATOR_BLAXEL_TOKEN")?,
            RelayPoker::agent()?,
        );
        Ok(Self::new(blaxel, var(SANDBOX_ENV)?, port, tick_token(base)?))
    }
}

impl SelfWake for BlaxelSelfWake {
    fn arm(&self, at_ms: i64) -> Result<()> {
        // Replace any earlier time: drop the old one by name, then create.
        self.blaxel.delete_schedules_named(&self.sandbox, SELF_WAKE_NAME)?;
        self.blaxel.create_schedule(&self.sandbox, &self_wake_body(self.port, &self.token, at_ms, now_ms()))?;
        Ok(())
    }

    fn disarm(&self) -> Result<()> {
        self.blaxel.delete_schedules_named(&self.sandbox, SELF_WAKE_NAME)?;
        Ok(())
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
        gone: AtomicBool,
    }
    impl Poker for Arc<FakePoker> {
        fn poke(&self, wake: &Wake) -> Result<()> {
            if self.gone.load(Ordering::SeqCst) {
                return Err(SandboxGone(wake.sandbox.clone()).into());
            }
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
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = tod_test_util::scratch_dir(format!("tod-orch-wakes-{}-{}-{n}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_poke_404_from_a_sandbox_that_exists_is_not_gone() {
        let live = after_poke_404(true, "node-a", "https://x/poke");
        assert!(!is_gone(&live), "{live:#}");
        let gone = after_poke_404(false, "node-a", "https://x/poke");
        assert!(is_gone(&gone), "{gone:#}");
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
    fn a_gone_sandbox_is_reported_lost_and_not_retried() {
        let dir = base();
        let poker = Arc::new(FakePoker::default());
        poker.gone.store(true, Ordering::SeqCst);
        let wakes = Wakes::load(&dir, Box::new(poker.clone())).unwrap();
        let lost = Arc::new(Mutex::new(Vec::new()));
        let seen = lost.clone();
        wakes.set_lost_handler(Box::new(move |w: &Wake| seen.lock().unwrap().push(w.sandbox.clone())));

        wakes.poke_now(wake("now", 0));
        assert!(wakes.list().is_empty(), "a poke of a gone sandbox is not retried");
        wakes.put(wake("due", 100)).unwrap();
        assert!(wakes.fire_due(200).is_empty());
        assert!(wakes.list().is_empty());
        assert_eq!(*lost.lock().unwrap(), ["sb-now", "sb-due"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_timer_thread_fires_a_due_wake_and_holds_awake_meanwhile() {
        struct Awake(Mutex<Vec<bool>>);
        impl KeepAwake for Arc<Awake> {
            fn tick(&self, next_due: Option<i64>, _now: i64) {
                self.0.lock().unwrap().push(next_due.is_some());
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

    /// A relay on loopback that records the path of each request it gets.
    fn fake_relay() -> (LocalRelayAwake, Arc<Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                let _ = BufReader::new(stream).read_line(&mut line);
                log.lock().unwrap().push(line.split_whitespace().nth(1).unwrap_or_default().to_string());
            }
        });
        (LocalRelayAwake { addr }, seen)
    }

    fn seen_eventually(seen: &Arc<Mutex<Vec<String>>>, n: usize) -> Vec<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while seen.lock().unwrap().len() < n && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));
        seen.lock().unwrap().clone()
    }

    #[derive(Default)]
    struct FakeWaker {
        armed: Mutex<Vec<i64>>,
        disarmed: Mutex<u32>,
        fail: AtomicBool,
    }
    impl SelfWake for Arc<FakeWaker> {
        fn arm(&self, at_ms: i64) -> Result<()> {
            if self.fail.load(Ordering::SeqCst) {
                bail!("blaxel is down");
            }
            self.armed.lock().unwrap().push(at_ms);
            Ok(())
        }
        fn disarm(&self) -> Result<()> {
            *self.disarmed.lock().unwrap() += 1;
            Ok(())
        }
    }

    const NOW: i64 = 1_000_000;
    const FAR: i64 = NOW + 10 * 60_000;

    #[test]
    fn a_far_wake_arms_a_schedule_and_lets_the_sandbox_sleep() {
        let (relay, seen) = fake_relay();
        let waker = Arc::new(FakeWaker::default());
        let awake = SleepingAwake::new(relay, Ok(Box::new(waker.clone())));
        // Awake for something else first: the hold is let go once armed.
        awake.hold();
        awake.tick(Some(FAR), NOW);
        awake.tick(Some(FAR), NOW + 45_000);
        awake.tick(Some(FAR), NOW + 90_000);
        assert_eq!(*waker.armed.lock().unwrap(), [FAR], "armed once, not on every pass");
        assert_eq!(seen_eventually(&seen, 2), ["/hold?reason=wakes&secs=120", "/release?reason=wakes"], "released once, then silent");
        // An earlier wake replaces the schedule.
        awake.tick(Some(FAR - 60_000), NOW + 100_000);
        assert_eq!(*waker.armed.lock().unwrap(), [FAR, FAR - 60_000]);
    }

    #[test]
    fn a_wake_due_soon_holds_the_sandbox_and_makes_no_schedule() {
        let (relay, seen) = fake_relay();
        let waker = Arc::new(FakeWaker::default());
        let awake = SleepingAwake::new(relay, Ok(Box::new(waker.clone())));
        awake.tick(Some(NOW + 60_000), NOW);
        awake.tick(Some(NOW - 5_000), NOW);
        assert!(waker.armed.lock().unwrap().is_empty());
        assert_eq!(seen_eventually(&seen, 2), ["/hold?reason=wakes&secs=120"; 2], "renewed on each pass");
    }

    #[test]
    fn a_schedule_that_cannot_be_made_keeps_the_sandbox_awake() {
        let (relay, seen) = fake_relay();
        let waker = Arc::new(FakeWaker::default());
        waker.fail.store(true, Ordering::SeqCst);
        let awake = SleepingAwake::new(relay, Ok(Box::new(waker.clone())));
        awake.tick(Some(FAR), NOW);
        assert_eq!(seen_eventually(&seen, 1), ["/hold?reason=wakes&secs=120"], "never sleep through a wake");

        let (relay, seen) = fake_relay();
        let awake = SleepingAwake::new(relay, Err("TOD_ORCHESTRATOR_SANDBOX is not set".into()));
        awake.tick(Some(FAR), NOW);
        assert_eq!(seen_eventually(&seen, 1), ["/hold?reason=wakes&secs=120"]);
    }

    #[test]
    fn no_wake_left_removes_the_schedule_and_the_hold() {
        let (relay, seen) = fake_relay();
        let waker = Arc::new(FakeWaker::default());
        let awake = SleepingAwake::new(relay, Ok(Box::new(waker.clone())));
        awake.tick(Some(FAR), NOW);
        awake.tick(None, NOW + 1);
        awake.tick(None, NOW + 2);
        assert_eq!(*waker.disarmed.lock().unwrap(), 1, "removed once");
        assert!(seen_eventually(&seen, 1).is_empty(), "nothing was held, so nothing to release");
    }

    #[test]
    fn the_timer_notices_a_wake_put_while_it_was_ticking_and_the_tick_route_holds() {
        #[derive(Default)]
        struct Awake {
            ticks: Mutex<Vec<Option<i64>>>,
            holds: Mutex<u32>,
        }
        impl KeepAwake for Arc<Awake> {
            fn tick(&self, next_due: Option<i64>, _now: i64) {
                self.ticks.lock().unwrap().push(next_due);
            }
            fn hold(&self) {
                *self.holds.lock().unwrap() += 1;
            }
        }
        let dir = base();
        let wakes = Wakes::load(&dir, Box::new(Arc::new(FakePoker::default()))).unwrap();
        let awake = Arc::new(Awake::default());
        wakes.spawn_timer(Box::new(awake.clone())).unwrap();
        wakes.tick();
        assert_eq!(*awake.holds.lock().unwrap(), 1, "held before the tick returns");
        wakes.put(wake("later", now_ms() + 3_600_000)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !awake.ticks.lock().unwrap().iter().any(|t| t.is_some()) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(awake.ticks.lock().unwrap().iter().any(|t| t.is_some()), "a new wake is looked at at once");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_tick_secret_is_kept_across_restarts_and_checked() {
        let dir = base();
        let token = tick_token(&dir).unwrap();
        assert!(token.len() >= 32);
        assert_eq!(tick_token(&dir).unwrap(), token, "a schedule made before a restart still works");
        assert!(tick_allowed(&token, Some(&token)));
        assert!(!tick_allowed(&token, Some("nope")));
        assert!(!tick_allowed(&token, Some(&format!("{token}x"))));
        assert!(!tick_allowed(&token, None));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_schedule_runs_a_tick_with_the_secret_at_the_wake_time() {
        let body = self_wake_body(8090, "sekrit", FAR, NOW);
        assert_eq!(body["input"]["name"], SELF_WAKE_NAME);
        assert_eq!(body["input"]["keepAlive"], true);
        let command = body["input"]["command"].as_str().unwrap();
        assert!(command.contains("http://127.0.0.1:8090/tick") && command.contains("X-Tod-Tick: sekrit"), "{command}");
        assert_eq!(body["value"], tod_sandbox::blaxel::rfc3339_utc(FAR / 1000 + i64::from(FAR % 1000 != 0)));
        // A time already past is not made sooner than five seconds from now.
        let late = self_wake_body(8090, "t", NOW - 1_000, NOW);
        assert_eq!(late["value"], tod_sandbox::blaxel::rfc3339_utc((NOW + 5_000) / 1000));
    }

    #[test]
    fn relay_poker_without_credentials_says_so() {
        let poker = RelayPoker { creds: None };
        let err = poker.poke(&wake("a", 0)).unwrap_err();
        assert!(format!("{err:#}").contains("TOD_ORCHESTRATOR_BLAXEL_TOKEN"), "{err:#}");
    }

    /// The poke's one client (the Blaxel lookup and the relay request both
    /// use it) is built on the host's CA store, never the bundled roots that
    /// rejected the control plane (`UnknownIssuer`) and left every answered
    /// decision unpoked. On a host with no CA store it fails by saying so.
    #[test]
    fn relay_poker_client_uses_the_host_ca_store() {
        match RelayPoker::agent() {
            Ok(_) => assert!(crate::notify::system_roots().is_ok()),
            Err(err) => assert!(format!("{err:#}").contains("system CA certificates"), "{err:#}"),
        }
    }
}
