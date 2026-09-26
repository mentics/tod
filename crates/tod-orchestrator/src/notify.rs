//! Telling the app (and the phone) that a user's data changed.
//!
//! The pub/sub service is **ntfy** (`https://ntfy.sh` unless
//! [`NTFY_URL_ENV`] names another server; `off` turns publishing off). Each
//! user has two unguessable topics derived from a random secret made once
//! and kept in `<root>/notify.json`:
//!
//! - `<topic>`: "changed", at most one a second per user, trailing edge
//!   included (the last change is always announced). The app subscribes
//!   while open and pulls the feed on each message.
//! - `<topic>-alerts`: "a node needs you", when a new question (pending
//!   decision) or a newly blocked plan step appears. For the phone.
//!
//! Messages carry no data: no task text, no ids, only the kind.
//!
//! Request threads only [`Notifier::poke`]; checking the database and
//! publishing run on the notifier's own thread.

use crate::users::Users;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The ntfy server (default `https://ntfy.sh`; `off` disables publishing).
pub const NTFY_URL_ENV: &str = "TOD_NTFY_URL";
pub const DEFAULT_NTFY_URL: &str = "https://ntfy.sh";
const TOPIC_FILE: &str = "notify.json";
/// At most one "changed" per user per this.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// What a message says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Something changed; pull the feed.
    Changed,
    /// A node needs the user (a question, a blocked step).
    NeedsYou,
}

/// Where messages go.
pub trait Sink: Send + Sync {
    fn publish(&self, topic: &str, kind: Kind) -> Result<()>;
}

/// The ntfy HTTP publish API.
pub struct NtfySink {
    server: String,
    agent: ureq::Agent,
}

impl NtfySink {
    pub fn new(server: impl Into<String>) -> Self {
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).build().into();
        Self { server: server.into().trim_end_matches('/').to_string(), agent }
    }
}

impl Sink for NtfySink {
    fn publish(&self, topic: &str, kind: Kind) -> Result<()> {
        let url = format!("{}/{topic}", self.server);
        let req = self.agent.post(&url);
        let req = match kind {
            Kind::Changed => req.header("Priority", "min"),
            Kind::NeedsYou => req.header("Title", "tod").header("Priority", "high").header("Tags", "bell"),
        };
        let body = match kind {
            Kind::Changed => "changed",
            Kind::NeedsYou => "A node needs you",
        };
        req.send(body).with_context(|| format!("publish to {}", self.server))?;
        Ok(())
    }
}

/// A sink that drops everything (publishing turned off).
pub struct NoSink;
impl Sink for NoSink {
    fn publish(&self, _: &str, _: Kind) -> Result<()> {
        Ok(())
    }
}

/// The server and topics a user's app subscribes to (`GET /users/<u>/notify`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Topics {
    pub server: String,
    pub topic: String,
    pub alerts_topic: String,
}

#[derive(Serialize, Deserialize)]
struct TopicFile {
    secret: String,
}

/// The user's topic, made (and saved in `root`) on first use.
pub fn topic(root: &Path) -> Result<String> {
    let path = root.join(TOPIC_FILE);
    if let Some(f) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<TopicFile>(&b).ok()) {
        return Ok(format!("tod-{}", f.secret));
    }
    std::fs::create_dir_all(root)?;
    let secret = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    std::fs::write(&path, serde_json::to_vec(&TopicFile { secret: secret.clone() })?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(format!("tod-{secret}"))
}

pub fn alerts_topic(topic: &str) -> String {
    format!("{topic}-alerts")
}

/// What a user's data looks like to the notifier: the change log's head
/// and what needs the user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    pub last_seq: i64,
    /// Ids of pending decisions and blocked plan steps.
    pub attention: HashSet<Vec<u8>>,
}

/// Reads a user's state and topic (injected in tests).
pub trait Probe: Send + Sync {
    fn observe(&self, user: &str) -> Result<Observed>;
    fn topic(&self, user: &str) -> Result<String>;
}

/// The real probe: the user's database under `<base>/users/<u>/`.
pub struct StoreProbe(pub Arc<Users>);

impl Probe for StoreProbe {
    fn observe(&self, user: &str) -> Result<Observed> {
        let data = self.0.get(user)?;
        let _ = data.store.flush_on_quit();
        let conn = rusqlite::Connection::open(data.store.paths().db())?;
        conn.busy_timeout(Duration::from_secs(30))?;
        let last_seq = tod_store::sync::last_seq(&conn)?;
        let mut attention = HashSet::new();
        for sql in [
            "SELECT id FROM decisions WHERE status = 'pending'",
            "SELECT id FROM node_plan_steps WHERE status IN ('blocked', 'partial')",
        ] {
            // A table missing from an old database has nothing to report.
            let Ok(mut stmt) = conn.prepare(sql) else { continue };
            let ids = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
            for id in ids {
                attention.insert(id?);
            }
        }
        Ok(Observed { last_seq, attention })
    }

    fn topic(&self, user: &str) -> Result<String> {
        topic(&self.0.root_of(user)?)
    }
}

/// The at-most-once-a-second rule, per user, with the time passed in.
#[derive(Debug, Default)]
pub struct Coalescer {
    users: HashMap<String, Slot>,
}

#[derive(Debug, Default)]
struct Slot {
    last_run: Option<Instant>,
    due: Option<Instant>,
}

impl Coalescer {
    /// A change for `user` at `now`: schedules a run now, or one interval
    /// after the last, unless one is already scheduled (which then covers it).
    pub fn poke(&mut self, user: &str, now: Instant) {
        let slot = self.users.entry(user.to_string()).or_default();
        if slot.due.is_none() {
            let earliest = slot.last_run.map_or(now, |t| t + MIN_INTERVAL);
            slot.due = Some(earliest.max(now));
        }
    }

    /// The earliest scheduled run.
    pub fn next_due(&self) -> Option<Instant> {
        self.users.values().filter_map(|s| s.due).min()
    }

    /// The users due at `now`, marked as run.
    pub fn take_due(&mut self, now: Instant) -> Vec<String> {
        let mut out = Vec::new();
        for (user, slot) in &mut self.users {
            if slot.due.is_some_and(|d| d <= now) {
                slot.due = None;
                slot.last_run = Some(now);
                out.push(user.clone());
            }
        }
        out.sort();
        out
    }
}

/// What was last announced for a user.
#[derive(Default)]
struct Announced {
    last_seq: Option<i64>,
    attention: Option<HashSet<Vec<u8>>>,
}

/// Decides what to publish for one observation; updates `seen`.
fn decide(seen: &mut Announced, now: Observed) -> (bool, bool) {
    let changed = seen.last_seq.is_none_or(|s| s != now.last_seq);
    // The first look at a user sets the baseline: nothing is "new" yet.
    let needs_you = seen.attention.as_ref().is_some_and(|before| now.attention.iter().any(|id| !before.contains(id)));
    seen.last_seq = Some(now.last_seq);
    seen.attention = Some(now.attention);
    (changed, needs_you)
}

pub struct Notifier {
    state: Mutex<Coalescer>,
    wake: Condvar,
    probe: Box<dyn Probe>,
    sink: Box<dyn Sink>,
    announced: Mutex<HashMap<String, Announced>>,
}

impl Notifier {
    /// Starts the notifier's thread.
    pub fn start(probe: Box<dyn Probe>, sink: Box<dyn Sink>) -> Arc<Self> {
        let n = Arc::new(Self {
            state: Mutex::new(Coalescer::default()),
            wake: Condvar::new(),
            probe,
            sink,
            announced: Mutex::new(HashMap::new()),
        });
        let worker = n.clone();
        std::thread::Builder::new()
            .name("tod-orchestrator-notify".into())
            .spawn(move || worker.run())
            .expect("spawn the notifier thread");
        n
    }

    /// The sink from the environment: ntfy at [`NTFY_URL_ENV`] (else
    /// ntfy.sh), or none when it is `off`.
    /// Publishes only when `TOD_NTFY_URL` is set (and not `off`): what
    /// [`crate::Server::new`] uses, so tests and embedders never reach the
    /// network unasked. The binary uses [`Self::default_sink`].
    pub fn sink_from_env() -> Box<dyn Sink> {
        match std::env::var(NTFY_URL_ENV) {
            Ok(v) if !v.is_empty() => Self::default_sink(),
            _ => Box::new(NoSink),
        }
    }

    /// [`server_from_env`]: `TOD_NTFY_URL`, else ntfy.sh; `off` is none.
    pub fn default_sink() -> Box<dyn Sink> {
        match server_from_env() {
            Some(server) => Box::new(NtfySink::new(server)),
            None => Box::new(NoSink),
        }
    }

    /// A change may have been committed for `user`. Never blocks on I/O.
    pub fn poke(&self, user: &str) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).poke(user, Instant::now());
        self.wake.notify_one();
    }

    fn run(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let now = Instant::now();
            let due = state.take_due(now);
            if !due.is_empty() {
                drop(state);
                for user in due {
                    self.announce(&user);
                }
                state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                continue;
            }
            state = match state.next_due() {
                Some(t) => self.wake.wait_timeout(state, t.saturating_duration_since(now)).unwrap_or_else(|e| e.into_inner()).0,
                None => self.wake.wait(state).unwrap_or_else(|e| e.into_inner()),
            };
        }
    }

    fn announce(&self, user: &str) {
        if let Err(err) = self.try_announce(user) {
            eprintln!("tod-orchestrator: notify {user}: {err:#}");
        }
    }

    fn try_announce(&self, user: &str) -> Result<()> {
        let observed = self.probe.observe(user)?;
        let (changed, needs_you) = {
            let mut announced = self.announced.lock().unwrap_or_else(|e| e.into_inner());
            decide(announced.entry(user.to_string()).or_default(), observed)
        };
        if !changed && !needs_you {
            return Ok(());
        }
        let topic = self.probe.topic(user)?;
        if changed {
            self.sink.publish(&topic, Kind::Changed)?;
        }
        if needs_you {
            self.sink.publish(&alerts_topic(&topic), Kind::NeedsYou)?;
        }
        Ok(())
    }
}

/// The ntfy server, or `None` when publishing is off.
pub fn server_from_env() -> Option<String> {
    let url = std::env::var(NTFY_URL_ENV).unwrap_or_else(|_| DEFAULT_NTFY_URL.into());
    (!url.is_empty() && url != "off").then(|| url.trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_poke_runs_now_then_one_a_second_with_a_trailing_run() {
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        c.poke("a", t0);
        assert_eq!(c.take_due(t0), vec!["a"]);
        // Three changes within the next second: one run, a second after the last.
        for ms in [100, 300, 900] {
            c.poke("a", t0 + Duration::from_millis(ms));
        }
        assert_eq!(c.next_due(), Some(t0 + MIN_INTERVAL));
        assert!(c.take_due(t0 + Duration::from_millis(950)).is_empty());
        assert_eq!(c.take_due(t0 + MIN_INTERVAL), vec!["a"]);
        assert_eq!(c.next_due(), None);
        // Long after: immediate again.
        let later = t0 + Duration::from_secs(10);
        c.poke("a", later);
        assert_eq!(c.next_due(), Some(later));
    }

    #[test]
    fn users_are_coalesced_separately() {
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        c.poke("a", t0);
        c.take_due(t0);
        c.poke("a", t0);
        c.poke("b", t0);
        assert_eq!(c.take_due(t0), vec!["b"]);
        assert_eq!(c.take_due(t0 + MIN_INTERVAL), vec!["a"]);
    }

    #[test]
    fn needs_you_only_for_new_attention_after_the_baseline() {
        let mut seen = Announced::default();
        let obs = |seq, ids: &[u8]| Observed { last_seq: seq, attention: ids.iter().map(|i| vec![*i]).collect() };
        assert_eq!(decide(&mut seen, obs(1, &[1])), (true, false));
        assert_eq!(decide(&mut seen, obs(1, &[1])), (false, false));
        assert_eq!(decide(&mut seen, obs(2, &[1, 2])), (true, true));
        assert_eq!(decide(&mut seen, obs(3, &[2])), (true, false));
    }

    #[test]
    fn topic_is_made_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("tod-notify-topic-{}", uuid::Uuid::new_v4()));
        let a = topic(&dir).unwrap();
        assert_eq!(a, topic(&dir).unwrap());
        assert!(a.len() > 60, "{a}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
