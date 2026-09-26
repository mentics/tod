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
use tod_store::fleet::FleetStore;
use tod_store::sync::{self, ApplyReport, Change};

/// The app's cloud-sync record, under the data root.
pub const STATE_FILE: &str = "cloud-sync.json";

/// Overrides the orchestrator's URL (e.g. `http://127.0.0.1:8080` in tests).
pub const ORCHESTRATOR_URL_ENV: &str = "TOD_ORCHESTRATOR_URL";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudSyncState {
    /// The orchestrator's base URL; else [`ORCHESTRATOR_URL_ENV`], else the
    /// `tod-orchestrator` sandbox's `/port/8080`.
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

/// Held by every [`sync`] in this process. Background syncs (the outbox
/// push, notices, the start) go through one coalescing runner
/// ([`crate::cloud_notify::runner`]); this lock also keeps the user's own
/// "Sync now" and [`run_in_cloud`] from overlapping one.
static SYNC_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn sync_lock() -> std::sync::MutexGuard<'static, ()> {
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
        let v = Self::finish(resp, "changes")?;
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
    drop(_guard);
    if let Err(err) = lost::retire_done(fleet) {
        tracing::warn!("cloud sync: retiring done nodes: {err:#}");
    }
    Ok(report)
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
/// `tod-orchestrator` sandbox) and, for a sandbox URL, the Blaxel token.
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
    let url = sandboxes
        .url(&bx, tod_sandbox::orchestrator::NAME)
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
    if !CloudSyncState::load(&root).is_ok_and(|s| s.seeded) {
        return;
    }
    // Sync, then check every cloud node's sandbox against Blaxel and
    // replace any that is gone.
    lost::spawn_check(fleet, lost::Check::Full);
}

/// Where a node's code comes from: its repository as an HTTPS URL, and branch.
fn node_source(fleet: &FleetStore, node_id: &str) -> Result<(tod_store::fleet::FleetTask, String, String)> {
    let task = fleet.get_node(node_id)?.ok_or_else(|| anyhow!("no node {node_id}"))?;
    let repo = task.repo.clone().filter(|r| !r.trim().is_empty()).ok_or_else(|| {
        anyhow!("the node has no repository (set its workspace directory in the Files section)")
    })?;
    let repo_url = match https_repo_url(&repo) {
        Some(url) => url,
        None => {
            let out = std::process::Command::new("git")
                .args(["-C", &repo, "remote", "get-url", "origin"])
                .output()
                .context("run git")?;
            let remote = String::from_utf8_lossy(&out.stdout).trim().to_string();
            https_repo_url(&remote)
                .ok_or_else(|| anyhow!("{repo}: no HTTPS-reachable origin remote ({remote:?})"))?
        }
    };
    let branch = task.branch.clone().filter(|b| !b.trim().is_empty()).unwrap_or_else(|| task.slug.clone());
    Ok((task, repo_url, branch))
}

/// Whether Blaxel's record of a sandbox says it can no longer run.
pub fn sandbox_is_dead(info: &tod_sandbox::blaxel::SandboxInfo) -> bool {
    matches!(info.status.to_ascii_uppercase().as_str(), "FAILED" | "TERMINATED" | "DELETING" | "DELETED")
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
    let mut sandboxes = tod_store::fleet::sandbox::Sandboxes::load(root)?;
    let account = sandboxes.account()?.clone();
    let bx = sandboxes.blaxel()?;
    let orchestrator_url = sandboxes
        .url(&bx, tod_sandbox::orchestrator::NAME)
        .context("find the orchestrator sandbox")?;
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
    };
    if credentials.github_token.is_none() {
        progress("warning: no GitHub token stored; the node cannot push or open a pull request");
    }

    let name = node_sandbox_name(&task.slug);
    let spec = node::NodeSandboxSpec {
        name: &name,
        image: &account.default_image,
        region: &account.region,
        memory_mb: account.memory_mb,
        user,
        node: node_id,
        orchestrator_host: &orchestrator_host,
        orchestrator_cli_url: &cli_url,
    };
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
        progress(&format!("creating sandbox {name}…"));
        node::create(&bx, &spec, &credentials)?;
    }
    progress(&format!("waiting for {name}…"));
    let info = bx.wait_deployed(&name, Duration::from_secs(300))?;
    let url = info.url.clone().ok_or_else(|| anyhow!("sandbox {name} has no URL"))?;

    let relay_path = tod_store::fleet::sandbox::relay_path()?;
    let relay = std::fs::read(&relay_path).with_context(|| format!("read {}", relay_path.display()))?;
    let supervisor = relay_path.parent().and_then(node::supervisor_from);
    let payload = node::NodePayload {
        relay: &relay,
        shim: tod_store::fleet::cli_relay::HTTP_SHIM_SCRIPT.as_bytes(),
        supervisor: supervisor.as_deref(),
        repo_url: &repo_url,
        branch: &branch,
    };
    progress("installing the relay and supervisor…");
    node::provision(&bx, &url, &payload, progress)?;

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
    let _ = fleet.flush_on_quit();
    tod_store::cloud_nodes::upsert(
        &connect(fleet.paths().db())?,
        node_uuid,
        &record.sandbox,
        &record.user,
        record.accepted_at_ms,
    )?;
    let _ = fleet.reload_if_stale();
    Ok(record)
}

/// Run `node_id` in the cloud: sync (seeding first), make its sandbox
/// ([`ensure_node_sandbox`]), and record it. `progress` hears each step.
pub fn run_in_cloud(
    fleet: &FleetStore,
    root: &Path,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<CloudNode> {
    // Fails early, before any network call, on a node with no repository.
    node_source(fleet, node_id)?;
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

pub mod lost;

#[cfg(test)]
mod tests;
