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
    /// The orchestrator's seeding ([`Seeding::seed_id`]) this copy was sent
    /// to. When the orchestrator names another, its database was replaced
    /// and the next sync seeds it again ([`sync_once`]).
    #[serde(default)]
    pub seed_id: Option<String>,
    /// The last local `sync_changes.seq` the orchestrator accepted.
    #[serde(default)]
    pub sent_after: i64,
    /// The feed's cursor: the orchestrator's `last_seq` last pulled.
    #[serde(default)]
    pub feed_after: i64,
    /// Nodes running in the cloud, by node id: the record older builds kept
    /// here, moved into the database by [`lost::adopt_legacy_records`].
    #[serde(default)]
    pub nodes: BTreeMap<String, LegacyCloudNode>,
    /// Nodes whose sandbox the user deleted on purpose: the lost-sandbox
    /// check leaves them alone until the user runs them in the cloud again.
    #[serde(default)]
    pub deleted_by_user: std::collections::BTreeSet<String>,
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

/// A node record as older builds kept it in [`STATE_FILE`], which named the
/// sandbox itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyCloudNode {
    pub sandbox: String,
    pub user: String,
    pub accepted_at_ms: i64,
    #[serde(default)]
    pub lost_at: Option<i64>,
}

/// A node that runs in the cloud, as its `cloud_nodes` row says. The row's
/// copy of the sandbox's name is deliberately not here: which sandbox the
/// node has is its Files location's to say ([`cloud_sandbox`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudNode {
    pub user: String,
    pub accepted_at_ms: i64,
    /// When the orchestrator found its sandbox gone, until the app replaces it.
    pub lost_at: Option<i64>,
}

impl From<tod_store::cloud_nodes::CloudNodeRow> for CloudNode {
    fn from(row: tod_store::cloud_nodes::CloudNodeRow) -> Self {
        Self { user: row.user, accepted_at_ms: row.accepted_at, lost_at: row.lost_at }
    }
}

/// The sandbox `node_id` has, if any: the one its Files location records, the one
/// record of it (`cloud_nodes.sandbox` is a copy for the orchestrator that
/// nothing here reads). Reads the store's own connection: fine on the UI thread.
pub fn cloud_sandbox_if_any(fleet: &FleetStore, node_id: &str) -> Result<Option<String>> {
    tod_store::fleet::session_log::node_sandbox(fleet, node_id)
}

/// [`cloud_sandbox_if_any`] for a node that must have one.
pub fn cloud_sandbox(fleet: &FleetStore, node_id: &str) -> Result<String> {
    cloud_sandbox_if_any(fleet, node_id)?.ok_or_else(|| {
        anyhow!(
            "node {node_id} runs in the cloud but its Files location records no sandbox (a record made before \
             the location was the one record: Start makes the node a sandbox, or a later step adopts the old one)"
        )
    })
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

/// Free space a wake needs (the supervisor's settings, its state, its logs).
const WAKE_MIN_FREE: u64 = 64 * 1024 * 1024;

/// Records (or, with `deleted` false, forgets) that the user deleted
/// `node_id`'s sandbox on purpose.
pub fn set_sandbox_deleted_by_user(root: &Path, node_id: &str, deleted: bool) -> Result<()> {
    let mut state = CloudSyncState::load(root)?;
    let changed = if deleted { state.deleted_by_user.insert(node_id.to_string()) } else { state.deleted_by_user.remove(node_id) };
    if changed {
        state.save(root)?;
    }
    Ok(())
}

/// Whether the user deleted `node_id`'s sandbox on purpose and has not run
/// the node in the cloud since. Reads a small file: for display, a failed
/// read is logged and counts as no.
pub fn sandbox_deleted_by_user(root: &Path, node_id: &str) -> bool {
    match CloudSyncState::load(root) {
        Ok(state) => state.deleted_by_user.contains(node_id),
        Err(err) => {
            tracing::warn!("cloud: reading which sandboxes the user deleted: {err:#}");
            false
        }
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

/// What the app knows of a cloud node's supervisor, for the places that show
/// a cloud node's transcript or status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudStatus {
    /// The supervisor said it was working at `heard_at` (ms), while `what`.
    Working { heard_at: i64, what: String },
    /// It said so once and has not since: its sandbox or process died
    /// mid-run, or the news has not reached this app.
    Silent { heard_at: i64, what: String },
    /// Nothing is running in the cloud for it (no heartbeat): it finished,
    /// is waiting, or has not started.
    Idle,
    /// Its sandbox is gone; the app replaces it at the next sync.
    Lost,
    /// The user deleted its sandbox; nothing replaces it until they Start the
    /// node again.
    Deleted,
}

impl CloudStatus {
    /// One sentence for what a sync found out about the node, with clock
    /// times (not ages) so it does not go stale on screen.
    pub fn describe(&self) -> String {
        let at = |ms: i64| {
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
                .unwrap_or_default()
        };
        let doing = |what: &str| if what.is_empty() { String::new() } else { format!(" while {what}") };
        match self {
            Self::Working { heard_at, what } => {
                format!("Its supervisor is working{} (last heard {}).", doing(what), at(*heard_at))
            }
            Self::Silent { heard_at, what } => format!(
                "Its supervisor has not been heard from since {}{}: it stopped, or its news has not arrived.",
                at(*heard_at),
                doing(what)
            ),
            Self::Idle => "Nothing is running in the cloud for it now.".to_string(),
            Self::Lost => "Its sandbox is gone; a new one is made at the next sync.".to_string(),
            Self::Deleted => "Its sandbox was deleted; Start makes a new one.".to_string(),
        }
    }
}

/// A cloud node's [`CloudStatus`]; `None` for a node that does not run in
/// the cloud. Reads the store's own connection: fine on the UI thread.
pub fn cloud_status(fleet: &FleetStore, node_id: uuid::Uuid, now_ms: i64) -> Option<CloudStatus> {
    let (row, beat) = fleet
        .read(|conn| {
            Ok((
                tod_store::cloud_nodes::get(conn, node_id)?,
                tod_store::cloud_activity::get(conn, node_id)?,
            ))
        })
        .ok()?;
    let row = row?;
    if row.lost_at.is_some() {
        let deleted = sandbox_deleted_by_user(fleet.paths().root(), &node_id.to_string());
        return Some(if deleted { CloudStatus::Deleted } else { CloudStatus::Lost });
    }
    Some(match beat {
        Some(b) if b.is_idle() => CloudStatus::Idle,
        Some(b) if b.is_live(now_ms) => CloudStatus::Working { heard_at: b.heartbeat_at, what: b.what },
        Some(b) => CloudStatus::Silent { heard_at: b.heartbeat_at, what: b.what },
        None => CloudStatus::Idle,
    })
}

/// A node whose supervisor is working in the cloud now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudWork {
    pub node_id: uuid::Uuid,
    pub title: String,
    pub lifecycle: String,
}

/// The cloud nodes whose supervisor is working now: those `next_actor` says
/// are `Working`, which for a cloud node is proved only by a live heartbeat
/// (`tod_store::cloud_activity`) the supervisor writes while its autopilot
/// runs. A node that is assigned to the cloud but idle, stopped, waiting, or
/// whose supervisor stopped beating is not working, whatever else is or is
/// not recorded for it (an absent wait or request proves nothing).
///
/// Reads the store's own connection, so call it off the UI thread.
pub fn working_in_cloud(fleet: &FleetStore) -> Result<Vec<CloudWork>> {
    let working = fleet.read(|conn| {
        let ids: Vec<uuid::Uuid> = tod_store::cloud_nodes::list(conn)?
            .into_iter()
            .filter(|row| row.lost_at.is_none())
            .map(|row| row.node_id)
            .collect();
        let inputs = crate::next_actor::Inputs { now_ms: tod_store::outline::uuid_blob::now_ms(), ..Default::default() };
        let standings = crate::next_actor::for_nodes(conn, &ids, &inputs)?;
        Ok(ids
            .into_iter()
            .filter(|id| standings.get(id).is_some_and(|s| matches!(s.actor, crate::next_actor::NextActor::Working { .. })))
            .collect::<Vec<_>>())
    })?;
    let mut out = Vec::new();
    for node_id in working {
        let Some(node) = fleet.get_node(&node_id.to_string())? else { continue };
        let title = if node.title.trim().is_empty() { node.slug.clone() } else { node.title.clone() };
        out.push(CloudWork { node_id, title, lifecycle: node.lifecycle });
    }
    Ok(out)
}

/// Held by every [`sync`] in this process. Background syncs (the outbox
/// push, notices, the start) go through one coalescing runner
/// ([`crate::cloud_notify::runner`]); this lock also keeps the user's own
/// "Sync now" and [`run_in_cloud`] from overlapping one.
static SYNC_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The lock file beside [`STATE_FILE`] that keeps two *processes* from
/// syncing at once: the app and `tod-agentd` both sync one data root (the
/// daemon in the background, the app on the user's click and while it waits
/// for a node's supervisor), and both read and write the cursors in
/// [`STATE_FILE`].
pub const LOCK_FILE: &str = "cloud-sync.lock";

/// [`SYNC_LOCK`] and the lock on [`LOCK_FILE`], released together.
pub(crate) struct SyncGuard {
    _process: std::sync::MutexGuard<'static, ()>,
    _file: std::fs::File,
}

/// Waits for any sync of `root`, in this process or another, to finish, and
/// holds the right to sync.
pub(crate) fn sync_lock(root: &Path) -> Result<SyncGuard> {
    let process = SYNC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = root.join(LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    fs2::FileExt::lock_exclusive(&file).with_context(|| format!("lock {}", path.display()))?;
    Ok(SyncGuard { _process: process, _file: file })
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

/// What the orchestrator answers to a seed.
pub struct Seeding {
    /// The feed's start: the snapshot's own change number.
    pub last_seq: i64,
    /// Names this seeding ([`CloudSyncState::seed_id`]); `None` from an
    /// orchestrator too old to say.
    pub seed_id: Option<String>,
}

/// What the orchestrator answers to a feed request.
pub struct Feed {
    pub last_seq: i64,
    pub changes: Vec<Change>,
    /// The seeding the feed follows; see [`Seeding::seed_id`].
    pub seed_id: Option<String>,
    /// How many seconds the orchestrator's notice service has left in a
    /// refusal (HTTP 429): no notice will arrive until then, so the app pulls
    /// on a short timer ([`crate::cloud_notify::DEGRADED_POLL`]). `None` from
    /// an orchestrator that is publishing, or too old to say.
    pub notices_degraded_secs: Option<u64>,
    /// The orchestrator honoured `?wait=` (a long-poll); `false` from an
    /// orchestrator too old to know it, which answers at once.
    pub waited: bool,
}

/// How the app reaches the orchestrator.
pub trait Orchestrator {
    /// `POST /users/<u>/seed`: replaces the orchestrator's copy.
    fn seed(&self, user: &str, snapshot: &[u8]) -> Result<Seeding>;
    /// `POST /users/<u>/changes`.
    fn send(&self, user: &str, changes: &[Change]) -> Result<ApplyReport>;
    /// `GET /users/<u>/changes?after=n`.
    fn pull(&self, user: &str, after: i64) -> Result<Feed>;
    /// `POST /users/<u>/problems/pull`: what the cloud's executables reported
    /// after `cursor` (`None`: from now), and the new cursor. Only ever
    /// called inside a sync, so it adds no contact with the cloud of its own.
    fn pull_problems(
        &self,
        user: &str,
        cursor: Option<&tod_problems::Cursor>,
    ) -> Result<(tod_problems::Cursor, Vec<tod_problems::Problem>)>;
    /// Refuses (with both versions named) an orchestrator whose wire or
    /// schema is newer than this app reads (`tod_store::sync::contract`);
    /// an older one is the app's to redeploy (`deploy`), not a refusal.
    fn check_contract(&self) -> Result<()> {
        Ok(())
    }
    /// `PUT /users/<u>/github-routes`: which repositories each of the
    /// orchestrator's GitHub tokens reads. A seeding gives the orchestrator a
    /// fresh disk's worth of state, which holds no routes, so each seeding is
    /// followed by this.
    fn put_github_routes(&self, _user: &str, _routes: &tod_store::github_routes::GithubRoutes) -> Result<()> {
        Ok(())
    }
}

/// This build's version contract with the orchestrator.
pub fn my_contract() -> tod_store::sync::contract::Contract {
    tod_store::sync::contract::Contract::current(crate::CLI_BUILD_STAMP)
}

/// How many times a request that is safe to repeat is sent while the
/// gateway answers 502/503/504 (pauses of 2 s, then 4 s between them).
const GATEWAY_ATTEMPTS: u32 = 3;

/// A transport error worth sending the request again for: the connection
/// could not be made, timed out, or was reset.
fn transient(err: &ureq::Error) -> bool {
    use std::io::ErrorKind::*;
    match err {
        ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed => true,
        ureq::Error::Io(e) => matches!(e.kind(), ConnectionReset | ConnectionAborted | BrokenPipe | TimedOut | UnexpectedEof | ConnectionRefused),
        _ => false,
    }
}

/// The orchestrator over HTTP(S).
pub struct HttpOrchestrator {
    base: String,
    token: Option<String>,
    /// This app as a sync client (`X-Tod-Client`): what it sends reaches the
    /// node supervisors and never comes back to it.
    client: String,
    /// Sends `X-Tod-Take-Over: yes` with a seed, which the orchestrator
    /// otherwise refuses when another client made the current seeding. Set
    /// only when the user chose to take it over.
    take_over: bool,
    agent: ureq::Agent,
    /// Set once the orchestrator's contract was judged readable.
    contract_ok: std::sync::atomic::AtomicBool,
}

impl HttpOrchestrator {
    pub fn new(base: impl Into<String>, token: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(300)))
            .build()
            .into();
        Self { base: base.into().trim_end_matches('/').to_string(), token, client: "app".into(), take_over: false, agent, contract_ok: false.into() }
    }

    /// Names this app as the sync client `client` ([`client_id`]).
    pub fn with_client(mut self, client: impl Into<String>) -> Self {
        self.client = client.into();
        self
    }

    /// Whether seeds carry `X-Tod-Take-Over: yes`: the user's explicit choice
    /// to replace a database another client seeded (the UI sets this only
    /// from that choice; see `tod_orchestrator::sync_backend::seed`).
    pub fn with_take_over(mut self, take_over: bool) -> Self {
        self.take_over = take_over;
        self
    }

    /// `GET /health`: the orchestrator's `{wire, schema, build}`; an
    /// orchestrator from before the contract reads as wire 0.
    pub fn health(&self) -> Result<tod_store::sync::contract::Contract> {
        let mut resp = Self::with_gateway_retry("health", || self.auth(self.agent.get(&format!("{}/health", self.base))).call())?;
        let status = resp.status();
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        if !status.is_success() {
            bail!("orchestrator health: {}", tod_sandbox::blaxel::describe_failure(status, &body));
        }
        Ok(tod_store::sync::contract::Contract::parse_health(&body))
    }

    /// `GET /users/<user>/<rest>` as JSON.
    pub fn get_json(&self, user: &str, rest: &str) -> Result<serde_json::Value> {
        let resp = self.auth(self.agent.get(&self.url(user, rest))).call().with_context(|| format!("orchestrator {rest}"))?;
        Self::finish(resp, rest)
    }

    /// `PUT /users/<user>/github-routes`: which of the orchestrator's GitHub
    /// tokens reads which repository.
    pub fn put_github_routes(&self, user: &str, routes: &tod_store::github_routes::GithubRoutes) -> Result<()> {
        let mut resp = self
            .auth(self.agent.put(&self.url(user, "github-routes")))
            .send_json(routes)
            .context("orchestrator github-routes")?;
        let status = resp.status();
        if !status.is_success() {
            let body: String = resp.body_mut().read_to_string().unwrap_or_default().chars().take(400).collect();
            bail!("orchestrator github-routes: {status}: {body}");
        }
        Ok(())
    }

    /// `GET /github-secrets`: the names of the secrets the last apply put in
    /// the orchestrator's proxy; `None` when none is recorded (an orchestrator
    /// older than this endpoint, or a fresh disk).
    pub fn github_secrets(&self) -> Result<Option<Vec<String>>> {
        let resp = self
            .auth(self.agent.get(&format!("{}/github-secrets", self.base)))
            .call()
            .context("orchestrator github-secrets")?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let v = Self::finish(resp, "github-secrets")?;
        let names = v["names"]
            .as_array()
            .ok_or_else(|| anyhow!("orchestrator github-secrets: no names"))?
            .iter()
            .filter_map(|n| n.as_str().map(str::to_string))
            .collect();
        Ok(Some(names))
    }

    /// `PUT /github-secrets`: records the names (never values) now in the
    /// orchestrator's proxy.
    pub fn put_github_secrets(&self, names: &[String]) -> Result<()> {
        let resp = self
            .auth(self.agent.put(&format!("{}/github-secrets", self.base)))
            .send_json(serde_json::json!({ "names": names }))
            .context("orchestrator github-secrets")?;
        Self::finish_text(resp, "github-secrets")
    }

    /// The next request a node's sandbox has asked this machine to answer
    /// (`GET /users/<u>/broker/next`), waiting up to `wait_secs` for one:
    /// `(id, node, request)`. Holding this open is also how the orchestrator
    /// knows this machine is there to answer.
    pub fn broker_next(&self, user: &str, wait_secs: u64) -> Result<Option<(u64, String, serde_json::Value)>> {
        let rest = format!("broker/next?wait={wait_secs}");
        let timeout = Duration::from_secs(sync::BROKER_POLL_TIMEOUT_SECS.max(wait_secs + 15));
        let url = self.url(user, &rest);
        let mut attempt = 1;
        let resp = loop {
            let sent = self.auth(self.agent.get(&url)).config().timeout_global(Some(timeout)).build().call();
            let why = match &sent {
                Ok(resp) if matches!(resp.status().as_u16(), 502 | 503 | 504) => format!("{} from the gateway", resp.status()),
                Err(err) if transient(err) => err.to_string(),
                _ => break sent.context("orchestrator broker")?,
            };
            if attempt >= sync::BROKER_POLL_ATTEMPTS {
                break sent.context("orchestrator broker")?;
            }
            let wait = Duration::from_secs(1 << attempt);
            tracing::info!("orchestrator broker: {why}; retrying in {wait:?} (attempt {attempt} of {})", sync::BROKER_POLL_ATTEMPTS);
            std::thread::sleep(wait);
            attempt += 1;
        };
        let v = Self::finish(resp, "broker")?;
        let Some(id) = v["id"].as_u64() else { return Ok(None) };
        let node = v["node"].as_str().context("orchestrator broker: no node")?.to_string();
        Ok(Some((id, node, v["request"].clone())))
    }

    /// `GET /users/<u>/changes?after=n&wait=<secs>`: the feed as a long-poll.
    /// The orchestrator answers when another client's change is there, or
    /// after `wait_secs` with no changes; one request is in flight, and the
    /// orchestrator is awake only while it is. [`Feed::waited`] is `false`
    /// when the orchestrator is too old to know `wait` (it answered at once).
    pub fn pull_waiting(&self, user: &str, after: i64, wait_secs: u64) -> Result<Feed> {
        self.pull_feed(user, after, Some(wait_secs))
    }

    /// The feed, with the gateway retry (502/503/504) either way.
    fn pull_feed(&self, user: &str, after: i64, wait_secs: Option<u64>) -> Result<Feed> {
        let rest = match wait_secs {
            Some(w) => format!("changes?after={after}&wait={w}"),
            None => format!("changes?after={after}"),
        };
        let url = self.url(user, &rest);
        let timeout = wait_secs.map(|w| Duration::from_secs(w + 15));
        let resp = Self::with_gateway_retry("feed", || {
            let req = self.auth(self.agent.get(&url));
            match timeout {
                Some(t) => req.config().timeout_global(Some(t)).build().call(),
                None => req.call(),
            }
        })?;
        let mut v = Self::finish(resp, "feed")?;
        let last_seq = v["last_seq"].as_i64().ok_or_else(|| anyhow!("orchestrator feed: no last_seq"))?;
        let changes = serde_json::from_value(v["changes"].take())?;
        Ok(Feed {
            last_seq,
            changes,
            seed_id: v["seed_id"].as_str().map(str::to_string),
            notices_degraded_secs: v["notices_degraded_secs"].as_u64(),
            waited: v["waited"].as_bool() == Some(true),
        })
    }

    /// Answers broker request `id` (`POST /users/<u>/broker/reply/<id>`).
    pub fn broker_reply(&self, user: &str, id: u64, reply: &serde_json::Value) -> Result<()> {
        let rest = format!("broker/reply/{id}");
        let resp = self.auth(self.agent.post(&self.url(user, &rest))).send_json(reply).context("orchestrator broker reply")?;
        Self::finish_text(resp, "broker reply")
    }

    fn finish_text(mut resp: ureq::http::Response<ureq::Body>, what: &str) -> Result<()> {
        let status = resp.status();
        if !status.is_success() {
            let body: String = resp.body_mut().read_to_string().unwrap_or_default().chars().take(400).collect();
            bail!("orchestrator {what}: {status}: {body}");
        }
        Ok(())
    }

    fn url(&self, user: &str, rest: &str) -> String {
        format!("{}/users/{user}/{rest}", self.base)
    }

    fn auth<B>(&self, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        let mut req = req.header(sync::CLIENT_HEADER, &self.client);
        for (name, value) in my_contract().headers() {
            req = req.header(name, &value);
        }
        match &self.token {
            Some(t) => req.header("Authorization", &format!("Bearer {t}")),
            None => req,
        }
    }

    /// Sends the request `make` builds, again after a pause while the gateway
    /// answers 502/503/504 ("could not proxy the request to the workload":
    /// the orchestrator's sandbox is waking or restarting), up to
    /// [`GATEWAY_ATTEMPTS`] times. Only for requests that are safe to repeat:
    /// the feed and problem pulls read, and an outbox replays harmlessly (the
    /// orchestrator skips a row that already holds the sender's `after`, and
    /// the sent cursor moves only on success). The last reply is returned for
    /// [`Self::finish`] to report.
    fn with_gateway_retry(
        what: &str,
        make: impl Fn() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<ureq::http::Response<ureq::Body>> {
        for attempt in 1.. {
            let resp = make().with_context(|| format!("orchestrator {what}"))?;
            if matches!(resp.status().as_u16(), 502 | 503 | 504) && attempt < GATEWAY_ATTEMPTS {
                let wait = Duration::from_secs(1 << attempt);
                tracing::info!("orchestrator {what}: {} from the gateway; retrying in {wait:?} (attempt {attempt} of {GATEWAY_ATTEMPTS})", resp.status());
                std::thread::sleep(wait);
                continue;
            }
            return Ok(resp);
        }
        unreachable!()
    }

    fn finish(mut resp: ureq::http::Response<ureq::Body>, what: &str) -> Result<serde_json::Value> {
        let status = resp.status();
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        if !status.is_success() {
            // The raw reply goes to the error log; the error carries what it says.
            tracing::debug!("orchestrator {what}: {status}: {body}");
            bail!("orchestrator {what}: {}", tod_sandbox::blaxel::describe_failure(status, &body));
        }
        serde_json::from_str(&body).with_context(|| format!("orchestrator {what}: not JSON"))
    }
}

impl Orchestrator for HttpOrchestrator {
    fn put_github_routes(&self, user: &str, routes: &tod_store::github_routes::GithubRoutes) -> Result<()> {
        HttpOrchestrator::put_github_routes(self, user, routes)
    }

    fn check_contract(&self) -> Result<()> {
        use tod_store::sync::contract::{Verdict, judge, newer_message};
        if self.contract_ok.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        let (mine, theirs) = (my_contract(), self.health()?);
        match judge(&mine, &theirs) {
            Verdict::Newer => bail!("{}", newer_message(&mine, &theirs)),
            Verdict::Older => tracing::info!(
                "the orchestrator is older than this app ({}; this app is {}); the app redeploys it at launch",
                theirs.describe(),
                mine.describe()
            ),
            Verdict::Compatible => {}
        }
        self.contract_ok.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn seed(&self, user: &str, snapshot: &[u8]) -> Result<Seeding> {
        // The orchestrator answers 409 while the user's store is in use; a
        // 409 for another client's seeding is not retried.
        for attempt in 0..5 {
            let mut req = self
                .auth(self.agent.post(&self.url(user, "seed")))
                .header("Content-Type", "application/octet-stream");
            if self.take_over {
                req = req.header("X-Tod-Take-Over", "yes");
            }
            let mut resp = req.send(snapshot).context("orchestrator seed")?;
            if resp.status().as_u16() == 409 {
                let body = resp.body_mut().read_to_string().unwrap_or_default();
                if body.contains("store is in use") && attempt < 4 {
                    std::thread::sleep(Duration::from_millis(500 * (attempt + 1)));
                    continue;
                }
                bail!(
                    "orchestrator seed: {}",
                    tod_sandbox::blaxel::describe_failure(resp.status(), &body)
                );
            }
            let v = Self::finish(resp, "seed").map_err(older_orchestrator_hint)?;
            let last_seq = v["last_seq"].as_i64().ok_or_else(|| anyhow!("orchestrator seed: no last_seq"))?;
            return Ok(Seeding { last_seq, seed_id: v["seed_id"].as_str().map(str::to_string) });
        }
        unreachable!()
    }

    fn send(&self, user: &str, changes: &[Change]) -> Result<ApplyReport> {
        let resp = Self::with_gateway_retry("changes", || self.auth(self.agent.post(&self.url(user, "changes"))).send_json(changes))?;
        let v = Self::finish(resp, "changes").map_err(older_orchestrator_hint)?;
        Ok(serde_json::from_value(v)?)
    }

    fn pull(&self, user: &str, after: i64) -> Result<Feed> {
        self.pull_feed(user, after, None)
    }

    fn pull_problems(
        &self,
        user: &str,
        cursor: Option<&tod_problems::Cursor>,
    ) -> Result<(tod_problems::Cursor, Vec<tod_problems::Problem>)> {
        let body = serde_json::json!({ "cursor": cursor });
        let resp = Self::with_gateway_retry("problems", || self.auth(self.agent.post(&self.url(user, "problems/pull"))).send_json(&body))?;
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
    /// The nodes the feed changed something of (other than the record of
    /// where they run, which the orchestrator also marks): proof that a
    /// node's supervisor, or the orchestrator for it, has been heard from.
    pub heard_from: std::collections::BTreeSet<uuid::Uuid>,
    /// The feed's `notices_degraded_secs`; see [`Feed`].
    pub notices_degraded_secs: Option<u64>,
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
    orch.check_contract()?;
    match sync_once(fleet, root, orch, user) {
        Err(err) if format!("{err:#}").contains(NOT_SEEDED) => {
            // The orchestrator's data is gone (a redeploy without its volume):
            // what the app remembers sending is not there. Seed it again.
            tracing::warn!("cloud sync: {err:#}; seeding it again");
            {
                let _guard = sync_lock(root)?;
                let mut state = CloudSyncState::load(root)?;
                state.seeded = false;
                state.save(root)?;
            }
            sync_once(fleet, root, orch, user)
        }
        other => other,
    }
}

/// What the orchestrator says (409) to sync traffic for a user it holds no
/// seeded database for (`tod_orchestrator::sync_backend::NOT_SEEDED`).
pub const NOT_SEEDED: &str = "the orchestrator has not been seeded with this user's database";

fn sync_once(fleet: &FleetStore, root: &Path, orch: &dyn Orchestrator, user: &str) -> Result<SyncReport> {
    let _guard = sync_lock(root)?;
    let mut state = CloudSyncState::load(root)?;
    let mut report = SyncReport::default();
    let _ = fleet.flush_on_quit();
    let db = fleet.paths().db().to_path_buf();

    // Which seeding the orchestrator is on, from its feed (no changes are
    // asked for). A seeding this copy was not sent to replaced the database
    // the cursors follow: take what it holds, then seed it with this one.
    if state.seeded {
        let now = orch.pull(user, i64::MAX)?.seed_id;
        match (state.seed_id.clone(), now) {
            (Some(known), Some(now)) if known != now => {
                tracing::warn!(
                    known,
                    now,
                    "the orchestrator's database was seeded again; taking what it holds, then seeding it with this database"
                );
                let taken = orch.pull(user, 0)?;
                let mut conn = connect(&db)?;
                let applied = sync::apply_changes(&mut conn, &taken.changes)?;
                drop(conn);
                report.received += taken.changes.len();
                report.conflicts += applied.conflicts.len();
                state.seeded = false;
                state.seed_id = None;
                state.save(root)?;
            }
            (None, now @ Some(_)) => {
                // Copied before the seeding was recorded: from now on it is.
                state.seed_id = now;
                state.save(root)?;
            }
            _ => {}
        }
    }

    if !state.seeded {
        let tmp = root.join("cloud-seed-snapshot.db");
        let _ = std::fs::remove_file(&tmp);
        sync::snapshot(&db, &tmp).context("snapshot the database")?;
        let result = (|| {
            let snap_seq = sync::last_seq(&connect(&tmp)?)?;
            let bytes = std::fs::read(&tmp)?;
            let feed = orch.seed(user, &bytes)?;
            // A seeded orchestrator may be a fresh one (its disk was reset):
            // it knows no GitHub routes until told, and its gate would read
            // every pull request without the token that can see it.
            let github = tod_store::orchestrator_github::Config::load(root)?;
            if !github.tokens.is_empty() {
                orch.put_github_routes(user, &github.routes())
                    .context("seeded the orchestrator, but could not give it the GitHub routes")?;
            }
            Ok::<_, anyhow::Error>((snap_seq, feed))
        })();
        let _ = std::fs::remove_file(&tmp);
        let (snap_seq, seeding) = result?;
        state.seeded = true;
        state.seed_id = seeding.seed_id;
        state.sent_after = snap_seq;
        state.feed_after = seeding.last_seq;
        state.save(root)?;
        report.seeded = true;
    }

    let outbox = sync::export_changes(&connect(&db)?, state.sent_after)?;
    if !outbox.is_empty() {
        let applied = orch.send(user, &outbox)?;
        report.sent = outbox.len();
        report.conflicts += applied.conflicts.len();
        record_conflicts(&applied.conflicts);
        record_rejects(&applied);
        record_dropped_revivals("the orchestrator", &applied);
        state.sent_after = outbox.iter().map(|c| c.seq).max().unwrap_or(state.sent_after);
        state.save(root)?;
    }

    // What the orchestrator has accepted is not read again, and each entry
    // holds a whole old row: drop them (also the ones a seeding covered).
    let pruned = sync::prune_sent(&connect(&db)?, state.sent_after).context("prune the sent change log")?;
    if pruned > 0 {
        tracing::debug!(pruned, upto = state.sent_after, "cloud sync: pruned sent changes from the outbox");
    }

    let pulled = orch.pull(user, state.feed_after)?;
    if let (Some(known), Some(now)) = (&state.seed_id, &pulled.seed_id)
        && known != now
    {
        // Seeded again since the check above: nothing from it is applied, and
        // the next sync takes it (what was sent just now is in the copy it
        // makes then).
        anyhow::bail!("the orchestrator's database was seeded again during this sync; the next sync takes it");
    }
    let last = pulled.last_seq;
    report.notices_degraded_secs = pulled.notices_degraded_secs;
    let feed = pulled.changes;
    if !feed.is_empty() {
        let mut conn = connect(&db)?;
        let applied = sync::apply_changes(&mut conn, &feed)?;
        drop(conn);
        report.received = feed.len();
        report.heard_from = feed
            .iter()
            .filter(|c| c.table != tod_store::cloud_nodes::TABLE)
            .filter_map(|c| c.node_id)
            .collect();
        report.conflicts += applied.conflicts.len();
        record_dropped_revivals("the orchestrator's feed", &applied);
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
    // A done node's sandbox is deleted by the daemon (`sandbox_retire`), not here.
    if let Err(err) = lost::retire_done(fleet) {
        tracing::warn!("cloud sync: retiring done nodes: {err:#}");
    }
    Ok(report)
}

/// A change the orchestrator refused is skipped there (so the batch and the
/// cursor still advance), and so the user has to be told: it is a sign the
/// orchestrator is older than this app. Each distinct error is raised once per
/// process, as an error (a toast) with the hint, and logged at warn level.
fn record_rejects(applied: &sync::ApplyReport) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    if applied.rejected == 0 {
        return;
    }
    for error in &applied.reject_errors {
        let fresh = SEEN.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(error.clone());
        if !fresh {
            continue;
        }
        let hinted = format!("{:#}", older_orchestrator_hint(anyhow!("{error}")));
        tracing::warn!(rejected = applied.rejected, "cloud sync: the orchestrator refused changes: {hinted}");
        crate::logging::buffer::record(
            crate::logging::buffer::Level3::Error,
            crate::logging::buffer::LogSource::App,
            format!(
                "The orchestrator refused {} of this copy's changes and skipped them (they are not synced): {hinted}",
                applied.rejected
            ),
        );
    }
}

/// A late update of a `cloud_nodes` row this side had removed (the node left
/// its sandbox) was dropped, not applied (`tod_store::sync`): said, once per
/// sync, so a sandbox that is still pushing for a node that left it is seen.
fn record_dropped_revivals(from: &str, applied: &sync::ApplyReport) {
    if applied.revival_dropped.is_empty() {
        return;
    }
    let nodes: Vec<String> = applied.revival_dropped.iter().map(|n| n.to_string()).collect();
    tracing::warn!(?nodes, "cloud sync: dropped a late update of a removed cloud node row from {from}");
    crate::logging::buffer::record(
        crate::logging::buffer::Level3::Warn,
        crate::logging::buffer::LogSource::App,
        format!(
            "A late update from {from} for node(s) {} would have recorded them in a sandbox again after they left it; it was dropped, and the removal is sent on. Something in the cloud is still reporting for them.",
            nodes.join(", ")
        ),
    );
}

/// Logs the changes the orchestrator reported as conflicting with its copy.
/// Each was applied over the orchestrator's row, or dropped when its parent
/// is not there, so the user has to be able to see it, not only the count.
fn record_conflicts(conflicts: &[sync::Conflict]) {
    if conflicts.is_empty() {
        return;
    }
    let mut tables: Vec<&str> = conflicts.iter().map(|c| c.table.as_str()).collect();
    tables.sort_unstable();
    tables.dedup();
    crate::logging::buffer::record(
        crate::logging::buffer::Level3::Warn,
        crate::logging::buffer::LogSource::App,
        format!(
            "The orchestrator reported {} of this copy's changes as conflicting ({}): each was applied over its row, or dropped when its parent is missing there.",
            conflicts.len(),
            tables.join(", ")
        ),
    );
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
                 Redeploy it with scripts/deploy.sh to see the cloud's problems here. Syncing otherwise works; this is not repeated until tod restarts.",
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
    let user = user_of(&state, sandboxes.as_ref().ok())?;
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

/// The user name the orchestrator knows this data root as, without finding
/// the orchestrator (what a sandbox's environment needs; [`resolve`] also finds it).
pub fn resolve_user(root: &Path) -> Result<String> {
    let state = CloudSyncState::load(root)?;
    let sandboxes = tod_store::fleet::sandbox::Sandboxes::load(root);
    user_of(&state, sandboxes.as_ref().ok())
}

fn user_of(state: &CloudSyncState, sandboxes: Option<&tod_store::fleet::sandbox::Sandboxes>) -> Result<String> {
    let owner = sandboxes.and_then(|s| s.config.blaxel.as_ref()).and_then(|a| a.owner.clone());
    state
        .user
        .clone()
        .or(owner)
        .or_else(|| std::env::var("USERNAME").or_else(|_| std::env::var("USER")).ok())
        .and_then(|u| user_name(&u))
        .ok_or_else(|| anyhow!("no user name for the orchestrator: set `user` in {STATE_FILE}"))
}

/// [`sync`] with the resolved orchestrator: the "Sync with the cloud" action.
pub fn sync_now(fleet: &FleetStore, root: &Path) -> Result<SyncReport> {
    let (orch, user) = resolve(root)?;
    let result = sync(fleet, root, &orch, &user);
    if result.is_err() {
        // The orchestrator may be down (its container reset): see whether it
        // needs starting again, without blocking this call on it.
        deploy::heal_after_failed_sync(root);
    }
    result
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

/// The HTTPS URL of the repository recorded on the node's cloud row
/// (`owner/name`, `tod_store::cloud_nodes::Ownership::repo`), if there is one.
fn recorded_repo_url(fleet: &FleetStore, node_id: &str) -> Option<String> {
    let node: uuid::Uuid = node_id.parse().ok()?;
    let row = fleet.read(|conn| tod_store::cloud_nodes::get(conn, node)).ok().flatten()?;
    let repo = row.ownership.repo.as_deref()?.trim();
    (repo.split('/').count() == 2 && !repo.starts_with('/') && !repo.ends_with('/')).then(|| format!("https://github.com/{repo}.git"))
}

/// The HTTPS origin of a node whose Files directory is not a URL: a checkout
/// on this machine says it itself; a path inside a cloud sandbox (Files →
/// Runs in → Cloud sandbox) is asked in the node's own sandbox, else in the
/// sandbox its files are forked from. The error says what to set instead.
fn origin_url_of(fleet: &FleetStore, node_id: &str, repo: &str) -> Result<String> {
    let mut tried: Vec<String> = Vec::new();
    let mut golden_missing = false;
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
        // The repository the node's cloud row records (from an earlier run) says
        // it without asking a sandbox, which may be gone: the case this is for.
        if let Some(url) = recorded_repo_url(fleet, node_id) {
            return Ok(url);
        }
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
        // `golden` is an alias for the repository's golden, not a sandbox name:
        // one that cannot be resolved (none, or several: the location must name
        // one) is not asked, and the error says why.
        let mut resolved = Vec::new();
        for name in sandboxes {
            if name != tod_sandbox::config::GOLDEN {
                resolved.push(name);
                continue;
            }
            let root = fleet.paths().root();
            match tod_store::fleet::sandbox::resolve_fork_source(root, &name) {
                Ok(real) => resolved.push(real),
                Err(err) => {
                    golden_missing = true;
                    tried.push(format!(
                        "this node's files are forked from the golden sandbox, which cannot be found: {err:#}. \
                         Make one with `tod-sandbox golden refresh <repo>`, or point tod at one with `tod-sandbox golden <repo> <sandbox>`"
                    ));
                }
            }
        }
        // The repository each golden holds is on the golden itself (its
        // `tod-repo` label), read from Blaxel's listing: no sandbox is started
        // or asked, and none but the node's own golden is looked at.
        let mut found: Vec<String> = Vec::new();
        for name in resolved {
            match golden_repo(fleet, &name) {
                Ok(repo_slug) => found.push(format!("https://github.com/{repo_slug}.git")),
                Err(err) => tried.push(format!("{err:#}")),
            }
        }
        found.sort();
        found.dedup();
        match found.as_slice() {
            [] => {}
            [url] => return Ok(url.clone()),
            many => tried.push(format!("the goldens hold different repositories ({}): say which the node's files are", many.join(", "))),
        }
    }
    let why = if tried.is_empty() { format!("{repo} is not a checkout on this machine") } else { tried.join("; ") };
    if golden_missing {
        // The Files directory is not at fault; do not send the user to change it.
        bail!("no HTTPS repository URL for the cloud run: {why}");
    }
    bail!("no HTTPS repository URL for the cloud run ({why}); {}", what_to_set(fleet, node_id))
}

/// The repository the golden sandbox `name` holds, from its `tod-repo` label.
/// A golden without one is an error saying how to give it one.
fn golden_repo(fleet: &FleetStore, name: &str) -> Result<String> {
    let sandboxes = tod_store::fleet::sandbox::Sandboxes::load(fleet.paths().root())?;
    let bx = sandboxes.blaxel()?;
    let info = bx.get(name)?.ok_or_else(|| anyhow!("the golden sandbox {name} does not exist"))?;
    tod_sandbox::golden::info_repo(&info)?.ok_or_else(|| {
        anyhow!(
            "the golden sandbox {name} does not say which repository it holds (it has no {} label): \
             run `tod-sandbox golden <owner/name> {name}` to record it, or `tod-sandbox golden refresh <owner/name>`",
            tod_sandbox::golden::LABEL_REPO
        )
    })
}

/// What the user can do about a repository tod cannot find the URL of, by how
/// the node's Files location gets its files.
fn what_to_set(fleet: &FleetStore, node_id: &str) -> String {
    let dev = fleet.resolve_files_for_node(node_id).ok().flatten().and_then(|files| files.dev_container);
    match dev {
        Some(dev) if dev.sandbox && matches!(dev.sandbox_from, tod_store::fleet::SandboxFrom::Image(_)) => {
            "the cloud clones the repository from its GitHub HTTPS URL: open Locations, edit this node's \
             location, and give the repository (e.g. https://github.com/owner/repo) under \"Repository to clone\""
                .into()
        }
        Some(dev) if dev.sandbox => "the sandbox is forked from a golden whose checkout must have a GitHub \
             remote: in Locations, choose a directory inside the golden that is a git checkout, or refresh the \
             golden (`tod-sandbox golden refresh <repo>`)"
            .into(),
        _ => "the cloud checks the repository out from its HTTPS URL, so set the Files directory (in \
              Locations) to a checkout on this machine whose origin is on GitHub, or to that URL (e.g. \
              https://github.com/owner/repo)"
            .into(),
    }
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
    // A location that makes sandboxes from an image says where to clone from:
    // an empty image has no checkout to ask.
    let clone_url = fleet
        .resolve_files_for_node(node_id)
        .ok()
        .flatten()
        .and_then(|files| files.dev_container)
        .and_then(|dev| dev.clone_url().map(str::to_string));
    for repo in [clone_url, inherited, task.repo.clone()].into_iter().flatten() {
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
pub(crate) fn cloud_agent() -> Option<String> {
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

/// Why Blaxel's record of a sandbox says it can no longer run, if it does:
/// its deployment `status` or its running `state` is a terminal one. A
/// sandbox that expired can stay `DEPLOYED` for an hour while its `state`
/// already says so.
pub fn sandbox_dead_reason(info: &tod_sandbox::blaxel::SandboxInfo) -> Option<String> {
    const DEAD: [&str; 6] = ["FAILED", "TERMINATED", "DELETING", "DELETED", "EXPIRED", "EXPIRING"];
    let field = |what: &str, value: &str| {
        DEAD.contains(&value.to_ascii_uppercase().as_str()).then(|| format!("sandbox {} has {what} {value}", info.name))
    };
    field("status", &info.status).or_else(|| info.state.as_deref().and_then(|state| field("state", state)))
}

/// Whether Blaxel's record of a sandbox says it can no longer run.
pub fn sandbox_is_dead(info: &tod_sandbox::blaxel::SandboxInfo) -> bool {
    sandbox_dead_reason(info).is_some()
}

/// Why a node's recorded sandbox cannot serve this app, when it was made for
/// another orchestrator user than the one this app syncs as (the root's owner
/// changed since): its supervisor would read that other user's database, never
/// find the node, and exit.
pub fn sandbox_user_reason(sandbox: &str, recorded_user: &str, user: &str) -> Option<String> {
    (recorded_user != user).then(|| {
        format!("sandbox {sandbox} was made for orchestrator user {recorded_user}, but this app syncs as {user}")
    })
}

/// How long a wake of a node's supervisor waits for a sandbox in standby to
/// answer. Blaxel answers 404 `WORKLOAD_UNAVAILABLE` (and 502-504) while one
/// wakes; that is an outage to wait out, never a dead sandbox.
pub const WAKE_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

/// What a wake does about the sandbox Blaxel reports for a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeVerdict {
    /// Use it. Whether it answers yet is a bounded wait ([`WAKE_WAIT`]) whose
    /// failure is an error, never a reason to replace it.
    Use,
    /// Replace it, for this reason: Blaxel has no such sandbox (a 404 on the
    /// sandbox itself, not on its workload), or reports it terminal, or it was
    /// made for another orchestrator user.
    Replace(String),
}

/// Decides [`WakeVerdict`] from Blaxel's record `found` for the node's
/// `sandbox` (`None`: Blaxel has none by that name), and `user_reason`
/// ([`sandbox_user_reason`]). Pure: a sandbox that is merely not answering
/// (standby, waking, `WORKLOAD_UNAVAILABLE`) is `Use`.
pub fn wake_verdict(sandbox: &str, found: Option<&tod_sandbox::blaxel::SandboxInfo>, user_reason: Option<String>) -> WakeVerdict {
    if let Some(reason) = user_reason {
        return WakeVerdict::Replace(reason);
    }
    match found {
        None => WakeVerdict::Replace(format!("Blaxel has no sandbox {sandbox}")),
        Some(info) => sandbox_dead_reason(info).map_or(WakeVerdict::Use, WakeVerdict::Replace),
    }
}

/// The error for a sandbox that did not answer within [`WAKE_WAIT`]: names
/// what failed and says nothing was replaced or deleted.
pub fn still_waking_error(sandbox: &str, kind: &str, err: &anyhow::Error) -> anyhow::Error {
    anyhow!(
        "sandbox {sandbox} is still waking after {}s ({err:#}). It was not replaced or deleted; \
         the {kind} request is recorded and the supervisor takes it once the sandbox answers. Try again in a minute",
        WAKE_WAIT.as_secs()
    )
}

/// What [`ensure_supervisor`] finds a node's sandbox needs to host the
/// supervisor of this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorState {
    /// It answers with this build's relay and was given this build's install.
    Current,
    /// It lacks them (an old sandbox, or one an older build installed into):
    /// install this build's files, for this reason.
    Install { why: String },
    /// Made just now and still not answering with this build's relay: the
    /// golden it was forked from does not carry what it should. An error, not
    /// something to patch over by installing here.
    BrokenNew,
}

/// Decides [`SupervisorState`] from `made_now` (this call made the sandbox),
/// the install stamp last `recorded` for it, this build's `current` one, and
/// whether its relay `healthy` (answered with this build's manifest).
pub fn supervisor_state(made_now: bool, recorded: Option<&str>, current: &str, healthy: bool) -> SupervisorState {
    if made_now {
        return if healthy { SupervisorState::Current } else { SupervisorState::BrokenNew };
    }
    if !healthy {
        return SupervisorState::Install { why: "it does not answer with this build's relay".to_string() };
    }
    match recorded {
        Some(stamp) if stamp == current => SupervisorState::Current,
        Some(_) => SupervisorState::Install { why: "it was last given an older install".to_string() },
        None => SupervisorState::Install { why: "no install was recorded for it".to_string() },
    }
}

/// Make sure `node_id` has a sandbox that can host its supervisor, of this
/// build, and (with `wake`) poke it. Returns the sandbox's name. The one path
/// Start, replacing a lost sandbox, a reinstall and Reset environment take;
/// it makes nothing itself: the sandbox comes from
/// [`tod_store::fleet::provision::resolve_launch_cwd_with`] (`make_sandbox`, the one creator).
///
/// 1. [`tod_store::fleet::provision::retire_unusable_sandbox`]: a recorded sandbox that is gone
///    or dead is forgotten, and one made before every sandbox could host the
///    supervisor is **replaced once** (logs saved, work checkpointed, deleted;
///    a failure of either deletes nothing). This is the upgrade path.
/// 2. The sandbox is made or found; a sandbox made for another orchestrator
///    user is refused (it holds that user's database).
/// 3. Its relay is asked for this build's manifest. A sandbox that was already
///    there and does not answer with it, or was given an older install
///    ([`supervisor_state`]), gets this build's files and a restarted relay.
///    One made just now never does: it carries them from the golden, so a miss
///    is an error.
/// 4. The supervisor's settings are sent, and with `wake` it is poked.
///
/// Blocks on the network: never on the UI thread.
pub fn ensure_supervisor(
    fleet: &FleetStore,
    root: &Path,
    node_id: &str,
    wake: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<String> {
    use tod_sandbox::node;
    register_sandbox_host();
    // One provisioning of a node at a time, across the app and the daemon:
    // two would fork the same name or start two relays.
    let _provisioning = provisioning::lock_node_provisioning(root, node_id, progress)?;
    let (task, repo_url, branch) = node_source(fleet, node_id)?;
    // Before any network call: without it the sandbox could not run Claude.
    let claude_oauth_token = claude_token_for_node(root)?;
    let user = resolve_user(root)?;

    let retired = tod_store::fleet::provision::retire_unusable_sandbox(fleet, node_id, progress)?;
    if let tod_store::fleet::provision::Retired::Replaced(old) | tod_store::fleet::provision::Retired::Upgraded(old) = &retired {
        progress(&format!("{old} is replaced by a new sandbox"));
    }
    let known = tod_store::fleet::provision::recorded_sandboxes(fleet, node_id)?;
    let (dir, warnings) = tod_store::fleet::provision::resolve_launch_cwd_with(fleet, node_id, progress)?;
    for warning in warnings {
        progress(&format!("warning: {warning}"));
    }
    let name = dir
        .sandbox_name()
        .ok_or_else(|| anyhow!("node {} does not run in a sandbox: its Files setting must make one", task.slug))?
        .to_string();
    let made_now = !known.contains(&name);

    let sandboxes = tod_store::fleet::sandbox::Sandboxes::load(root)?;
    let bx = sandboxes.blaxel()?;
    let info = bx.get(&name)?.ok_or_else(|| anyhow!("sandbox {name} no longer exists"))?;
    // A sandbox made for another orchestrator user would read that user's
    // database and never find the node. It may hold the node's work, so it is
    // not deleted here: say so, and let the user's Delete Sandbox checkpoint it.
    if let Some(why) = sandbox_user_reason(&name, info.env("TOD_USER").unwrap_or(&user), &user) {
        bail!("{why}; use Delete Sandbox in the task panel (it saves the work first), then run the node again");
    }
    let url = info.url.clone().ok_or_else(|| anyhow!("sandbox {name} has no URL"))?;
    progress(&format!("waking {name}…"));
    bx.wait_reachable(&url, Duration::from_secs(180)).with_context(|| format!("wake {name}"))?;

    let files = refresh::install_files()?;
    let current = files.install();
    let expected_manifest = tod_sandbox::provision::node_manifest(&files.relay);
    let target = HealthTarget { label: &name, node: node_id, repo_url: &repo_url, url: &url };
    let ready = |progress: &mut dyn FnMut(&str)| {
        probe_health(
            &bx,
            &target,
            tod_sandbox::health::Stage::Ready,
            INSTALL_MIN_FREE,
            Some(&expected_manifest),
            node::IMAGE_PROBE_DELAYS,
            progress,
        )
    };
    let healthy = ready(progress);
    let recorded = refresh::recorded_stamp(root, node_id)?;
    match supervisor_state(made_now, recorded.as_deref(), &current.stamp, healthy.is_ok()) {
        SupervisorState::Current => {}
        SupervisorState::BrokenNew => {
            let failure = healthy.err().map(|f| format!("{f}")).unwrap_or_default();
            bail!(
                "{name} was just made from the golden, but it does not answer with this build's relay ({failure}). \
                 The golden does not carry this build's files: refresh it with `tod-sandbox golden refresh`"
            );
        }
        SupervisorState::Install { why } => {
            progress(&format!("{name} gets this build's relay and supervisor: {why}…"));
            let identity = git_identity(task.repo.as_deref());
            let credentials = node::NodeCredentials { claude_oauth_token, ..Default::default() };
            let claude_via = sandboxes.claude_token_via();
            let supervisor_env = node::supervisor_env(&credentials, claude_via);
            let payload = node::NodePayload {
                relay: &files.relay,
                shim: tod_store::fleet::cli_relay::HTTP_SHIM_SCRIPT.as_bytes(),
                local_cli: files.local_cli.as_deref(),
                supervisor: files.supervisor.as_deref(),
                repo_url: &repo_url,
                branch: &branch,
                git_identity: &identity,
                bundles: &files.bundles,
                supervisor_env: &supervisor_env,
            };
            node::install_into_fork(&bx, &url, &payload, progress)?;
            node::start_relay(&bx, &url, &supervisor_env)?;
            // Adopted as good only now: the relay answers with the manifest just installed.
            ready(progress).with_context(|| format!("{name} was given this build's files but is still not healthy"))?;
            refresh::record(root, node_id, &current, Some(&why))?;
        }
    }

    push_supervisor_settings(&bx, &url, root, node_id)?;
    if wake {
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
    }
    Ok(name)
}

/// Free space a provisioning needs on a node sandbox's root filesystem.
const INSTALL_MIN_FREE: u64 = 128 * 1024 * 1024;

/// The sandbox a health probe looks at.
pub struct HealthTarget<'a> {
    pub label: &'a str,
    pub node: &'a str,
    pub repo_url: &'a str,
    pub url: &'a str,
}

/// Probes a node's sandbox ([`tod_sandbox::health`]): reachable, disk free,
/// relay up, manifest sane (equal to `expected_manifest` when given), the
/// node's environment, and `git ls-remote` through the proxy. The failure
/// names the check that failed. `delays` are the seconds to wait between
/// tries of the checks a fresh fork or a restarted relay passes late.
pub fn probe_health(
    bx: &tod_sandbox::blaxel::Blaxel,
    target: &HealthTarget,
    stage: tod_sandbox::health::Stage,
    min_free: u64,
    expected_manifest: Option<&str>,
    delays: &[u64],
    progress: &mut dyn FnMut(&str),
) -> Result<(), tod_sandbox::health::HealthFailure> {
    let spec = tod_sandbox::health::HealthSpec {
        label: target.label,
        node: target.node,
        repo_url: target.repo_url,
        min_free,
        reachable_wait: Duration::from_secs(180),
        expected_manifest,
        probe_delays: delays,
    };
    tod_sandbox::health::probe(bx, target.url, stage, &spec, progress)
}

/// The workspace, this data root's client id and the repository (`owner/name`)
/// a node being recorded belongs to.
fn ownership_of(fleet: &FleetStore, node_id: &str) -> Result<tod_store::cloud_nodes::Ownership> {
    let root = fleet.paths().root();
    let workspace = tod_store::fleet::sandbox::Sandboxes::load(root)?.account()?.workspace.clone();
    let (_, repo_url, _) = node_source(fleet, node_id)?;
    let repo = tod_store::cloud_nodes::normalize_repo(&repo_url)
        .ok_or_else(|| anyhow!("cannot read an owner and repository name from {repo_url}"))?;
    Ok(tod_store::cloud_nodes::Ownership {
        workspace: Some(workspace),
        owner_client: Some(client_id(root)?),
        repo: Some(repo),
    })
}

/// Records that `node_id` runs in the cloud (clearing any lost mark) in the
/// database, which is synced: the orchestrator needs to know the node is
/// active (`tod_core::impact`), and its supervisor reads its context mark
/// there. The sandbox is not an argument: the row's copy of it is taken from
/// the node's Files location by the writer, which fails when the location
/// names none.
fn record_cloud_node(fleet: &FleetStore, node_id: &str, user: &str) -> Result<CloudNode> {
    let root = fleet.paths().root();
    set_sandbox_deleted_by_user(root, node_id, false)?;
    // Recorded now, so the sweeper no longer needs the intent to know the
    // sandbox is the node's.
    let record = CloudNode {
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
        user: record.user.clone(),
        accepted_at: record.accepted_at_ms,
    })?;
    // Whose it is (robustness plan 3.10): the orchestrator routes the
    // broker, webhooks, and the seed by it. A failure to say is an error,
    // not a row left unknown.
    fleet.enqueue(FleetMutation::CloudNodeSetOwnership { node: node_uuid, ownership: ownership_of(fleet, node_id)? })?;
    fleet.writer().flush()?;
    let _ = fleet.reload_if_stale();
    provisioning::clear_intent(root, node_id)?;
    Ok(record)
}

/// Run `node_id` in the cloud: sync (seeding first), make its sandbox
/// ([`ensure_supervisor`]), and record it. `progress` hears each step.
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
    run_in_cloud_reporting(fleet, root, node_id, progress).map(|(record, _)| record)
}

/// [`run_in_cloud`], and what the final sync heard of the node: its
/// supervisor may already be at work by then.
fn run_in_cloud_reporting(
    fleet: &FleetStore,
    root: &Path,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<(CloudNode, SyncReport)> {
    // Fails early, before any network call, on a node with no repository or
    // no Claude token to run it with.
    node_source(fleet, node_id)?;
    claude_token_for_node(root)?;
    lost::forget_failure(node_id);
    // "Run": a row the supervisor consumes when it starts, which also
    // supersedes an older answer to leave the node stopped.
    record_node_request(fleet, node_id, REQUEST_RUN)?;
    record_starting(fleet, node_id)?;
    failing_start_is_recorded(fleet, node_id, || {
        progress("syncing with the orchestrator…");
        let (orch, user) = resolve(root)?;
        sync(fleet, root, &orch, &user)?;
        ensure_supervisor(fleet, root, node_id, true, progress)?;
        let record = record_cloud_node(fleet, node_id, &user)?;
        let heard = match sync(fleet, root, &orch, &user) {
            Ok(report) => report,
            Err(err) => {
                progress(&format!("warning: could not send the record to the orchestrator yet: {err:#}"));
                SyncReport::default()
            }
        };
        Ok((record, heard))
    })
}

/// Runs `step`, the work after [`record_starting`]. When it fails the node's
/// `starting` row would go on saying "working: starting in the cloud" for the
/// whole time a supervisor is given to report in, though nothing is coming;
/// so the failure replaces it (`runner_failed`, the user's to resume), and the
/// original error is returned either way.
fn failing_start_is_recorded<T>(fleet: &FleetStore, node_id: &str, step: impl FnOnce() -> Result<T>) -> Result<T> {
    match step() {
        Ok(v) => Ok(v),
        Err(err) => {
            let node = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
            let text = format!("starting in the cloud failed: {err:#}");
            match crate::run_state::record(fleet, node, crate::run_state::runner_failed(&text), "app") {
                Ok(_) => Err(err),
                Err(rec) => Err(err.context(format!("(and the failure could not be recorded on the node: {rec:#})"))),
            }
        }
    }
}

/// How long after sending a node to the cloud its supervisor has to report
/// in (sandbox boot, seeding its copy, starting the autopilot) before the
/// user is told it did not.
pub const SUPERVISOR_REPORTS_WITHIN: Duration = Duration::from_secs(150);
/// How often the orchestrator is pulled from while waiting for it.
const REPORT_POLL: Duration = Duration::from_secs(4);

/// What starting a node in the cloud came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub node: CloudNode,
    /// Its supervisor was heard from (a heartbeat, or any change it made)
    /// within [`SUPERVISOR_REPORTS_WITHIN`].
    pub heard: bool,
}

/// Whether the cloud side has told this app anything about `node` since it
/// was sent: a heartbeat is there now, or `report` carried a change to it.
///
/// A row left by an earlier run does not count: it must be stamped at or
/// after `since_ms` (less [`CLOCK_SKEW_MS`]). A run that only found the node
/// waiting leaves an idle row (`cloud_activity::idle`), which is a report.
fn supervisor_heard(fleet: &FleetStore, node: uuid::Uuid, report: &SyncReport, since_ms: i64) -> Result<bool> {
    if report.heard_from.contains(&node) {
        return Ok(true);
    }
    fleet.read(|conn| {
        Ok(tod_store::cloud_activity::get(conn, node)?.is_some_and(|b| b.heartbeat_at >= since_ms - CLOCK_SKEW_MS))
    })
}

/// How far the sandbox's clock may be behind this machine's when comparing
/// a heartbeat's stamp with when the node was sent.
const CLOCK_SKEW_MS: i64 = 30_000;

/// [`run_in_cloud`] as the user's click means it: from the first moment the
/// node holds an activity lease (`KIND_CLOUD_START`), so it shows as being
/// worked, not as waiting on the user, while their request is carried out;
/// and the lease is held until its supervisor has been heard from, so the
/// node never falls back to "not started" in the gap between the sandbox
/// being ready and its first heartbeat arriving. Pulls every few seconds
/// meanwhile. Not hearing from it by [`SUPERVISOR_REPORTS_WITHIN`] is
/// reported (`heard: false`), never taken for success.
pub fn start_in_cloud(
    fleet: &FleetStore,
    root: &Path,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<Started> {
    let node: uuid::Uuid = node_id.parse().context("node id")?;
    let owner = crate::next_actor::this_process();
    tod_store::activity::with_lease(fleet, node, tod_store::activity::KIND_CLOUD_START, &owner, || {
        let since = tod_store::outline::uuid_blob::now_ms();
        let (record, report) = run_in_cloud_reporting(fleet, root, node_id, progress)?;
        let heard = wait_for_supervisor(fleet, root, node, report, since, progress)?;
        Ok(Started { node: record, heard })
    })?
}

/// [`wake_node`] with the same promise as [`start_in_cloud`]: the node shows
/// as being worked from the click until its supervisor is heard from. The
/// text is what [`wake_node`] said; the flag is whether it was heard.
pub fn wake_node_tracked(
    fleet: &FleetStore,
    root: &Path,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<(String, bool)> {
    wake_node_tracked_with(fleet, root, node_id, false, progress)
}

/// [`wake_node_tracked`] that also asks the supervisor to run the node's
/// current step in a new session (`fresh_session`; see [`wake_node_with`]).
pub fn wake_node_tracked_with(
    fleet: &FleetStore,
    root: &Path,
    node_id: &str,
    fresh_session: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<(String, bool)> {
    let kind = if fresh_session { REQUEST_FRESH_SESSION } else { REQUEST_CHECK_NOW };
    wake_node_tracked_requesting(fleet, root, node_id, kind, progress)
}

/// [`wake_node_tracked`] that records the user's request of `kind`
/// (`tod_store::node_requests::KINDS`) for the supervisor to consume.
pub fn wake_node_tracked_requesting(
    fleet: &FleetStore,
    root: &Path,
    node_id: &str,
    kind: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<(String, bool)> {
    let node: uuid::Uuid = node_id.parse().context("node id")?;
    let owner = crate::next_actor::this_process();
    tod_store::activity::with_lease(fleet, node, tod_store::activity::KIND_CLOUD_START, &owner, || {
        let since = tod_store::outline::uuid_blob::now_ms();
        let said = wake_node_requesting(fleet, root, node_id, kind)?;
        let heard = wait_for_supervisor(fleet, root, node, SyncReport::default(), since, progress)?;
        Ok((said, heard))
    })?
}

/// Pulls from the orchestrator every [`REPORT_POLL`] until the node's
/// supervisor is heard from ([`supervisor_heard`], starting with `report`) or
/// [`SUPERVISOR_REPORTS_WITHIN`] passes.
fn wait_for_supervisor(
    fleet: &FleetStore,
    root: &Path,
    node: uuid::Uuid,
    mut report: SyncReport,
    since_ms: i64,
    progress: &mut dyn FnMut(&str),
) -> Result<bool> {
    let deadline = std::time::Instant::now() + SUPERVISOR_REPORTS_WITHIN;
    progress("waiting to hear from the node's supervisor…");
    loop {
        if supervisor_heard(fleet, node, &report, since_ms)? {
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(REPORT_POLL);
        report = sync_now(fleet, root)
            .context("the node was sent to the cloud, but looking for its supervisor failed")?;
    }
}

/// An orchestrator that rejects rows this app wrote (a constraint or a
/// column it does not have) is older than the app: says so, and how to bring
/// it up to date. Any other error passes through unchanged.
fn older_orchestrator_hint(err: anyhow::Error) -> anyhow::Error {
    let text = format!("{err:#}");
    if text.contains("CHECK constraint failed")
        || text.contains("no such column")
        || text.contains("no such table")
        || text.contains("newer than this orchestrator supports")
    {
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
        .with_context(|| format!("send the settings to the supervisor ({path})"))?;
    // Which databases the user set up for it (names only), so its agent is
    // told about `tod-cli db` exactly when it has something to use.
    let databases = format!("{}/{node_id}/{}", tod_store::fleet::cli_relay::SUPERVISOR_STATE_ROOT, crate::databases_context::FILE);
    bx.upload(url, &databases, crate::databases_context::snapshot(root)?.as_bytes(), "0644")
        .with_context(|| format!("send the declared databases to the supervisor ({databases})"))
}

/// Wakes `node_id`'s supervisor now, instead of at its next scheduled check:
/// sends what changed here (a cancelled wait) to the orchestrator, then
/// pokes the node's sandbox, which pulls it and goes on. Returns what it
/// did. Blocks on the network: never on the UI thread.
pub fn wake_node(fleet: &FleetStore, root: &Path, node_id: &str) -> Result<String> {
    wake_node_with(fleet, root, node_id, false)
}

/// [`wake_node`]; with `fresh_session` the sandbox first gets the request
/// file ([`crate::autopilot::FRESH_SESSION_REQUEST_FILE`]) that makes the
/// supervisor run the node's current step in a new session, never one it
/// would reopen or resume. A running supervisor takes it at its next poll
/// (cancelling the turn in flight); a sleeping one when this poke wakes it.
pub fn wake_node_with(fleet: &FleetStore, root: &Path, node_id: &str, fresh_session: bool) -> Result<String> {
    let kind = if fresh_session { REQUEST_FRESH_SESSION } else { REQUEST_CHECK_NOW };
    wake_node_requesting(fleet, root, node_id, kind)
}

pub use tod_store::node_requests::{
    KIND_CHECK_NOW as REQUEST_CHECK_NOW, KIND_FRESH_BUDGET as REQUEST_FRESH_BUDGET,
    KIND_FRESH_SESSION as REQUEST_FRESH_SESSION, KIND_KEEP_GOING as REQUEST_KEEP_GOING, KIND_PAUSE as REQUEST_PAUSE,
    KIND_RUN as REQUEST_RUN, KIND_STOP_NOW as REQUEST_STOP_NOW,
};

/// Writes the node's `starting` run state (`tod_store::run_state`) before
/// anything is sent or seeded, so it reads as being started from the first
/// sync on, not as "not started" until its supervisor first reports in. The
/// supervisor overwrites it with its own words as soon as it has them; one
/// built before the table ignores it and its heartbeat speaks instead.
pub fn record_starting(fleet: &FleetStore, node_id: &str) -> Result<()> {
    let node = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    crate::run_state::record(fleet, node, crate::run_state::sending(), "app")?;
    Ok(())
}

/// The actor that records a request row.
pub const REQUEST_ACTOR: &str = "app:request";

/// Records the user's request of `kind` on the node as a row that travels
/// with the next sync and is consumed by the supervisor
/// (`tod_store::node_requests`).
pub fn record_node_request(fleet: &FleetStore, node_id: &str, kind: &str) -> Result<uuid::Uuid> {
    let node = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    let result = fleet
        .interview(
            REQUEST_ACTOR,
            tod_store::interview::InterviewCommand::RecordNodeRequest {
                node_id: node,
                kind: kind.to_string(),
                by: "app".to_string(),
                at: tod_store::outline::uuid_blob::now_ms(),
            },
        )
        .map_err(|e| anyhow!("record the {kind} request: {e}"))?;
    let id = result["id"].as_str().ok_or_else(|| anyhow!("the {kind} request was recorded without an id"))?;
    uuid::Uuid::parse_str(id).context("request id")
}

/// [`wake_node_with`] for a request of `kind`: the row is recorded first, so
/// it travels with the sync (and stays pending until the supervisor consumes
/// it, whenever it next wakes). A request to run or to go on also answers the
/// node's stop question to continue (2.1), for a supervisor that reads only
/// that; a new session also leaves the marker file an older supervisor reads.
/// A `pause` or `stop_now` is not a request to run: it records, syncs and
/// pokes only (no stop answer, no "starting" state), and `stop_now` also
/// leaves the marker ([`crate::autopilot::STOP_NOW_REQUEST_FILE`]) a turn in
/// flight sees before the row arrives. `fresh_budget` goes as a request to run.
pub fn wake_node_requesting(fleet: &FleetStore, root: &Path, node_id: &str, kind: &str) -> Result<String> {
    match wake_node_requesting_inner(fleet, root, node_id, kind) {
        Ok(said) => Ok(said),
        // The `starting` row this call wrote (see [`failing_start_is_recorded`]),
        // only while it is still the node's state: a stop request writes none,
        // and a call that failed before writing it must not replace a better one.
        Err(err) if !matches!(kind, REQUEST_PAUSE | REQUEST_STOP_NOW) && still_starting(fleet, node_id) => {
            failing_start_is_recorded::<String>(fleet, node_id, || Err(err))
        }
        Err(err) => Err(err),
    }
}

fn still_starting(fleet: &FleetStore, node_id: &str) -> bool {
    let Ok(node) = uuid::Uuid::parse_str(node_id) else { return false };
    fleet
        .read(|conn| tod_store::run_state::get(conn, node))
        .ok()
        .flatten()
        .is_some_and(|row| row.phase == tod_store::run_state::Phase::Starting)
}

fn wake_node_requesting_inner(fleet: &FleetStore, root: &Path, node_id: &str, kind: &str) -> Result<String> {
    let fresh_session = kind == REQUEST_FRESH_SESSION;
    let stops = matches!(kind, REQUEST_PAUSE | REQUEST_STOP_NOW);
    let row = cloud_node(fleet, node_id).ok_or_else(|| anyhow!("this node does not run in the cloud"))?;
    // The node's sandbox is its Files location's, not the row's copy.
    let sandbox = cloud_sandbox(fleet, node_id)?;
    let (orch, user) = resolve(root)?;
    // Before the sync, so the answer and the row travel with it: a supervisor
    // that reads "left stopped" would ignore the request.
    if kind != REQUEST_CHECK_NOW && !stops {
        continue_over_stop_answer(fleet, node_id)?;
    }
    // A fresh budget also answers the budget question still waiting, or the
    // pending decision would stop the run again before it used it.
    if kind == REQUEST_FRESH_BUDGET {
        continue_over_pending_budget_question(fleet, node_id)?;
    }
    record_node_request(fleet, node_id, kind)?;
    if !stops {
        record_starting(fleet, node_id)?;
    }
    sync(fleet, root, &orch, &user).context("send the change to the orchestrator")?;
    let sandboxes = tod_store::fleet::sandbox::Sandboxes::load(root)?;
    let bx = sandboxes.blaxel()?;
    // A sandbox that is gone (deleted or reclaimed since the last look) is
    // replaced the way the lost-sandbox check does, then woken.
    let found = bx.get(&sandbox)?;
    // A sandbox in standby answers 502/503/504 and Blaxel's own 404 "workload
    // is not ready" for up to ~30s after waking, and WORKLOAD_UNAVAILABLE for
    // longer in an outage. That is waited out below, bounded, and an error
    // when it lasts: only Blaxel having no such sandbox, or reporting it
    // terminal, replaces it (`wake_verdict`).
    let made_for = found.as_ref().and_then(|info| info.env("TOD_USER")).unwrap_or(&row.user);
    let verdict = wake_verdict(&sandbox, found.as_ref(), sandbox_user_reason(&sandbox, made_for, &user));
    if let WakeVerdict::Replace(reason) = &verdict {
        tracing::warn!("cloud: {reason}; replacing it");
    }
    let (sandbox, info, replaced) = match (found, verdict) {
        (Some(info), WakeVerdict::Use) => {
            if let Some(url) = info.url.as_deref() {
                bx.wait_reachable(url, WAKE_WAIT).map_err(|err| still_waking_error(&sandbox, kind, &err))?;
            }
            (sandbox, info, None)
        }
        _ => {
            let gone = sandbox;
            let mut progress = |msg: &str| log_progress(&format!("replacing {gone}"), msg);
            let name = ensure_supervisor(fleet, root, node_id, true, &mut progress)
                .and_then(|name| record_cloud_node(fleet, node_id, &user).map(|_| name))
                .with_context(|| format!("sandbox {gone} is gone, and replacing it failed"))?;
            let info = bx.get(&name)?.ok_or_else(|| anyhow!("sandbox {name} no longer exists"))?;
            (name, info, Some(gone))
        }
    };
    let url = info.url.ok_or_else(|| anyhow!("sandbox {sandbox} has no URL"))?;
    // The disk check, and the rest of the health probe: a sandbox that is
    // `DEPLOYED` but cannot reach GitHub, lost its environment or has no relay
    // is named here, not discovered by a turn that fails later.
    let (_, repo_url, _) = node_source(fleet, node_id)?;
    let mut wake_progress = |msg: &str| log_progress(&format!("wake {node_id}"), msg);
    probe_health(
        &bx,
        &HealthTarget { label: &sandbox, node: node_id, repo_url: &repo_url, url: &url },
        tod_sandbox::health::Stage::Ready,
        WAKE_MIN_FREE,
        None,
        &[],
        &mut wake_progress,
    )
    .with_context(|| format!("not waking {node_id}'s supervisor"))?;
    push_supervisor_settings(&bx, &url, root, node_id)?;
    if fresh_session {
        let request = format!(
            "{}/{node_id}/{}",
            tod_store::fleet::cli_relay::SUPERVISOR_STATE_ROOT,
            crate::autopilot::FRESH_SESSION_REQUEST_FILE
        );
        bx.upload(&url, &request, b"", "0644")
            .with_context(|| format!("ask the supervisor for a new session ({request})"))?;
    }
    if kind == REQUEST_STOP_NOW {
        let request = format!(
            "{}/{node_id}/{}",
            tod_store::fleet::cli_relay::SUPERVISOR_STATE_ROOT,
            crate::autopilot::STOP_NOW_REQUEST_FILE
        );
        bx.upload(&url, &request, b"", "0644")
            .with_context(|| format!("ask the supervisor to stop the turn now ({request})"))?;
    }
    let poke = format!("{}/port/{}/poke", url.trim_end_matches('/'), tod_sandbox::blaxel::RELAY_PORT);
    let resp = ureq::post(&poke)
        .header("Authorization", &format!("Bearer {}", bx.token()))
        .config()
        .http_status_as_error(false)
        .build()
        .send_empty()
        .context("poke the node")?;
    if !resp.status().is_success() {
        bail!("poking sandbox {sandbox} got {}", resp.status());
    }
    Ok(match replaced {
        Some(gone) => format!("replaced the missing sandbox {gone} with {sandbox} and woke its supervisor"),
        None => format!("woke the supervisor in {sandbox}"),
    })
}

/// The actor of the answer [`continue_over_stop_answer`] records.
pub const RUN_REQUEST_ACTOR: &str = "app:run-with-a-new-session";

/// The user asked to run the node ("Run with a new session"): if its latest
/// stop question (`crate::stop_questions`) is answered to leave it stopped,
/// that is answered again to continue, so the supervisor does not read the
/// old answer and go back to sleep. Answers are append-only, so the earlier
/// one stays in the log. Returns the decision answered, if any.
pub fn continue_over_stop_answer(fleet: &FleetStore, node_id: &str) -> Result<Option<uuid::Uuid>> {
    use crate::stop_questions::Answer;
    let node = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    answer_latest_to_continue(fleet, node, |latest| latest.answer == Answer::Stop)
}

/// "Resume with a fresh budget": if the node's latest stop question is the
/// budget one and still unanswered, it is answered to continue (which grants
/// the budget, `tod_supervisor::answers`). Returns the decision answered, if any.
pub fn continue_over_pending_budget_question(fleet: &FleetStore, node_id: &str) -> Result<Option<uuid::Uuid>> {
    use crate::stop_questions::{self, Answer};
    let node = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    answer_latest_to_continue(fleet, node, |latest| {
        latest.answer == Answer::Pending && latest.kind == stop_questions::BUDGET
    })
}

/// Answers the node's latest stop question to continue when `wanted` says so.
fn answer_latest_to_continue(
    fleet: &FleetStore,
    node: uuid::Uuid,
    wanted: impl Fn(&crate::stop_questions::Latest) -> bool,
) -> Result<Option<uuid::Uuid>> {
    use crate::stop_questions;
    let Some(latest) = fleet.read(|conn| stop_questions::latest(conn, node))? else {
        return Ok(None);
    };
    if !wanted(&latest) {
        return Ok(None);
    }
    let decision = fleet
        .read(|conn| tod_store::decisions::DecisionRepo::new(conn).get(latest.decision))?
        .ok_or_else(|| anyhow!("stop question {} not found", latest.decision))?;
    let option = decision
        .options
        .iter()
        .position(|o| {
            o == stop_questions::KEEP_GOING || o == stop_questions::WAKE_AGAIN || o == stop_questions::FIXED_TRY_AGAIN
        })
        .ok_or_else(|| anyhow!("stop question {} has no option to continue", latest.decision))?;
    fleet
        .interview(
            RUN_REQUEST_ACTOR,
            tod_store::interview::InterviewCommand::AnswerDecision {
                decision_id: latest.decision,
                option: Some(option as i64 + 1),
                text: None,
            },
        )
        .map_err(|e| anyhow!("answer the stop question {} to continue: {e}", latest.decision))?;
    Ok(Some(latest.decision))
}

pub mod adopt;
pub mod golden;
pub mod host;
pub mod lost;
pub mod provisioning;
pub mod refresh;
pub mod deploy;
pub use host::register_sandbox_host;

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
