//! Hearing from the orchestrator that something changed.
//!
//! The orchestrator publishes a data-free "changed" message to the user's
//! ntfy topic whenever it commits a change for them (at most one every 30 seconds per user;
//! `tod-orchestrator`'s `notify` module). While the app is open and this data
//! root is seeded, [`start`] holds a subscription to that topic
//! (`GET <server>/<topic>/json`, one JSON object per line) on a thread of its
//! own and runs [`cloud_sync::sync_now`] on each message; a message that
//! arrives while a sync runs makes one more run after it. The sync reloads
//! the store, whose change events update the views.
//!
//! A notice is only a hint, and ntfy may drop, delay, or refuse (a rate
//! limit, a 502) with nothing on this side to show it, so while any node runs
//! in the cloud [`start`] also pulls on a timer ([`FALLBACK_POLL`]). A sync
//! reads the orchestrator only, so polling wakes no node's sandbox.
//!
//! When the orchestrator's publishes are refused (ntfy answers 429) it stops
//! publishing for a minute and says so in the feed
//! (`notices_degraded_secs`); until then the app pulls every
//! [`DEGRADED_POLL`], node in the cloud or not, and goes back to the
//! subscription alone after.
//!
//! **Long-poll** ([`spawn_longpoll`]). An orchestrator that knows it
//! (`?wait=`; see `tod_orchestrator::sync_backend::export_changes_waiting`)
//! is pulled from with one request in flight, held until another client's
//! change is committed: no third party in the path, and the orchestrator is
//! awake only while the request is. It is held only while a node runs in the
//! cloud ([`any_running_in_cloud`]); with none there is nothing to wait for,
//! and the app's own syncs (start, each local change) still pull. Once an
//! orchestrator has answered one ([`longpoll_supported`]) the ntfy
//! subscription and the timers stand down: ntfy remains for the phone's
//! alerts, which the orchestrator publishes whatever the app does. An older
//! orchestrator answers a `wait` at once without saying it waited, and the
//! subscription and timers above apply as before.
//!
//! The server and topic come from `GET /users/<u>/notify` on the
//! orchestrator, kept in `cloud-sync.json` (`notify`) and fetched again on
//! every subscription and after every sync ([`refresh_topics`]), so a topic
//! the orchestrator has changed (its data was recreated) is not subscribed to
//! stale.

use crate::cloud_sync::{self, CloudSyncState};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::BufRead;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tod_store::fleet::FleetStore;

/// What the app subscribes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyTopics {
    pub server: String,
    pub topic: String,
    /// For the phone (questions, blocked nodes); the app does not use it.
    #[serde(default)]
    pub alerts_topic: Option<String>,
}

/// How long one subscription is held before it is renewed (it resumes
/// from the last message id, so nothing is missed). Bounds a connection
/// that died without closing.
const RENEW_AFTER: Duration = Duration::from_secs(10 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How often a data root that is not seeded yet is looked at again.
const SEEDED_CHECK: Duration = Duration::from_secs(30);
/// How often the orchestrator is pulled from while a node runs in the
/// cloud, whether or not a notice arrived.
pub const FALLBACK_POLL: Duration = Duration::from_secs(20);
/// How often it is pulled from while the orchestrator says its notices are
/// refused ([`note_degraded`]).
pub const DEGRADED_POLL: Duration = Duration::from_secs(5);

/// Per data root, until when the orchestrator said its notices are refused.
static DEGRADED_UNTIL: Mutex<Option<std::collections::HashMap<std::path::PathBuf, Instant>>> = Mutex::new(None);

/// Records what a feed said of the notice service: `secs` left in a refusal,
/// or `None` for publishing.
fn note_degraded(root: &Path, secs: Option<u64>) {
    let mut map = DEGRADED_UNTIL.lock().unwrap_or_else(|e| e.into_inner());
    let map = map.get_or_insert_with(Default::default);
    match secs {
        Some(secs) => {
            if !map.contains_key(root) {
                tracing::warn!("cloud notices: the orchestrator's notices are refused; pulling every {}s for {secs}s", DEGRADED_POLL.as_secs());
            }
            map.insert(root.to_path_buf(), Instant::now() + Duration::from_secs(secs));
        }
        None => {
            map.remove(root);
        }
    }
}

/// How long one feed long-poll is asked to be held. Under the orchestrator's
/// own cap and any gateway's idle limit.
pub const LONGPOLL_WAIT_SECS: u64 = 25;
/// How often an orchestrator that did not know `wait` is asked again (it may
/// have been updated since).
const LONGPOLL_REPROBE: Duration = Duration::from_secs(10 * 60);
/// How often the long-poll thread looks for a node running in the cloud.
const LONGPOLL_IDLE_CHECK: Duration = Duration::from_secs(10);

/// What this data root last learned of its orchestrator's long-poll.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LongPoll {
    Supported,
    /// Answered a `wait` at once, at this time.
    Unsupported(Instant),
}

static LONGPOLL: Mutex<Option<std::collections::HashMap<std::path::PathBuf, LongPoll>>> = Mutex::new(None);

fn note_longpoll(root: &Path, state: LongPoll) {
    let mut map = LONGPOLL.lock().unwrap_or_else(|e| e.into_inner());
    map.get_or_insert_with(Default::default).insert(root.to_path_buf(), state);
}

fn longpoll_state(root: &Path) -> Option<LongPoll> {
    LONGPOLL.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(root)).copied()
}

/// Whether the orchestrator has answered a feed long-poll for this data root,
/// so the ntfy subscription and the timers are not needed for the app.
pub fn longpoll_supported(root: &Path) -> bool {
    longpoll_state(root) == Some(LongPoll::Supported)
}

/// Whether the long-poll thread should ask now: not while an orchestrator
/// that did not know `wait` was asked less than [`LONGPOLL_REPROBE`] ago.
fn longpoll_due(state: Option<LongPoll>, now: Instant) -> bool {
    match state {
        Some(LongPoll::Unsupported(at)) => now.duration_since(at) >= LONGPOLL_REPROBE,
        _ => true,
    }
}

/// What a long-poll answer means for the loop.
#[derive(Debug, PartialEq, Eq)]
enum Heard {
    /// The orchestrator is too old to wait.
    Unsupported,
    /// Waited and nothing is new.
    Nothing,
    /// Another client's changes are there to sync.
    Changes,
}

fn heard(feed: &cloud_sync::Feed) -> Heard {
    if !feed.waited {
        Heard::Unsupported
    } else if feed.changes.is_empty() {
        Heard::Nothing
    } else {
        Heard::Changes
    }
}

/// Holds the feed long-poll while a node runs in the cloud (see the module
/// docs). A change heard runs a sync through the shared runner and waits for
/// the feed cursor to pass it before asking again, so the same changes are
/// not answered twice.
fn spawn_longpoll(fleet: Arc<FleetStore>) {
    let root = fleet.paths().root().to_path_buf();
    let spawned = std::thread::Builder::new().name("tod-cloud-longpoll".into()).spawn(move || {
        let mut orch: Option<(cloud_sync::HttpOrchestrator, String)> = None;
        let mut backoff = Duration::from_secs(1);
        loop {
            let state = CloudSyncState::load(&root);
            let seeded = state.as_ref().is_ok_and(|s| s.seeded);
            if !seeded || !longpoll_due(longpoll_state(&root), Instant::now()) || !any_running_in_cloud(&fleet) {
                std::thread::sleep(if seeded { LONGPOLL_IDLE_CHECK } else { SEEDED_CHECK });
                continue;
            }
            let after = state.map(|s| s.feed_after).unwrap_or(0);
            let result = (|| {
                if orch.is_none() {
                    orch = Some(cloud_sync::resolve(&root)?);
                }
                let (o, user) = orch.as_ref().expect("resolved above");
                o.pull_waiting(user, after, LONGPOLL_WAIT_SECS)
            })();
            match result {
                Ok(feed) => {
                    backoff = Duration::from_secs(1);
                    note_degraded(&root, feed.notices_degraded_secs);
                    match heard(&feed) {
                        Heard::Unsupported => {
                            tracing::info!("cloud notices: the orchestrator does not hold feed long-polls (it is older than this app); using notices and a timer");
                            note_longpoll(&root, LongPoll::Unsupported(Instant::now()));
                        }
                        Heard::Nothing => note_longpoll(&root, LongPoll::Supported),
                        Heard::Changes => {
                            note_longpoll(&root, LongPoll::Supported);
                            runner(&fleet).request();
                            wait_for_cursor(&root, feed.last_seq);
                        }
                    }
                }
                Err(err) => {
                    // The next try resolves the orchestrator again (its URL may have moved).
                    orch = None;
                    tracing::warn!("cloud notices: feed long-poll: {err:#}; retrying in {}s", backoff.as_secs());
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("cloud notices: could not start the feed long-poll: {err}");
    }
}

/// Waits (at most 30 s) until a sync has moved the feed cursor to `last_seq`.
/// A sync that keeps failing ends the wait, so the long-poll asks again at
/// that pace rather than in a spin.
fn wait_for_cursor(root: &Path, last_seq: i64) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        if CloudSyncState::load(root).is_ok_and(|s| s.feed_after >= last_seq) {
            return;
        }
    }
}

fn degraded(root: &Path) -> bool {
    let map = DEGRADED_UNTIL.lock().unwrap_or_else(|e| e.into_inner());
    map.as_ref().and_then(|m| m.get(root)).is_some_and(|until| *until > Instant::now())
}

/// Whether the timer pulls now: every [`DEGRADED_POLL`] while notices are
/// refused, else every [`FALLBACK_POLL`] while a node runs in the cloud.
fn poll_due(since_last: Duration, degraded: bool, in_cloud: impl FnOnce() -> bool) -> bool {
    if degraded {
        since_last >= DEGRADED_POLL
    } else {
        since_last >= FALLBACK_POLL && in_cloud()
    }
}

/// Whether any node is assigned to the cloud and not lost: its supervisor may
/// be changing it there, so the feed is worth pulling without a notice.
fn any_running_in_cloud(fleet: &FleetStore) -> bool {
    fleet
        .read(|conn| Ok(tod_store::cloud_nodes::list(conn)?.iter().any(|row| row.lost_at.is_none())))
        .unwrap_or_else(|err| {
            tracing::warn!("cloud poll: reading the cloud nodes: {err:#}");
            false
        })
}

/// Pulls on a timer for as long as the app runs: while a node runs in the
/// cloud ([`any_running_in_cloud`]), in case no notice comes, and faster
/// while the orchestrator says its notices are refused.
fn spawn_fallback_poll(fleet: Arc<FleetStore>) {
    let root = fleet.paths().root().to_path_buf();
    let spawned = std::thread::Builder::new().name("tod-cloud-poll".into()).spawn(move || {
        let mut last = Instant::now();
        loop {
            std::thread::sleep(DEGRADED_POLL);
            // The feed long-poll replaces the timers once the orchestrator holds one.
            if longpoll_supported(&root) {
                last = Instant::now();
                continue;
            }
            let due = poll_due(last.elapsed(), degraded(&root), || any_running_in_cloud(&fleet));
            if due && CloudSyncState::load(&root).is_ok_and(|s| s.seeded) {
                last = Instant::now();
                runner(&fleet).request();
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("cloud poll: could not start: {err}");
    }
}

/// One line of ntfy's JSON stream.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StreamEvent {
    #[serde(default)]
    pub id: String,
    /// `open`, `keepalive`, `message`, `poll_request`.
    pub event: String,
}

/// The line's event if it is a message; `None` for keepalives, `open`, and
/// anything unreadable.
pub fn parse_line(line: &str) -> Option<StreamEvent> {
    let event: StreamEvent = serde_json::from_str(line.trim()).ok()?;
    (event.event == "message").then_some(event)
}

/// Holds one subscription until the server closes it (or [`RENEW_AFTER`]),
/// calling `on_message` per message. `since` is the last message id seen,
/// to resume from; returns the last id seen now.
pub fn subscribe_once(
    topics: &NotifyTopics,
    since: Option<&str>,
    on_message: &mut dyn FnMut(),
) -> Result<Option<String>> {
    let mut url = format!("{}/{}/json", topics.server.trim_end_matches('/'), topics.topic);
    if let Some(id) = since {
        url.push_str(&format!("?since={id}"));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(RENEW_AFTER))
        .build()
        .into();
    let resp = agent.get(&url).call().with_context(|| format!("subscribe to {}", topics.server))?;
    let reader = std::io::BufReader::new(resp.into_body().into_reader());
    let mut last = since.map(str::to_string);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if let Some(event) = parse_line(&line) {
            if !event.id.is_empty() {
                last = Some(event.id);
            }
            on_message();
        }
    }
    Ok(last)
}

/// Runs a sync on each [`Coalesced::request`], never two at once: requests
/// during a run make exactly one more run after it.
pub struct Coalesced {
    state: Mutex<(bool, bool)>,
    run: Box<dyn Fn() + Send + Sync>,
}

impl Coalesced {
    pub fn new(run: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { state: Mutex::new((false, false)), run: Box::new(run) })
    }

    /// Starts a run on a thread of its own, or marks one to follow the
    /// current run.
    pub fn request(self: &Arc<Self>) {
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if s.0 {
                s.1 = true;
                return;
            }
            *s = (true, false);
        }
        let this = self.clone();
        std::thread::spawn(move || {
            loop {
                (this.run)();
                let mut s = this.state.lock().unwrap_or_else(|e| e.into_inner());
                if !s.1 {
                    s.0 = false;
                    return;
                }
                s.1 = false;
            }
        });
    }
}

/// How often a sync that keeps failing the same way is logged again.
const REPEAT_FAILURE_LOG: Duration = Duration::from_secs(5 * 60);

/// Keeps a sync that fails every few seconds the same way from filling the
/// log: the first failure and every different one is logged in full, an
/// identical one again only each [`REPEAT_FAILURE_LOG`] (with how many there
/// were), and the end of a run of failures is logged too.
#[derive(Default)]
struct FailureLog {
    last: Option<String>,
    since_logged: u32,
    logged_at: Option<Instant>,
    total: u32,
    escalated_at: Option<Instant>,
}

/// A run of this many failures in a row is raised to an error the user sees
/// (and again at most each [`ESCALATION_REPEAT`]), instead of only counting.
const ESCALATE_AFTER: u32 = 20;
const ESCALATION_REPEAT: Duration = Duration::from_secs(60 * 60);

impl FailureLog {
    /// The line to log for this failure, if it is not a repeat to hold back.
    fn failed(&mut self, text: &str, now: Instant) -> Option<String> {
        self.total += 1;
        let repeat = self.last.as_deref() == Some(text);
        let due = self.logged_at.is_none_or(|t| now.duration_since(t) >= REPEAT_FAILURE_LOG);
        if repeat && !due {
            self.since_logged += 1;
            return None;
        }
        let held = std::mem::take(&mut self.since_logged);
        self.last = Some(text.to_string());
        self.logged_at = Some(now);
        Some(if repeat {
            format!("cloud sync still failing ({} failures in a row, {held} since the last report): {text}", self.total)
        } else {
            format!("cloud sync failed: {text}")
        })
    }

    /// The error to raise (toast and log) once a run of failures passes
    /// [`ESCALATE_AFTER`], then at most hourly; call after [`Self::failed`].
    fn escalate(&mut self, text: &str, now: Instant) -> Option<String> {
        if self.total < ESCALATE_AFTER {
            return None;
        }
        if self.escalated_at.is_some_and(|t| now.duration_since(t) < ESCALATION_REPEAT) {
            return None;
        }
        self.escalated_at = Some(now);
        Some(format!(
            "Cloud sync has failed {} times in a row and is not syncing; last error: {text}",
            self.total
        ))
    }

    /// The line to log when a sync works again after failures.
    fn recovered(&mut self) -> Option<String> {
        let total = std::mem::take(&mut self.total);
        self.last = None;
        self.since_logged = 0;
        self.logged_at = None;
        self.escalated_at = None;
        (total > 0).then(|| format!("cloud sync works again after {total} failures"))
    }
}

/// The one background sync runner for `fleet`'s data root, shared by
/// notices ([`start`]), the app's start, and the outbox push
/// (`cloud_sync::spawn_outbox_pusher`), so their syncs coalesce and never
/// overlap.
pub fn runner(fleet: &Arc<FleetStore>) -> Arc<Coalesced> {
    use std::collections::HashMap;
    use std::path::PathBuf;
    static RUNNERS: Mutex<Option<HashMap<PathBuf, Arc<Coalesced>>>> = Mutex::new(None);
    let root = fleet.paths().root().to_path_buf();
    let mut runners = RUNNERS.lock().unwrap_or_else(|e| e.into_inner());
    runners
        .get_or_insert_with(HashMap::new)
        .entry(root.clone())
        .or_insert_with(|| {
            let fleet = fleet.clone();
            let failures = Mutex::new(FailureLog::default());
            Coalesced::new(move || match cloud_sync::sync_now(&fleet, &root) {
                Ok(report) => {
                    if let Some(note) = failures.lock().unwrap_or_else(|e| e.into_inner()).recovered() {
                        tracing::info!("{note}");
                    }
                    tracing::info!("cloud sync: {}", report.summary());
                    note_degraded(&root, report.notices_degraded_secs);
                    refresh_topics(&root);
                    // The orchestrator found a node's sandbox gone: replace it.
                    if cloud_sync::lost::any_marked(&fleet) {
                        cloud_sync::lost::spawn_check(fleet.clone(), cloud_sync::lost::Check::Marked);
                    }
                }
                Err(err) => {
                    let text = format!("{err:#}");
                    let mut failures = failures.lock().unwrap_or_else(|e| e.into_inner());
                    let now = Instant::now();
                    if let Some(line) = failures.failed(&text, now) {
                        tracing::warn!("{line}");
                    }
                    if let Some(line) = failures.escalate(&text, now) {
                        // An error-level entry is what raises a toast.
                        crate::logging::buffer::record(
                            crate::logging::buffer::Level3::Error,
                            crate::logging::buffer::LogSource::App,
                            line,
                        );
                    }
                }
            })
        })
        .clone()
}

/// Whether the orchestrator's topics differ from the ones saved: its data was
/// recreated (a new secret), and what is saved is a dead topic.
fn topics_changed(saved: Option<&NotifyTopics>, fresh: &NotifyTopics) -> bool {
    saved != Some(fresh)
}

/// The topics for `root` as the orchestrator says now (`GET /notify`), saved
/// in `cloud-sync.json` when they differ from what is there. Never the saved
/// copy alone: a stale one subscribes to a topic nothing is published to.
fn topics(root: &Path) -> Result<NotifyTopics> {
    let (orch, user) = cloud_sync::resolve(root)?;
    let fresh: NotifyTopics = serde_json::from_value(orch.get_json(&user, "notify")?)?;
    save_topics(root, &fresh)?;
    Ok(fresh)
}

fn save_topics(root: &Path, fresh: &NotifyTopics) -> Result<()> {
    // A sync holds this lock from loading the state to saving it; without
    // it, one already running would save its older copy over `notify`.
    let _guard = cloud_sync::sync_lock(root)?;
    let mut state = CloudSyncState::load(root)?;
    if topics_changed(state.notify.as_ref(), fresh) {
        if let Some(old) = &state.notify {
            tracing::warn!("cloud notices: the orchestrator's topic changed ({} -> {}); using the new one", old.topic, fresh.topic);
        }
        state.notify = Some(fresh.clone());
        state.save(root)?;
    }
    Ok(())
}

/// After a sync: fetches the topics again when this data root has any saved
/// (so one that subscribes, or did, notices a changed topic at its next
/// renewal). A failure is logged, not dropped.
fn refresh_topics(root: &Path) {
    if longpoll_supported(root) || !CloudSyncState::load(root).is_ok_and(|s| s.notify.is_some()) {
        return;
    }
    if let Err(err) = topics(root) {
        tracing::warn!("cloud notices: could not re-read the topic: {err:#}");
    }
}

/// Subscribes for as long as the app runs, once this data root is seeded.
/// Never blocks the caller; for the app's start.
pub fn start(fleet: Arc<FleetStore>) {
    spawn_fallback_poll(fleet.clone());
    spawn_longpoll(fleet.clone());
    let root = fleet.paths().root().to_path_buf();
    let spawned = std::thread::Builder::new().name("tod-cloud-notify".into()).spawn(move || {
        while !CloudSyncState::load(&root).is_ok_and(|s| s.seeded) {
            std::thread::sleep(SEEDED_CHECK);
        }
        let sync = runner(&fleet);
        let mut backoff = Duration::from_secs(1);
        let mut since: Option<String> = None;
        loop {
            // The orchestrator holds a feed long-poll: the app needs no
            // subscription (ntfy is for the phone's alerts).
            if longpoll_supported(&root) {
                std::thread::sleep(LONGPOLL_IDLE_CHECK);
                continue;
            }
            let result = topics(&root).and_then(|t| {
                // A (re)connect may have missed messages; pull once to be sure.
                sync.request();
                subscribe_once(&t, since.as_deref(), &mut || sync.request())
            });
            match result {
                Ok(last) => {
                    since = last;
                    backoff = Duration::from_secs(1);
                    // A server that closes at once is not reconnected to in a spin.
                    std::thread::sleep(backoff);
                }
                Err(err) => {
                    tracing::warn!("cloud notices: {err:#}; retrying in {}s", backoff.as_secs());
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("cloud notices: could not start: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_timer_is_fast_only_while_notices_are_refused() {
        let s = Duration::from_secs;
        assert!(!poll_due(s(5), false, || true));
        assert!(poll_due(s(20), false, || true));
        assert!(!poll_due(s(20), false, || false), "no node in the cloud, notices working: no timer");
        assert!(poll_due(s(5), true, || false), "refused: pull even with no node in the cloud");
        assert!(!poll_due(s(4), true, || true));
    }

    fn feed(waited: bool, changes: usize) -> cloud_sync::Feed {
        let change = || tod_store::sync::Change {
            seq: 1,
            node_id: None,
            table: "t".into(),
            key: Default::default(),
            before: None,
            after: None,
        };
        cloud_sync::Feed {
            last_seq: 5,
            changes: (0..changes).map(|_| change()).collect(),
            seed_id: None,
            notices_degraded_secs: None,
            waited,
        }
    }

    #[test]
    fn a_long_poll_answer_is_told_from_an_old_orchestrators_by_waited() {
        assert_eq!(heard(&feed(false, 0)), Heard::Unsupported);
        assert_eq!(heard(&feed(false, 2)), Heard::Unsupported);
        assert_eq!(heard(&feed(true, 0)), Heard::Nothing);
        assert_eq!(heard(&feed(true, 1)), Heard::Changes);
    }

    #[test]
    fn an_orchestrator_without_long_polls_is_asked_again_only_after_a_while() {
        let now = Instant::now();
        assert!(longpoll_due(None, now));
        assert!(longpoll_due(Some(LongPoll::Supported), now));
        assert!(!longpoll_due(Some(LongPoll::Unsupported(now)), now + Duration::from_secs(60)));
        assert!(longpoll_due(Some(LongPoll::Unsupported(now)), now + LONGPOLL_REPROBE));
    }

    #[test]
    fn a_changed_topic_is_noticed() {
        let t = |topic: &str| NotifyTopics { server: "s".into(), topic: topic.into(), alerts_topic: None };
        assert!(topics_changed(None, &t("a")));
        assert!(!topics_changed(Some(&t("a")), &t("a")));
        assert!(topics_changed(Some(&t("a")), &t("b")));
    }

    /// Serves `replies` (status, body), one per connection; returns the port
    /// and the request lines it saw.
    fn stub(replies: Vec<(u16, String)>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in replies {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string());
                write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            seen
        });
        (port, handle)
    }

    #[test]
    fn pull_waiting_asks_for_a_wait_and_reports_whether_it_was_held() {
        let waited = r#"{"last_seq":7,"changes":[],"seed_id":"s","waited":true}"#.to_string();
        let old = r#"{"last_seq":7,"changes":[],"seed_id":"s"}"#.to_string();
        let (port, server) = stub(vec![(200, waited), (200, old)]);
        let orch = cloud_sync::HttpOrchestrator::new(format!("http://127.0.0.1:{port}"), None);
        let new = orch.pull_waiting("u", 3, 25).unwrap();
        assert!(new.waited && new.changes.is_empty() && new.last_seq == 7);
        let old = orch.pull_waiting("u", 3, 25).unwrap();
        assert!(!old.waited);
        let seen = server.join().unwrap();
        assert!(seen[0].starts_with("GET /users/u/changes?after=3&wait=25 "), "{seen:?}");
    }

    #[test]
    fn pull_waiting_keeps_the_gateway_retry() {
        let ok = r#"{"last_seq":1,"changes":[],"waited":true}"#.to_string();
        let (port, server) = stub(vec![(502, "bad gateway".into()), (503, "waking".into()), (200, ok)]);
        let orch = cloud_sync::HttpOrchestrator::new(format!("http://127.0.0.1:{port}"), None);
        let feed = orch.pull_waiting("u", 0, 1).unwrap();
        assert!(feed.waited);
        assert_eq!(server.join().unwrap().len(), 3);
    }

    #[test]
    fn a_refusal_is_remembered_until_a_feed_says_it_is_over() {
        let root = std::path::PathBuf::from("/nowhere/degraded-test");
        assert!(!degraded(&root));
        note_degraded(&root, Some(60));
        assert!(degraded(&root));
        note_degraded(&root, None);
        assert!(!degraded(&root));
    }
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn only_message_lines_count() {
        assert_eq!(
            parse_line(r#"{"id":"a1","time":1,"event":"message","topic":"t","message":"changed"}"#),
            Some(StreamEvent { id: "a1".into(), event: "message".into() })
        );
        assert_eq!(parse_line(r#"{"id":"k","time":1,"event":"keepalive","topic":"t"}"#), None);
        assert_eq!(parse_line(r#"{"id":"o","time":1,"event":"open","topic":"t"}"#), None);
        assert_eq!(parse_line("not json"), None);
        assert_eq!(parse_line(""), None);
    }

    #[test]
    fn subscribe_reads_the_stream_from_a_stub_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap();
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = concat!(
                r#"{"id":"o1","time":1,"event":"open","topic":"tod-x"}"#, "\n",
                r#"{"id":"m1","time":2,"event":"message","topic":"tod-x","message":"changed"}"#, "\n",
                r#"{"id":"k1","time":3,"event":"keepalive","topic":"tod-x"}"#, "\n",
                r#"{"id":"m2","time":4,"event":"message","topic":"tod-x","message":"changed"}"#, "\n",
            );
            write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            head
        });
        let topics = NotifyTopics { server: format!("http://127.0.0.1:{port}"), topic: "tod-x".into(), alerts_topic: None };
        let mut n = 0;
        let last = subscribe_once(&topics, Some("m0"), &mut || n += 1).unwrap();
        assert_eq!(n, 2);
        assert_eq!(last.as_deref(), Some("m2"));
        let head = server.join().unwrap();
        assert!(head.starts_with("GET /tod-x/json?since=m0 "), "{head}");
    }

    #[test]
    fn requests_during_a_run_make_one_more_run() {
        let runs = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let c = {
            let (runs, gate) = (runs.clone(), gate.clone());
            Coalesced::new(move || {
                let _g = gate.lock().unwrap();
                runs.fetch_add(1, Ordering::SeqCst);
            })
        };
        c.request();
        std::thread::sleep(Duration::from_millis(50));
        for _ in 0..5 {
            c.request();
        }
        drop(held);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while c.state.lock().unwrap().0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn a_repeated_failure_is_logged_once_per_interval() {
        let t0 = Instant::now();
        let mut log = FailureLog::default();
        assert!(log.failed("502", t0).unwrap().starts_with("cloud sync failed: 502"));
        assert_eq!(log.failed("502", t0 + Duration::from_secs(30)), None);
        assert_eq!(log.failed("502", t0 + Duration::from_secs(60)), None);
        // A different failure is always reported.
        assert!(log.failed("timeout", t0 + Duration::from_secs(90)).unwrap().starts_with("cloud sync failed: timeout"));
        assert_eq!(log.failed("timeout", t0 + Duration::from_secs(120)), None);
        let again = log.failed("timeout", t0 + Duration::from_secs(90) + REPEAT_FAILURE_LOG).unwrap();
        assert!(again.contains("still failing") && again.contains("1 since the last report"), "{again}");
        assert!(log.recovered().unwrap().contains("works again after 6 failures"));
        assert_eq!(log.recovered(), None);
    }

    #[test]
    fn a_long_run_of_failures_escalates_once_then_hourly() {
        let t0 = Instant::now();
        let mut log = FailureLog::default();
        let mut raised = Vec::new();
        for i in 0..ESCALATE_AFTER + 30 {
            let now = t0 + Duration::from_secs(i as u64);
            log.failed("500", now);
            raised.extend(log.escalate("500", now));
        }
        assert_eq!(raised.len(), 1, "{raised:?}");
        assert!(raised[0].contains("20 times in a row") && raised[0].contains("500"), "{raised:?}");
        // An hour later it is raised again, with the new count.
        let later = t0 + Duration::from_secs(3600 + 100);
        log.failed("500", later);
        assert!(log.escalate("500", later).unwrap().contains("51 times"));
        log.recovered();
        assert_eq!(log.escalate("500", later), None);
    }
}
