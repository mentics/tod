//! The app's side of cloud sync and of running a node in the cloud
//! (`doc/cloud-sandboxes/autonomous-nodes.md`, "Sync with the app").
//!
//! - [`sync`]: seed the user's database on the orchestrator if it has not
//!   been, send the outbox (the app's own `sync_changes` after the last one
//!   the orchestrator accepted), then pull the feed after the last cursor and
//!   apply it. Changes applied from the feed are not logged
//!   (`tod_store::sync::apply_changes` suppresses its triggers), so they are
//!   never sent back.
//! - [`run_in_cloud`]: accept a node: sync (seeding first), create and
//!   provision the node's own sandbox (`tod_sandbox::node`), poke it, and
//!   record that the node runs there.
//!
//! Both block on the network: never call them on the UI thread.
//!
//! - [`spawn_outbox_pusher`]: sends the outbox a moment after each change,
//!   in the background, so running nodes see the user's edits.
//!
//! The cursors and the orchestrator's URL and user override are kept in
//! [`STATE_FILE`] under the data root. Which nodes run in the cloud is in
//! the synced `cloud_nodes` table (`tod_store::cloud_nodes`); older builds
//! kept it in the state file, which the start-up check ([`lost`]) moves into
//! the table.
//!
//! - [`lost`]: on start, and after a sync that brings a lost mark, replace
//!   node sandboxes that are gone; leaving the cloud; retiring done nodes.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tod_store::credentials::{CredentialKind, CredentialStore};
use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::sync::{self, ApplyReport, Change};

/// The app's cloud-sync record, under the data root.
pub const STATE_FILE: &str = "cloud-sync.json";

/// Overrides the orchestrator's URL (e.g. `http://127.0.0.1:8090` in tests).
pub const ORCHESTRATOR_URL_ENV: &str = "TOD_ORCHESTRATOR_URL";

/// The agent a node's supervisor runs (`claude`, the default, or `mock`),
/// for sandboxes created from here on: testing a node end to end in the
/// cloud without a Claude subscription.
pub const CLOUD_AGENT_ENV: &str = "TOD_CLOUD_AGENT";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudSyncState {
    /// The orchestrator's base URL; else [`ORCHESTRATOR_URL_ENV`], else the
    /// orchestrator sandbox's `/port/8090` (`sandboxes.toml`'s `orchestrator`).
    #[serde(default)]
    pub orchestrator_url: Option<String>,
    /// The user name on the orchestrator; else the Blaxel account's owner,
    /// else the OS user.
    #[serde(default)]
    pub user: Option<String>,
    /// The orchestrator holds a copy of this database.
    #[serde(default)]
    pub seeded: bool,
    /// The last local `sync_changes.seq` the orchestrator accepted.
    #[serde(default)]
    pub sent_after: i64,
    /// The feed's cursor: the orchestrator's `last_seq` last pulled.
    #[serde(default)]
    pub feed_after: i64,
    /// Nodes running in the cloud, by node id.
    #[serde(default)]
    pub nodes: BTreeMap<String, CloudNode>,
    /// This data root's sync client id on the orchestrator (`app-<id>`),
    /// made on first use by [`client_id`].
    #[serde(default)]
    pub client_id: Option<String>,
    /// Where the orchestrator announces changes ([`crate::cloud_notify`]).
    #[serde(default)]
    pub notify: Option<crate::cloud_notify::NotifyTopics>,
    /// How far the orchestrator's problem log has been read
    /// ([`record_remote_problems`]); `None` until the first sync, which
    /// starts from now.
    #[serde(default)]
    pub problems_after: Option<tod_problems::Cursor>,
}

/// This data root's sync client id, made and saved on first use.
pub fn client_id(root: &Path) -> Result<String> {
    let mut state = CloudSyncState::load(root)?;
    if let Some(id) = state.client_id.clone() {
        return Ok(id);
    }
    let id = format!("app-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    state.client_id = Some(id.clone());
    state.save(root)?;
    Ok(id)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudNode {
    pub sandbox: String,
    pub user: String,
    pub accepted_at_ms: i64,
    /// When the orchestrator found its sandbox gone, until the app replaces it.
    #[serde(default)]
    pub lost_at: Option<i64>,
}

impl From<tod_store::cloud_nodes::CloudNodeRow> for CloudNode {
    fn from(row: tod_store::cloud_nodes::CloudNodeRow) -> Self {
        Self { sandbox: row.sandbox, user: row.user, accepted_at_ms: row.accepted_at, lost_at: row.lost_at }
    }
}

impl CloudSyncState {
    pub fn path(root: &Path) -> PathBuf {
        root.join(STATE_FILE)
    }

    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::path(root);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).with_context(|| format!("read {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = Self::path(root);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("write {}", path.display()))
    }
}

/// Where the node runs in the cloud, if it does: its `cloud_nodes` row
/// (synced, so every copy of the app sees it), read through the store's
/// already-open read connection, so fine on the UI thread (no database is
/// opened). Records older builds kept in [`STATE_FILE`] are moved into the
/// table by the start-up check ([`lost::adopt_legacy_records`]).
pub fn cloud_node(fleet: &FleetStore, node_id: &str) -> Option<CloudNode> {
    let node = uuid::Uuid::parse_str(node_id).ok()?;
    fleet.read(|conn| tod_store::cloud_nodes::get(conn, node)).ok()?.map(CloudNode::from)
}

/// A node whose supervisor is working in the cloud now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudWork {
    pub node_id: uuid::Uuid,
    pub title: String,
    pub lifecycle: String,
}

/// The cloud nodes that are working now, as far as the synced data says. The
/// supervisor reports no heartbeat, so this is derived: a node assigned to the
/// cloud (not lost, not `done`) is working unless it is waiting on something
/// (a pending `waits` row: a review, CI, a time) or on the user (a pending
/// request). Being assigned to the cloud alone is not work: such a node can
/// sit idle for days. A supervisor that died mid-turn still reads as working.
///
/// Reads the store's own connection, so call it off the UI thread.
pub fn working_in_cloud(fleet: &FleetStore) -> Result<Vec<CloudWork>> {
    let candidates = fleet.read(|conn| {
        let rows: Vec<_> = tod_store::cloud_nodes::list(conn)?
            .into_iter()
            .filter(|row| row.lost_at.is_none())
            .collect();
        let ids: Vec<uuid::Uuid> = rows.iter().map(|row| row.node_id).collect();
        let attention = crate::attention::for_nodes(conn, &ids)?;
        let waits = tod_store::waits::WaitRepo::new(conn);
        let mut free = Vec::new();
        for id in ids {
            if attention.get(&id).is_some_and(|a| a.count > 0) || !waits.list_pending_for_node(id)?.is_empty() {
                continue;
            }
            free.push(id);
        }
        Ok(free)
    })?;
    let mut working = Vec::new();
    for node_id in candidates {
        let Some(node) = fleet.get_node(&node_id.to_string())? else { continue };
        if node.lifecycle == "done" {
            continue;
        }
        let title = if node.title.trim().is_empty() { node.slug.clone() } else { node.title.clone() };
        working.push(CloudWork { node_id, title, lifecycle: node.lifecycle });
    }
    Ok(working)
}

/// Held by every [`sync`] in this process. Background syncs (the outbox
/// push, notices, the start) go through one coalescing runner
/// ([`crate::cloud_notify::runner`]); this lock also keeps the user's own
/// "Sync now" and [`run_in_cloud`] from overlapping one.
static SYNC_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn sync_lock() -> std::sync::MutexGuard<'static, ()> {
    SYNC_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A user name the orchestrator accepts: letters, digits, `-`, `_`, `.`,
/// not starting with `.` or `-`, at most 64.
pub fn user_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' })
        .collect();
    let cleaned = cleaned.trim_start_matches(['.', '-']);
    let cleaned: String = cleaned.chars().take(64).collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// A node's sandbox name: `node-<slug>`, as a Blaxel name allows.
pub fn node_sandbox_name(slug: &str) -> String {
    let base = tod_store::fleet::sandbox::label_value(slug);
    let base: String = base.chars().take(50).collect();
    let base = base.trim_matches('-');
    if base.is_empty() { "node".into() } else { format!("node-{base}") }
}

/// An HTTPS URL for a git remote (`git@host:o/r.git`, `ssh://git@host/o/r`),
/// since git in a node's sandbox goes through the proxy over HTTPS.
pub fn https_repo_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if remote.starts_with("https://") {
        return Some(remote.to_string());
    }
    if let Some(rest) = remote.strip_prefix("ssh://") {
        let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
        let (host, path) = rest.split_once('/')?;
        let host = host.split(':').next()?;
        return Some(format!("https://{host}/{path}"));
    }
    if let Some((user_host, path)) = remote.split_once(':')
        && !user_host.contains('/')
        && !path.starts_with("//")
        && user_host.contains('@')
    {
        let host = user_host.rsplit_once('@').map_or(user_host, |(_, h)| h);
        return Some(format!("https://{host}/{path}"));
    }
    None
}

/// How the app reaches the orchestrator.
pub trait Orchestrator {
    /// `POST /users/<u>/seed`: the new feed cursor.
    fn seed(&self, user: &str, snapshot: &[u8]) -> Result<i64>;
    /// `POST /users/<u>/changes`.
    fn send(&self, user: &str, changes: &[Change]) -> Result<ApplyReport>;
    /// `GET /users/<u>/changes?after=n`: `(last_seq, changes)`.
    fn pull(&self, user: &str, after: i64) -> Result<(i64, Vec<Change>)>;
    /// `POST /users/<u>/problems/pull`: what the cloud's executables reported
    /// after `cursor` (`None`: from now), and the new cursor. Only ever
    /// called inside a sync, so it adds no contact with the cloud of its own.
    fn pull_problems(
        &self,
        user: &str,
        cursor: Option<&tod_problems::Cursor>,
    ) -> Result<(tod_problems::Cursor, Vec<tod_problems::Problem>)>;
}

/// The orchestrator over HTTP(S).
pub struct HttpOrchestrator {
    base: String,
    token: Option<String>,
    /// This app as a sync client (`X-Tod-Client`): what it sends reaches the
    /// node supervisors and never comes back to it.
    client: String,
    agent: ureq::Agent,
}

impl HttpOrchestrator {
    pub fn new(base: impl Into<String>, token: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(300)))
            .build()
            .into();
        Self { base: base.into().trim_end_matches('/').to_string(), token, client: "app".into(), agent }
    }

    /// Names this app as the sync client `client` ([`client_id`]).
    pub fn with_client(mut self, client: impl Into<String>) -> Self {
        self.client = client.into();
        self
    }

    /// `GET /users/<user>/<rest>` as JSON.
    pub fn get_json(&self, user: &str, rest: &str) -> Result<serde_json::Value> {
        let resp = self.auth(self.agent.get(&self.url(user, rest))).call().with_context(|| format!("orchestrator {rest}"))?;
        Self::finish(resp, rest)
    }

    fn url(&self, user: &str, rest: &str) -> String {
        format!("{}/users/{user}/{rest}", self.base)
    }

    fn auth<B>(&self, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        let req = req.header(sync::CLIENT_HEADER, &self.client);
        match &self.token {
            Some(t) => req.header("Authorization", &format!("Bearer {t}")),
            None => req,
        }
    }

    fn finish(mut resp: ureq::http::Response<ureq::Body>, what: &str) -> Result<serde_json::Value> {
        let status = resp.status();
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        if !status.is_success() {
            let body: String = body.chars().take(400).collect();
            bail!("orchestrator {what}: {status}: {body}");
        }
        serde_json::from_str(&body).with_context(|| format!("orchestrator {what}: not JSON"))
    }
}

impl Orchestrator for HttpOrchestrator {
    fn seed(&self, user: &str, snapshot: &[u8]) -> Result<i64> {
        // The orchestrator answers 409 while the user's store is in use.
        for attempt in 0..5 {
            let resp = self
                .auth(self.agent.post(&self.url(user, "seed")))
                .header("Content-Type", "application/octet-stream")
                .send(snapshot)
                .context("orchestrator seed")?;
            if resp.status().as_u16() == 409 && attempt < 4 {
                std::thread::sleep(Duration::from_millis(500 * (attempt + 1)));
                continue;
            }
            let v = Self::finish(resp, "seed")?;
            return v["last_seq"].as_i64().ok_or_else(|| anyhow!("orchestrator seed: no last_seq"));
        }
        unreachable!()
    }

    fn send(&self, user: &str, changes: &[Change]) -> Result<ApplyReport> {
        let resp = self
            .auth(self.agent.post(&self.url(user, "changes")))
            .send_json(changes)
            .context("orchestrator changes")?;
        let v = Self::finish(resp, "changes").map_err(older_orchestrator_hint)?;
        Ok(serde_json::from_value(v)?)
    }

    fn pull(&self, user: &str, after: i64) -> Result<(i64, Vec<Change>)> {
        let resp = self
            .auth(self.agent.get(&self.url(user, &format!("changes?after={after}"))))
            .call()
            .context("orchestrator feed")?;
        let mut v = Self::finish(resp, "feed")?;
        let last = v["last_seq"].as_i64().ok_or_else(|| anyhow!("orchestrator feed: no last_seq"))?;
        let changes = serde_json::from_value(v["changes"].take())?;
        Ok((last, changes))
    }

    fn pull_problems(
        &self,
        user: &str,
        cursor: Option<&tod_problems::Cursor>,
    ) -> Result<(tod_problems::Cursor, Vec<tod_problems::Problem>)> {
        let resp = self
            .auth(self.agent.post(&self.url(user, "problems/pull")))
            .send_json(serde_json::json!({ "cursor": cursor }))
            .context("orchestrator problems")?;
        let v = Self::finish(resp, "problems")?;
        let cursor = serde_json::from_value(v["cursor"].clone()).context("orchestrator problems: cursor")?;
        let problems = v["problems"].as_array().map(|a| a.iter().filter_map(tod_problems::from_json).collect()).unwrap_or_default();
        Ok((cursor, problems))
    }
}

/// Put what the cloud's executables reported into the log panel (and, for
/// errors, the toast), labelled with the node or sandbox they concern.
pub fn record_remote_problems(problems: &[tod_problems::Problem]) {
    use crate::logging::buffer::{Level3, LogSource, record};
    for p in problems {
        let level = match p.level {
            tod_problems::Level::Error => Level3::Error,
            tod_problems::Level::Warn => Level3::Warn,
        };
        let label = p.label.as_deref().unwrap_or("cloud");
        record(level, LogSource::for_process(&p.process), format!("[{label}] {}", p.message));
    }
}

/// What one [`sync`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncReport {
    pub seeded: bool,
    pub sent: usize,
    pub received: usize,
    /// Rows both sides changed (the app's version won going up; the
    /// orchestrator's coming down).
    pub conflicts: usize,
}

impl SyncReport {
    pub fn summary(&self) -> String {
        let mut s = format!("Synced: sent {}, received {}", self.sent, self.received);
        if self.seeded {
            s = format!("Seeded the orchestrator. {s}");
        }
        if self.conflicts > 0 {
            s.push_str(&format!(", {} conflicting", self.conflicts));
        }
        s
    }
}

fn connect(db: &Path) -> Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(db).with_context(|| format!("open {}", db.display()))?;
    conn.busy_timeout(Duration::from_secs(30))?;
    Ok(conn)
}

/// Seed if not seeded, send the outbox, pull and apply the feed; saves the
/// cursors after each step, so a failure part way loses nothing.
pub fn sync(fleet: &FleetStore, root: &Path, orch: &dyn Orchestrator, user: &str) -> Result<SyncReport> {
    let _guard = sync_lock();
    let mut state = CloudSyncState::load(root)?;
    let mut report = SyncReport::default();
    let _ = fleet.flush_on_quit();
    let db = fleet.paths().db().to_path_buf();

    if !state.seeded {
        let tmp = root.join("cloud-seed-snapshot.db");
        let _ = std::fs::remove_file(&tmp);
        sync::snapshot(&db, &tmp).context("snapshot the database")?;
        let result = (|| {
            let snap_seq = sync::last_seq(&connect(&tmp)?)?;
            let bytes = std::fs::read(&tmp)?;
            let feed = orch.seed(user, &bytes)?;
            Ok::<_, anyhow::Error>((snap_seq, feed))
        })();
        let _ = std::fs::remove_file(&tmp);
        let (snap_seq, feed) = result?;
        state.seeded = true;
        state.sent_after = snap_seq;
        state.feed_after = feed;
        state.save(root)?;
        report.seeded = true;
    }

    let outbox = sync::export_changes(&connect(&db)?, state.sent_after)?;
    if !outbox.is_empty() {
        let applied = orch.send(user, &outbox)?;
        report.sent = outbox.len();
        report.conflicts += applied.conflicts.len();
        state.sent_after = outbox.iter().map(|c| c.seq).max().unwrap_or(state.sent_after);
        state.save(root)?;
    }

    let (last, feed) = orch.pull(user, state.feed_after)?;
    if !feed.is_empty() {
        let mut conn = connect(&db)?;
        let applied = sync::apply_changes(&mut conn, &feed)?;
        drop(conn);
        report.received = feed.len();
        report.conflicts += applied.conflicts.len();
        let _ = fleet.reload_if_stale();
    }
    state.feed_after = last;
    state.save(root)?;

    // Problems the cloud's executables pushed up, read on this same sync:
    // no extra contact with the cloud, so no sandbox is woken for them. A
    // failure is reported, not dropped, and the cursor stays for the retry.
    match orch.pull_problems(user, state.problems_after.as_ref()) {
        Ok((cursor, problems)) => {
            record_remote_problems(&problems);
            state.problems_after = Some(cursor);
            state.save(root)?;
        }
        Err(err) => warn_problem_log_unreadable(&err),
    }
    drop(_guard);
    if let Err(err) = lost::retire_done(fleet) {
        tracing::warn!("cloud sync: retiring done nodes: {err:#}");
    }
    Ok(report)
}

/// An orchestrator deployed before `problems/pull` existed answers 404 on every
/// sync, and nothing the user does changes that until it is redeployed. That
/// case is reported once per process, as a toast saying what to do; any other failure is
/// reported every time, since the retry may differ.
fn warn_problem_log_unreadable(err: &anyhow::Error) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED_MISSING: AtomicBool = AtomicBool::new(false);
    if format!("{err:#}").contains("orchestrator problems: 404") {
        if !WARNED_MISSING.swap(true, Ordering::Relaxed) {
            // An error-level entry is what raises a toast.
            crate::logging::buffer::record(
                crate::logging::buffer::Level3::Error,
                crate::logging::buffer::LogSource::App,
                "The cloud orchestrator is out of date: it cannot send its problem log (404 on problems/pull). \
                 Redeploy it with scripts/redeploy-orchestrator.sh --data-root <your data root> to see the cloud's problems here. Syncing otherwise works; this is not repeated until tod restarts.",
            );
        }
    } else {
        tracing::warn!("cloud sync: reading the cloud's problem log: {err:#}");
    }
}

/// How long the store must be quiet before the outbox goes out.
pub const PUSH_DEBOUNCE: Duration = Duration::from_secs(2);

/// On a thread of its own: a sync through the shared runner
/// ([`crate::cloud_notify::runner`]) [`PUSH_DEBOUNCE`] after the last store
/// change, for as long as the store lives, so the user's edits reach running
/// nodes. A data root that was never seeded makes no network call. The sync
/// the push starts reloads the store only when it received something, and
/// an empty outbox sends nothing.
pub fn spawn_outbox_pusher(fleet: std::sync::Arc<FleetStore>) {
    let mut rx = fleet.subscribe_changes();
    let root = fleet.paths().root().to_path_buf();
    let spawned = std::thread::Builder::new().name("tod-cloud-outbox".into()).spawn(move || {
        use tokio::sync::broadcast::error::{RecvError, TryRecvError};
        loop {
            match rx.blocking_recv() {
                Ok(()) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
            // Wait until a whole debounce passes with no change.
            loop {
                std::thread::sleep(PUSH_DEBOUNCE);
                let mut more = false;
                loop {
                    match rx.try_recv() {
                        Ok(()) | Err(TryRecvError::Lagged(_)) => more = true,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Closed) => return,
                    }
                }
                if !more {
                    break;
                }
            }
            if !CloudSyncState::load(&root).is_ok_and(|s| s.seeded) {
                continue;
            }
            // Nothing new of ours to send: no call (the store also changes
            // when a sync applies what it received).
            let pending = CloudSyncState::load(&root).ok().and_then(|state| {
                let conn = connect(fleet.paths().db()).ok()?;
                Some(sync::last_seq(&conn).ok()? > state.sent_after)
            });
            if pending == Some(true) {
                crate::cloud_notify::runner(&fleet).request();
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("cloud outbox pusher did not start: {err}");
    }
}

/// The orchestrator and user this data root syncs with: the URL (from the
/// state file, [`ORCHESTRATOR_URL_ENV`], or the workspace's
/// orchestrator sandbox, `sandboxes.toml`'s `orchestrator`) and, for a sandbox URL, the Blaxel token.
pub fn resolve(root: &Path) -> Result<(HttpOrchestrator, String)> {
    let state = CloudSyncState::load(root)?;
    let sandboxes = tod_store::fleet::sandbox::Sandboxes::load(root);
    let owner = sandboxes.as_ref().ok().and_then(|s| s.config.blaxel.as_ref()).and_then(|a| a.owner.clone());
    let user = state
        .user
        .clone()
        .or(owner)
        .or_else(|| std::env::var("USERNAME").or_else(|_| std::env::var("USER")).ok())
        .and_then(|u| user_name(&u))
        .ok_or_else(|| anyhow!("no user name for the orchestrator: set `user` in {STATE_FILE}"))?;
    let explicit = state.orchestrator_url.clone().or_else(|| std::env::var(ORCHESTRATOR_URL_ENV).ok());
    if let Some(url) = explicit {
        let token = if url.starts_with("https://") {
            sandboxes.ok().and_then(|s| s.token().ok())
        } else {
            None
        };
        return Ok((HttpOrchestrator::new(url, token).with_client(client_id(root)?), user));
    }
    let mut sandboxes = sandboxes?;
    let bx = sandboxes.blaxel()?;
    let name = sandboxes.account()?.orchestrator.clone();
    let url = sandboxes
        .url(&bx, &name)
        .context("find the orchestrator (run `tod-sandbox orchestrator` first)")?;
    let base = format!("{}/port/{}", url.trim_end_matches('/'), tod_sandbox::orchestrator::PORT);
    Ok((HttpOrchestrator::new(base, Some(bx.token().to_string())).with_client(client_id(root)?), user))
}

/// [`sync`] with the resolved orchestrator: the "Sync with the cloud" action.
pub fn sync_now(fleet: &FleetStore, root: &Path) -> Result<SyncReport> {
    let (orch, user) = resolve(root)?;
    sync(fleet, root, &orch, &user)
}

/// Sync on a thread of its own, when this data root has been seeded (a data
/// root that never sent a node to the cloud makes no network call). For the
/// app's start.
///
/// Also starts [`spawn_outbox_pusher`], whether or not the root is seeded
/// yet (it checks before each push).
pub fn sync_on_start(fleet: std::sync::Arc<FleetStore>) {
    spawn_outbox_pusher(fleet.clone());
    let root = fleet.paths().root().to_path_buf();
    // Whether or not anything is seeded: a workspace with no orchestrator
    // gets one, in the background.
    deploy::spawn_ensure(root.clone());
    if !CloudSyncState::load(&root).is_ok_and(|s| s.seeded) {
        return;
    }
    // Sync, then check every cloud node's sandbox against Blaxel and
    // replace any that is gone.
    lost::spawn_check(fleet, lost::Check::Full);
}

/// The user's git identity, for the node's commits in its sandbox: the
/// node's repository's own when it is on this machine, else this machine's
/// global one, else [`tod_sandbox::node::GitIdentity::fallback`].
fn git_identity(repo: Option<&str>) -> tod_sandbox::node::GitIdentity {
    let local = repo.map(Path::new).filter(|dir| dir.is_dir());
    let get = |key: &str| {
        let mut cmd = std::process::Command::new("git");
        if let Some(dir) = local {
            cmd.arg("-C").arg(dir);
        }
        cmd.args(["config", "--get", key])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|value| !value.is_empty())
    };
    match (get("user.name"), get("user.email")) {
        (Some(name), Some(email)) => tod_sandbox::node::GitIdentity { name, email },
        _ => tod_sandbox::node::GitIdentity::fallback(),
    }
}

/// The node's Environment credentials for its sandbox's proxy: secrets with
/// hosts (the agent never has the value). Variables reach the agent through
/// its context, not the sandbox. Unset secrets are left out.
pub(crate) fn environment_credentials(
    fleet: &FleetStore,
    creds: &CredentialStore,
    node_id: &str,
) -> Result<Vec<tod_sandbox::node::CustomCredential>> {
    let node: uuid::Uuid = node_id.parse().context("node id")?;
    fleet.read(|conn| tod_store::environment::sandbox_credentials(conn, creds, node))
}

/// `git config` pattern listing every remote's URL (`remote.<name>.url <url>`).
const REMOTE_URLS: &str = r"^remote\..*\.url$";

/// The HTTPS URL of a repository's remote, from `git config --get-regexp
/// REMOTE_URLS` output: `origin` when it has one, else the first remote
/// (a checkout may name its remote `fs` or `upstream`).
fn github_remote_url(config: &str) -> Option<String> {
    let remotes: Vec<(&str, &str)> = config
        .lines()
        .filter_map(|line| {
            let (key, url) = line.split_once(char::is_whitespace)?;
            Some((key.strip_prefix("remote.")?.strip_suffix(".url")?, url.trim()))
        })
        .collect();
    let origin = remotes.iter().filter(|(name, _)| *name == "origin");
    let others = remotes.iter().filter(|(name, _)| *name != "origin");
    origin.chain(others).find_map(|(_, url)| https_repo_url(url))
}

/// The HTTPS origin of a node whose Files directory is not a URL: a checkout
/// on this machine says it itself; a path inside a cloud sandbox (Files →
/// Runs in → Cloud sandbox) is asked in the node's own sandbox, else in the
/// sandbox its files are forked from. The error says what to set instead.
fn origin_url_of(fleet: &FleetStore, node_id: &str, repo: &str) -> Result<String> {
    let mut tried: Vec<String> = Vec::new();
    if std::path::Path::new(repo).is_dir() {
        let out = std::process::Command::new("git")
            .args(["-C", repo, "config", "--get-regexp", REMOTE_URLS])
            .output()
            .context("run git")?;
        let remotes = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Some(url) = github_remote_url(&remotes) {
            return Ok(url);
        }
        tried.push(format!("the checkout at {repo} has remotes {remotes:?}"));
    } else {
        let mut sandboxes: Vec<String> = Vec::new();
        if let Ok(Some(loc)) = fleet.read(|conn| tod_store::fleet::repos::files_location::FilesLocationRepo::new(conn).get(node_id)) {
            sandboxes.extend(loc.sandbox().map(str::to_string));
        }
        if let Ok(Some(files)) = fleet.resolve_files_for_node(node_id)
            && let Some(dev) = files.dev_container
            && let tod_store::fleet::SandboxFrom::Fork(name) = dev.sandbox_from
            && !name.trim().is_empty()
        {
            sandboxes.push(name.trim().to_string());
        }
        // `golden` is an alias for the newest golden sandbox, not a sandbox name.
        for name in &mut sandboxes {
            if name == tod_sandbox::config::GOLDEN {
                let root = fleet.paths().root();
                match tod_store::fleet::sandbox::Sandboxes::load(root)
                    .and_then(|s| s.blaxel().and_then(|bx| s.resolve_golden(&bx)))
                {
                    Ok(real) => *name = real,
                    Err(err) => tried.push(format!("could not resolve the golden sandbox: {err:#}")),
                }
            }
        }
        for name in sandboxes {
            let found = tod_store::fleet::sandbox::SandboxExec::new(&name)
                .output(repo, "git", &["config", "--get-regexp", REMOTE_URLS])
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            match found {
                Some(remotes) => match github_remote_url(&remotes) {
                    Some(url) => return Ok(url),
                    None => tried.push(format!("sandbox {name} has remotes {remotes:?} at {repo}")),
                },
                None => tried.push(format!("sandbox {name} has no git remotes at {repo}")),
            }
        }
    }
    let why = if tried.is_empty() { format!("{repo} is not a checkout on this machine") } else { tried.join("; ") };
    bail!(
        "no HTTPS repository URL for the cloud run ({why}); the cloud checks the repository out from its \
         HTTPS URL, so set the Files directory to that URL (e.g. https://github.com/owner/repo) or to a \
         checkout on this machine whose origin is on GitHub"
    )
}

/// Where a node's code comes from: its repository as an HTTPS URL, and branch.
fn node_source(fleet: &FleetStore, node_id: &str) -> Result<(tod_store::fleet::FleetTask, String, String)> {
    let task = fleet.get_node(node_id)?.ok_or_else(|| anyhow!("no node {node_id}"))?;
    // The Files capability's repository (the node's own, or the nearest
    // ancestor's) comes first: a node's own `repo` field may hold a stale path
    // (e.g. one inside a sandbox) while the ancestor holds the real URL. Try
    // each distinct candidate; the first error is the one reported.
    let inherited = fleet.resolve_files_for_node(node_id).ok().flatten().and_then(|files| files.repo);
    let mut candidates: Vec<String> = Vec::new();
    for repo in [inherited, task.repo.clone()].into_iter().flatten() {
        let repo = repo.trim().to_string();
        if !repo.is_empty() && !candidates.contains(&repo) {
            candidates.push(repo);
        }
    }
    if candidates.is_empty() {
        bail!("the node has no repository (set its workspace directory in the Files section)");
    }
    let mut first_err = None;
    let mut found = None;
    for repo in &candidates {
        let resolved = match https_repo_url(repo) {
            Some(url) => Ok(url),
            None => origin_url_of(fleet, node_id, repo),
        };
        match resolved {
            Ok(url) => {
                found = Some(url);
                break;
            }
            Err(err) => {
                first_err.get_or_insert(err);
            }
        }
    }
    let repo_url = match found {
        Some(url) => url,
        None => return Err(first_err.expect("a candidate failed")),
    };
    let branch = task.branch.clone().filter(|b| !b.trim().is_empty()).unwrap_or_else(|| task.slug.clone());
    Ok((task, repo_url, branch))
}

/// The agent a node's supervisor will run ([`CLOUD_AGENT_ENV`]), if set.
fn cloud_agent() -> Option<String> {
    std::env::var(CLOUD_AGENT_ENV).ok().map(|a| a.trim().to_string()).filter(|a| !a.is_empty())
}

/// What to tell the user when a node that runs Claude has no subscription
/// token to run it with.
pub const MISSING_CLAUDE_TOKEN: &str = "the node's sandbox runs Claude Code, which needs your Claude \
     subscription token: set it once in Settings → Cloud sandboxes → Claude subscription \
     (Get a token runs `claude setup-token`)";

/// The Claude subscription token for a node's sandbox: required unless its
/// supervisor runs the mock (`agent`), so a sandbox that could not work is
/// never created. A mock node still gets the token when there is one.
fn require_claude_token(agent: Option<&str>, token: Option<String>) -> Result<Option<String>> {
    let token = token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    if token.is_none() && agent != Some("mock") {
        bail!("{MISSING_CLAUDE_TOKEN}");
    }
    Ok(token)
}

/// [`require_claude_token`] from the credential store. Reads the OS keyring:
/// never on the UI thread.
fn claude_token_for_node(root: &Path) -> Result<Option<String>> {
    require_claude_token(
        cloud_agent().as_deref(),
        CredentialStore::from_data_root(root).get(CredentialKind::ClaudeOauthToken),
    )
}

/// Whether Blaxel's record of a sandbox says it can no longer run.
pub fn sandbox_is_dead(info: &tod_sandbox::blaxel::SandboxInfo) -> bool {
    matches!(info.status.to_ascii_uppercase().as_str(), "FAILED" | "TERMINATED" | "DELETING" | "DELETED")
}

/// The sandbox `node_id` already has, so a cloud run reuses it instead of
/// making another beside it: the one its `cloud_nodes` row names (a re-run,
/// or a replacement of a lost one), else the one its Files location made
/// (`tod-<slug>`, from opening a shell or launching an agent).
fn existing_sandbox_name(fleet: &FleetStore, bx: &tod_sandbox::blaxel::Blaxel, node_id: &str) -> Result<Option<String>> {
    let cloud = cloud_node(fleet, node_id).map(|row| row.sandbox);
    let files = fleet
        .read(|conn| tod_store::fleet::repos::files_location::FilesLocationRepo::new(conn).get(node_id))?
        .and_then(|location| location.sandbox);
    // Both are normally the same sandbox (a Files launch adopts the cloud
    // one). They differ only after one was lost and replaced: take the one
    // that can still run, rather than make a second beside it.
    let usable = |name: &str| -> Result<bool> { Ok(bx.get(name)?.is_some_and(|info| !sandbox_is_dead(&info))) };
    Ok(match (cloud, files) {
        (Some(cloud), Some(files)) if cloud != files && !usable(&cloud)? && usable(&files)? => Some(files),
        (Some(cloud), _) => Some(cloud),
        (None, files) => files,
    })
}

/// Make sure `node_id` has a working sandbox: create it (with the user's
/// credentials in its proxy) if Blaxel has none by its name, or replace one
/// that can no longer run; wait for it, provision the relay and supervisor,
/// and poke it. Returns the sandbox's name. The one path both
/// [`run_in_cloud`] and replacing a lost sandbox ([`lost`]) take. Blocks on
/// the network.
pub fn ensure_node_sandbox(
    fleet: &FleetStore,
    root: &Path,
    node_id: &str,
    user: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    use tod_sandbox::node;

    let (task, repo_url, branch) = node_source(fleet, node_id)?;
    // Before any network call: without it the sandbox could not run Claude.
    let claude_oauth_token = claude_token_for_node(root)?;
    let mut sandboxes = tod_store::fleet::sandbox::Sandboxes::load(root)?;
    let account = sandboxes.account()?.clone();
    let bx = sandboxes.blaxel()?;
    let orchestrator_url = sandboxes
        .url(&bx, &account.orchestrator)
        .context("find the orchestrator sandbox: the app deploys it at launch (see the daemon log), or run `tod-sandbox orchestrator`")?;
    let orchestrator_host = orchestrator_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let cli_url = format!(
        "{}/port/{}/cli",
        orchestrator_url.trim_end_matches('/'),
        tod_sandbox::orchestrator::PORT
    );

    let creds = CredentialStore::from_data_root(root);
    let credentials = node::NodeCredentials {
        github_token: creds.get(CredentialKind::GithubToken),
        linear_api_key: creds.get(CredentialKind::LinearApiKey),
        blaxel_token: bx.token().to_string(),
        claude_oauth_token,
        custom: Vec::new(),
    };
    let mut credentials = credentials;
    match environment_credentials(fleet, &creds, node_id) {
        Ok(custom) => credentials.custom = custom,
        Err(err) => progress(&format!("warning: the node's Environment was not applied: {err:#}")),
    }
    if credentials.github_token.is_none() {
        progress("warning: no GitHub token stored; the node cannot push or open a pull request");
    }

    let source = node_sandbox_source(fleet, &sandboxes, &bx, node_id)?;
    let name = existing_sandbox_name(fleet, &bx, node_id)?.unwrap_or_else(|| node_sandbox_name(&task.slug));
    let agent = cloud_agent();
    let spec = node::NodeSandboxSpec {
        name: &name,
        image: match &source {
            NodeSandboxSource::Image(image) => image,
            // Not used: a fork is made from its source, not from an image.
            NodeSandboxSource::Fork(golden) => golden,
        },
        region: &account.region,
        memory_mb: account.memory_mb,
        user,
        node: node_id,
        orchestrator_host: &orchestrator_host,
        orchestrator_cli_url: &cli_url,
        agent: agent.as_deref(),
        claude_via: account.claude_token_via,
        scheduler: sandboxes.config.scheduler,
        workspace: &account.workspace,
    };
    let relay_path = tod_store::fleet::sandbox::relay_path()?;
    let relay = std::fs::read(&relay_path).with_context(|| format!("read {}", relay_path.display()))?;
    let supervisor = relay_path.parent().and_then(node::supervisor_from);
    let local_cli = relay_path.parent().and_then(node::local_cli_from);
    // The supervisor builds the agent's context from the same process and
    // media bundles as this app, installed beside it.
    let mut bundles = node::bundle_files(crate::process_bundle::TodInstallPaths::discover()?.process_root(), "process")
        .context("read the process bundle")?;
    bundles.extend(
        node::bundle_files(
            crate::media::MediaPaths::discover().map_err(|e| anyhow!("media bundle: {e}"))?.media_root(),
            "media",
        )
        .context("read the media bundle")?,
    );
    let identity = git_identity(task.repo.as_deref());
    let supervisor_env = node::supervisor_env(&credentials, account.claude_token_via);
    let payload = node::NodePayload {
        relay: &relay,
        shim: tod_store::fleet::cli_relay::HTTP_SHIM_SCRIPT.as_bytes(),
        local_cli: local_cli.as_deref(),
        supervisor: supervisor.as_deref(),
        repo_url: &repo_url,
        branch: &branch,
        git_identity: &identity,
        bundles: &bundles,
        // At every provisioning, so the supervisor's next start has the
        // token as it is now (with `claude_token_via = "env"`).
        supervisor_env: &supervisor_env,
    };

    let started = std::time::Instant::now();
    let existing = bx.get(&name)?;
    if let Some(info) = &existing
        && sandbox_is_dead(info)
    {
        progress(&format!("removing the dead sandbox {name} ({})…", info.status));
        bx.delete(&name)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        while bx.get(&name)?.is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    if existing.as_ref().is_none_or(sandbox_is_dead) {
        match &source {
            NodeSandboxSource::Fork(golden) => {
                progress(&format!("forking {name} from {golden}…"));
                if let Err(err) = node::fork_golden(&bx, golden, &spec, &credentials) {
                    // A fork that got as far as making the sandbox is not
                    // one to keep: it may have no credentials.
                    if bx.get(&name).ok().flatten().is_some() {
                        let _ = bx.delete(&name);
                    }
                    return Err(err.context(format!("fork {name} from {golden}")));
                }
            }
            NodeSandboxSource::Image(image) => {
                progress(&format!("creating sandbox {name} from {image}…"));
                node::create(&bx, &spec, &credentials)?;
            }
        }
    }
    progress(&format!("waiting for {name}…"));
    let info = bx.wait_deployed(&name, Duration::from_secs(300))?;
    let url = info.url.clone().ok_or_else(|| anyhow!("sandbox {name} has no URL"))?;
    progress(&format!("{name} is up after {:.1}s", started.elapsed().as_secs_f64()));

    // Agent Drive is not enabled yet, so the node's transcripts go to the
    // orchestrator and nothing is mounted. When it is, turn this back on:
    // mount before the supervisor starts (it mirrors into the mount when it
    // is there); a fork does not keep its source's mounts, so this is every
    // time. `drive::mount_for_node` takes the sandbox's region: a fork's
    // golden's (`bx.get_json(golden)`, `spec.region`), else the account's.
    //
    // match tod_sandbox::drive::mount_for_node(&bx, &url, &region, user, node_id) {
    //     Ok(()) => progress("mounted Agent Drive for the node's transcripts"),
    //     Err(err) => progress(&format!(
    //         "warning: no Agent Drive ({err:#}); the node's transcripts go to the orchestrator instead"
    //     )),
    // }

    match &source {
        NodeSandboxSource::Fork(_) => {
            node::install_into_fork(&bx, &url, &payload, progress)?;
            node::start(&bx, &url, &payload, false, progress)?;
        }
        NodeSandboxSource::Image(_) => {
            progress("installing the relay and supervisor…");
            node::provision(&bx, &url, &payload, progress)?;
        }
    }
    progress(&format!(
        "{name} {} and provisioned after {:.1}s",
        match &source {
            NodeSandboxSource::Fork(_) => "forked",
            NodeSandboxSource::Image(_) => "created",
        },
        started.elapsed().as_secs_f64()
    ));

    push_supervisor_settings(&bx, &url, root, node_id)?;
    progress("waking the node…");
    let poke = format!("{}/port/{}/poke", url.trim_end_matches('/'), tod_sandbox::blaxel::RELAY_PORT);
    let resp = ureq::post(&poke)
        .header("Authorization", &format!("Bearer {}", bx.token()))
        .config()
        .http_status_as_error(false)
        .build()
        .send_empty()
        .context("poke the node")?;
    if !resp.status().is_success() {
        progress(&format!("warning: the poke got {}", resp.status()));
    }
    let _ = sandboxes.save();
    Ok(name)
}

/// Where a node's sandbox comes from: configured on the node or the nearest
/// ancestor with a Files setting, never defaulted.
enum NodeSandboxSource {
    /// A sandbox to fork, its `golden` alias resolved.
    Fork(String),
    Image(String),
}

fn node_sandbox_source(
    fleet: &FleetStore,
    sandboxes: &tod_store::fleet::sandbox::Sandboxes,
    bx: &tod_sandbox::blaxel::Blaxel,
    node_id: &str,
) -> Result<NodeSandboxSource> {
    use tod_store::fleet::SandboxFrom;
    let files = fleet
        .resolve_files_for_node(node_id)?
        .ok_or_else(|| anyhow!("no Files setting applies to this node or its ancestors, so there is no sandbox to make it from"))?;
    let origin = &files.source_title;
    let dev = files
        .dev_container
        .filter(|d| d.sandbox)
        .ok_or_else(|| anyhow!("the Files setting on \"{origin}\" does not run in a sandbox; choose the sandbox to fork or an image there"))?;
    match dev.sandbox_from {
        SandboxFrom::Fork(name) => match name.trim() {
            "" => bail!("the Files setting on \"{origin}\" forks a sandbox but names none; choose the sandbox to fork"),
            n if n == tod_sandbox::config::GOLDEN => Ok(NodeSandboxSource::Fork(
                sandboxes.resolve_golden(bx).with_context(|| format!("resolve the golden sandbox that \"{origin}\" forks"))?,
            )),
            n => Ok(NodeSandboxSource::Fork(n.to_string())),
        },
        SandboxFrom::Image(image) => match image.trim() {
            "" => bail!(
                "the Files setting on \"{origin}\" uses the default image, which a cloud run never does; \
                 choose the sandbox to fork or name an image"
            ),
            image => Ok(NodeSandboxSource::Image(image.to_string())),
        },
    }
}

/// Records that `node_id` runs in `sandbox` (clearing any lost mark) in the
/// database, which is synced: the orchestrator needs to know the node is
/// active (`tod_core::impact`), and its supervisor reads its context mark
/// there.
fn record_cloud_node(fleet: &FleetStore, node_id: &str, sandbox: String, user: &str) -> Result<CloudNode> {
    let record = CloudNode {
        sandbox,
        user: user.to_string(),
        accepted_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        lost_at: None,
    };
    let node_uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    // Through the writer (the daemon's, in the app), not a connection of its own.
    fleet.enqueue(FleetMutation::CloudNodeUpsert {
        node: node_uuid,
        sandbox: record.sandbox.clone(),
        user: record.user.clone(),
        accepted_at: record.accepted_at_ms,
    })?;
    fleet.writer().flush()?;
    let _ = fleet.reload_if_stale();
    Ok(record)
}

/// Run `node_id` in the cloud: sync (seeding first), make its sandbox
/// ([`ensure_node_sandbox`]), and record it. `progress` hears each step.
/// Logs one progress line from a cloud job, at `warn` when it says "warning:"
/// (these lines otherwise reach only the panel).
pub fn log_progress(context: &str, msg: &str) {
    if msg.starts_with("warning:") {
        tracing::warn!("cloud: {context}: {msg}");
    } else {
        tracing::info!("cloud: {context}: {msg}");
    }
}

pub fn run_in_cloud(
    fleet: &FleetStore,
    root: &Path,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<CloudNode> {
    // Fails early, before any network call, on a node with no repository or
    // no Claude token to run it with.
    node_source(fleet, node_id)?;
    claude_token_for_node(root)?;
    progress("syncing with the orchestrator…");
    let (orch, user) = resolve(root)?;
    sync(fleet, root, &orch, &user)?;
    let name = ensure_node_sandbox(fleet, root, node_id, &user, progress)?;
    let record = record_cloud_node(fleet, node_id, name, &user)?;
    if let Err(err) = sync(fleet, root, &orch, &user) {
        progress(&format!("warning: could not send the record to the orchestrator yet: {err:#}"));
    }
    Ok(record)
}

/// An orchestrator that rejects rows this app wrote (a constraint or a
/// column it does not have) is older than the app: says so, and how to bring
/// it up to date. Any other error passes through unchanged.
fn older_orchestrator_hint(err: anyhow::Error) -> anyhow::Error {
    let text = format!("{err:#}");
    if text.contains("CHECK constraint failed") || text.contains("no such column") || text.contains("no such table") {
        return err.context(
            "the orchestrator is older than this app and does not accept what it wrote; \
             update it with `scripts/build-sandbox-binaries.sh` then `tod-sandbox orchestrator`",
        );
    }
    err
}

/// The settings a node's supervisor reads in its sandbox, as `tod.yml`: only
/// what it acts on there (`lifecycle`: which repositories release, and
/// `pr_readiness`: the review schedule), so nothing else of this machine's
/// settings leaves it. Without them the supervisor judges a merged pull
/// request by the defaults, and waits for a release a `merge` repository
/// never makes.
pub fn supervisor_settings(root: &Path) -> Result<String> {
    let all = tod_store::TodSettings::load(&tod_store::TodPaths::at(root))?;
    let mut shared = tod_store::TodSettings::default();
    shared.lifecycle = all.lifecycle;
    shared.pr_readiness = all.pr_readiness;
    serde_yaml::to_string(&shared).context("serialize the supervisor's settings")
}

/// Writes [`supervisor_settings`] into the node's sandbox, where its
/// supervisor loads them from its state directory.
fn push_supervisor_settings(bx: &tod_sandbox::blaxel::Blaxel, url: &str, root: &Path, node_id: &str) -> Result<()> {
    let path = format!(
        "{}/{node_id}/{}",
        tod_store::fleet::cli_relay::SUPERVISOR_STATE_ROOT,
        tod_store::paths::SETTINGS_FILE
    );
    bx.upload(url, &path, supervisor_settings(root)?.as_bytes(), "0644")
        .with_context(|| format!("send the settings to the supervisor ({path})"))
}

/// Wakes `node_id`'s supervisor now, instead of at its next scheduled check:
/// sends what changed here (a cancelled wait) to the orchestrator, then
/// pokes the node's sandbox, which pulls it and goes on. Returns what it
/// did. Blocks on the network: never on the UI thread.
pub fn wake_node(fleet: &FleetStore, root: &Path, node_id: &str) -> Result<String> {
    let row = cloud_node(fleet, node_id).ok_or_else(|| anyhow!("this node does not run in the cloud"))?;
    let (orch, user) = resolve(root)?;
    sync(fleet, root, &orch, &user).context("send the change to the orchestrator")?;
    let sandboxes = tod_store::fleet::sandbox::Sandboxes::load(root)?;
    let bx = sandboxes.blaxel()?;
    let info = bx
        .get(&row.sandbox)?
        .ok_or_else(|| anyhow!("sandbox {} no longer exists", row.sandbox))?;
    let url = info.url.ok_or_else(|| anyhow!("sandbox {} has no URL", row.sandbox))?;
    push_supervisor_settings(&bx, &url, root, node_id)?;
    let poke = format!("{}/port/{}/poke", url.trim_end_matches('/'), tod_sandbox::blaxel::RELAY_PORT);
    let resp = ureq::post(&poke)
        .header("Authorization", &format!("Bearer {}", bx.token()))
        .config()
        .http_status_as_error(false)
        .build()
        .send_empty()
        .context("poke the node")?;
    if !resp.status().is_success() {
        bail!("poking sandbox {} got {}", row.sandbox, resp.status());
    }
    Ok(format!("woke the supervisor in {}", row.sandbox))
}

pub mod lost;
pub mod deploy;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod older_orchestrator_tests {
    use super::older_orchestrator_hint;

    #[test]
    fn a_rejected_row_says_the_orchestrator_is_older() {
        let err = anyhow::anyhow!("orchestrator changes: 500: CHECK constraint failed: capability IN ('spec')");
        let text = format!("{:#}", older_orchestrator_hint(err));
        assert!(text.contains("older than this app"), "{text}");
        assert!(text.contains("CHECK constraint failed"), "{text}");
    }

    #[test]
    fn other_errors_are_left_alone() {
        let text = format!("{:#}", older_orchestrator_hint(anyhow::anyhow!("connection refused")));
        assert_eq!(text, "connection refused");
    }
}
