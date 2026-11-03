//! Sync between a user's app and their copy here (`tod_store::sync`).
//!
//! - `POST /users/<u>/seed`: the body is a database snapshot
//!   (`tod_store::sync::snapshot`). It replaces the user's database; the
//!   snapshot's `last_seq` is recorded as the feed's start, since the
//!   snapshot carries the app's own log, which the app must not get back.
//!   Reply: `{"last_seq": n, "seed_id": id}`.
//! - `POST /users/<u>/changes`: the body is a client's changes (the app's,
//!   or a node supervisor's), a JSON array of `tod_store::sync::Change`.
//!   Logged, tagged with the client (`X-Tod-Client`, else `?client=`, else
//!   `unknown`), so every other client gets them. Reply: the `ApplyReport`
//!   and `last_seq`.
//! - `GET /users/<u>/changes?after=<n>`: the changes after `n` (never from
//!   before the seed), without the requesting client's own. Reply:
//!   `{"last_seq": n, "changes": [...], "seed_id": id}` (plus
//!   `notices_degraded_secs` while the notice service refuses publishes; `seed_id` names
//!   the seeding the feed follows; a client whose copy came from another one
//!   starts over); the client pulls again with
//!   `after` = `last_seq`.
//! - `GET /users/<u>/snapshot`: the whole database, for a node supervisor's
//!   local copy (its `last_seq` is where that copy pulls from).
//!
//! A node's supervisor asks for its node's slice instead (`?slice=1`, the
//! node named by `X-Tod-Node`; `tod_store::sync::slice`): the snapshot is cut
//! to the slice, which records its cursor and slice id itself, and the feed
//! is confined to it and carries `"slice_id"`, so a copy whose node moved is
//! cut again. A supervisor's own changes (a `supervisor-` client naming its
//! node with `X-Tod-Node`) are refused when they reach outside its node and
//! the nodes under it.
//!
//! Each runs on its own SQLite connection, under the user's sync lock, after
//! flushing the store's pending writes; the store reloads its read view
//! after an apply so `tod-cli` and later reads see the result.

use crate::http::Response;
use crate::users::{UserData, Users};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;
use tod_store::sync::slice::Scope;
use tod_store::sync::{self, Change};
use uuid::Uuid;

/// Where the seed's cursor is kept, in the user's data root.
const SEED_SEQ_FILE: &str = "orchestrator-seed-seq";
/// Names one seeding, new each time: what a client holding a copy compares to
/// know the database it copied from has been replaced.
const SEED_ID_FILE: &str = "orchestrator-seed-id";

fn seed_id(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join(SEED_ID_FILE)).ok().map(|s| s.trim().to_string())
}

/// The words of the 409 an unseeded user's sync requests get; the app looks
/// for them (`tod_core::cloud_sync::NOT_SEEDED`) and seeds again.
pub const NOT_SEEDED: &str = "the orchestrator has not been seeded with this user's database";

/// Whether `root` holds a seeded database. A redeployed orchestrator without
/// its data volume has none, whatever the app remembers having sent: rows
/// sent to it would be dropped as orphans of nodes it never received.
pub fn is_seeded(root: &Path) -> bool {
    root.join(SEED_SEQ_FILE).is_file()
}

/// Names the client that made the current seeding (`X-Tod-Client`).
const SEED_CLIENT_FILE: &str = "orchestrator-seed-client";

/// The header a seed carries to replace a database another client seeded.
pub const TAKE_OVER_HEADER: &str = "X-Tod-Take-Over";

/// The words of the 409 for a seed while the user's store is in use; the app
/// retries only on these.
pub const STORE_IN_USE: &str = "the user's store is in use; retry the seed";

/// The client that made the current seeding, `None` from before it was recorded.
fn seed_client(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join(SEED_CLIENT_FILE)).ok().map(|s| s.trim().to_string())
}

/// Writes `contents` to `root/name` so a reader sees all of it or none.
fn write_atomic(root: &Path, name: &str, contents: &str) -> Result<()> {
    let tmp = root.join(format!("{name}.tmp"));
    std::fs::write(&tmp, contents).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, root.join(name)).with_context(|| format!("replace {name}"))
}

/// `client` is who is seeding; `take_over` is the user's explicit choice to
/// replace a database another client seeded.
pub fn seed(users: &Users, user: &str, snapshot: &[u8], client: &str, take_over: bool) -> Response {
    reply(seed_inner(users, user, snapshot, client, take_over))
}

fn seed_inner(users: &Users, user: &str, snapshot: &[u8], client: &str, take_over: bool) -> Result<Response> {
    let root = users.root_of(user)?;
    // Everything that can refuse the seed comes before anything is replaced.
    if is_seeded(&root)
        && !take_over
        && let Some(current) = seed_client(&root)
        && current != client
    {
        return Ok(Response::text(
            409,
            format!(
                "this user's orchestrator database was seeded by another client ({current}); this is {client}. \
                 Seeding replaces it: send {TAKE_OVER_HEADER}: yes to take it over"
            ),
        ));
    }
    std::fs::create_dir_all(&root)?;
    let incoming = root.join("seed-incoming.db");
    std::fs::write(&incoming, snapshot).context("write the snapshot")?;
    let theirs = tod_store::fleet::schema::peek_user_version(&incoming);
    let ours = tod_store::fleet::schema::CURRENT_USER_VERSION;
    match theirs {
        Ok(v) if v > ours => {
            let _ = std::fs::remove_file(&incoming);
            return Ok(Response::text(
                422,
                format!(
                    "the seed's schema version {v} is newer than this orchestrator supports ({ours}): \
                     the orchestrator is out of date; update it with scripts/deploy.sh"
                ),
            ));
        }
        Ok(_) => {}
        Err(err) => {
            let _ = std::fs::remove_file(&incoming);
            return Ok(Response::text(400, format!("the seed is not a tod database: {err:#}")));
        }
    }
    // Another data root's cloud nodes are not this seed's to drop, take-over
    // or not (robustness plan 3.10, extending 2.9).
    if is_seeded(&root) {
        match foreign_nodes(users, user, &incoming, client) {
            Ok(held) if held.is_empty() => {}
            Ok(held) => {
                let _ = std::fs::remove_file(&incoming);
                return Ok(Response::text(
                    409,
                    format!(
                        "this user's orchestrator database holds {} cloud node{} owned by another data root ({}) that this \
                         seed from {client} does not contain; seeding would drop them. Use a different orchestrator user for \
                         this data root (`owner` in sandboxes.toml), or remove those nodes from the data root that owns them first",
                        held.len(),
                        if held.len() == 1 { "" } else { "s" },
                        held.iter().map(|(_, owner)| owner.as_str()).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
            Err(err) => {
                let _ = std::fs::remove_file(&incoming);
                return Err(err.context("check who owns the cloud nodes the seed would replace"));
            }
        }
    }
    // The store must be closed while its database is replaced.
    if !users.close_if_idle(user) {
        let _ = std::fs::remove_file(&incoming);
        return Ok(Response::text(409, STORE_IN_USE));
    }
    // Not seeded from here until the new database and its markers are all in
    // place: the seq file is the last thing written.
    match std::fs::remove_file(root.join(SEED_SEQ_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("clear the seed marker"),
    }
    let restored = sync::restore(&incoming, &root.join("tod.db"));
    let _ = std::fs::remove_file(&incoming);
    restored.context("restore the snapshot")?;
    // Reopen (migrating the snapshot to this build's schema) before reading
    // the cursor.
    let data = users.get(user)?;
    let last = sync::last_seq(&connect(&data)?)?;
    let id = uuid::Uuid::new_v4().to_string();
    write_atomic(&root, SEED_ID_FILE, &id)?;
    write_atomic(&root, SEED_CLIENT_FILE, client)?;
    write_atomic(&root, SEED_SEQ_FILE, &last.to_string())?;
    Ok(json(&serde_json::json!({ "last_seq": last, "seed_id": id })))
}

/// The cloud nodes (node id, owning client) the user's database records as
/// owned by a client other than `client` and that the `incoming` snapshot
/// does not hold: a seed from `client` would orphan them. A row with no
/// recorded owner (an older client's) is nobody's and not counted.
fn foreign_nodes(users: &Users, user: &str, incoming: &Path, client: &str) -> Result<Vec<(String, String)>> {
    let current = users.get(user)?;
    let rows = {
        let _guard = current.sync_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = current.store.flush_on_quit();
        tod_store::cloud_nodes::list(&connect(&current)?)?
    };
    let foreign: Vec<_> = rows
        .into_iter()
        .filter_map(|r| r.ownership.owner_client.filter(|o| !o.is_empty() && o != client).map(|o| (r.node_id, o)))
        .collect();
    if foreign.is_empty() {
        return Ok(Vec::new());
    }
    let theirs = Connection::open_with_flags(incoming, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("open the incoming snapshot")?;
    let has_table = theirs
        .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'cloud_nodes'")?
        .exists([])?;
    let mut held = std::collections::HashSet::new();
    if has_table {
        let mut stmt = theirs.prepare("SELECT node_id FROM cloud_nodes")?;
        for blob in stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
            held.insert(tod_store::outline::uuid_blob::blob_to_uuid_sql(&blob?)?);
        }
    }
    Ok(foreign.into_iter().filter(|(id, _)| !held.contains(id)).map(|(id, o)| (id.to_string(), o)).collect())
}

/// Logged, tagged with the sending `client`, so every other client gets
/// them through the feed and `client` does not get them back.
///
/// With `confined_to` (a node's supervisor), the whole batch is refused
/// (400) when it reaches outside that node and the nodes under it.
///
/// With `impact` (the user's name and the wakes), the running cloud nodes
/// the changes affect are marked and poked ([`crate::impact_handler`]), and
/// so are those whose stop questions they answer ([`crate::answers`]) and
/// those whose waits they change ([`crate::wait_changes`]).
pub fn apply_changes(
    user: &UserData,
    body: &[u8],
    client: &str,
    impact: Option<(&str, &crate::wakes::Wakes)>,
    confined_to: Option<Uuid>,
) -> Response {
    let mut affected = Vec::new();
    let mut answered = Vec::new();
    let mut waits_changed = Vec::new();
    let response = reply((|| {
        let changes: Vec<Change> = match serde_json::from_slice(body) {
            Ok(c) => c,
            Err(err) => return Ok(Response::text(400, format!("changes: a JSON array of Change: {err}"))),
        };
        let _guard = user.sync_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = user.store.flush_on_quit();
        let mut conn = connect(user)?;
        let report = match confined_to {
            Some(node) => match sync::apply_changes_confined(&mut conn, &changes, client, node) {
                Ok(report) => report,
                Err(err) => return Ok(Response::text(400, format!("{err:#}"))),
            },
            None => sync::apply_changes_from(&mut conn, &changes, client)?,
        };
        if impact.is_some() {
            affected = crate::impact_handler::record(&conn, &changes, client, crate::wakes::now_ms())
                .unwrap_or_else(|err| {
                    eprintln!("tod-orchestrator: impact: {err:#}");
                    Vec::new()
                });
            answered = crate::answers::record(&conn, &changes).unwrap_or_else(|err| {
                eprintln!("tod-orchestrator: answers: {err:#}");
                Vec::new()
            });
            waits_changed = crate::wait_changes::record(&conn, &changes, client).unwrap_or_else(|err| {
                eprintln!("tod-orchestrator: waits: {err:#}");
                Vec::new()
            });
        }
        if report.rejected > 0 {
            eprintln!(
                "tod-orchestrator: refused {} of {}'s changes from {client}: {}",
                report.rejected,
                user.root.display(),
                report.reject_errors.join("; ")
            );
        }
        let last = sync::last_seq(&conn)?;
        drop(conn);
        let _ = user.store.reload_if_stale();
        let mut v = serde_json::to_value(&report)?;
        v["last_seq"] = last.into();
        Ok(json(&v))
    })());
    if let Some((name, wakes)) = impact {
        crate::impact_handler::poke(wakes, name, &affected);
        crate::answers::poke(wakes, name, &answered);
        crate::wait_changes::poke(wakes, name, &waits_changed);
    }
    response
}

/// Wakes a long-poll of the feed ([`export_changes_waiting`]) when something
/// was committed: a counter and a condvar, bumped after every successful POST.
#[derive(Default)]
pub struct FeedSignal {
    generation: std::sync::Mutex<u64>,
    changed: std::sync::Condvar,
}

impl FeedSignal {
    /// A change may have been committed.
    pub fn bump(&self) {
        *self.generation.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.changed.notify_all();
    }

    fn generation(&self) -> u64 {
        *self.generation.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Blocks until the counter passes `seen` or `timeout` is over.
    fn wait_past(&self, seen: u64, timeout: Duration) {
        let guard = self.generation.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self.changed.wait_timeout_while(guard, timeout, |g| *g == seen).unwrap_or_else(|e| e.into_inner());
    }
}

/// The longest a long-poll of the feed is held (`?wait=` is cut to this): under
/// the gateway's own idle limit.
pub const MAX_FEED_WAIT: Duration = Duration::from_secs(55);

/// `client`'s own changes are left out. `notices_degraded_secs` is how long
/// the notice service has been refusing for (see `notify::Notifier::degraded_for`):
/// the app pulls on a short timer until then, as no notice will say a change came.
///
/// With `slice` (a node), only the changes in that node's slice, and its
/// `slice_id`.
pub fn export_changes(
    user: &UserData,
    after: i64,
    client: Option<&str>,
    slice: Option<Uuid>,
    notices_degraded_secs: Option<u64>,
) -> Response {
    reply(feed(user, after, client, slice, notices_degraded_secs).map(|(v, _)| json(&v)))
}

/// [`export_changes`] as a long-poll (`?wait=<secs>`): answers at once when
/// there are changes for `client` after `after`, else holds the request until
/// one is committed ([`FeedSignal::bump`]) or `wait` is over (an empty feed).
/// No lock is held while waiting. The answer says `"waited": true`: an
/// orchestrator that does not know `wait` answers at once without it, which
/// is how a client tells the two apart.
pub fn export_changes_waiting(
    user: &UserData,
    signal: &FeedSignal,
    wait: Duration,
    after: i64,
    client: Option<&str>,
    slice: Option<Uuid>,
    notices_degraded_secs: impl Fn() -> Option<u64>,
) -> Response {
    let deadline = std::time::Instant::now() + wait.min(MAX_FEED_WAIT);
    reply((|| loop {
        // Read the counter first: a commit between the look and the wait is
        // seen as a counter that moved, not missed.
        let seen = signal.generation();
        let (mut v, n) = feed(user, after, client, slice, notices_degraded_secs())?;
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if n > 0 || left.is_zero() {
            v["waited"] = true.into();
            return Ok(json(&v));
        }
        signal.wait_past(seen, left);
    })())
}

fn feed(
    user: &UserData,
    after: i64,
    client: Option<&str>,
    slice: Option<Uuid>,
    notices_degraded_secs: Option<u64>,
) -> Result<(serde_json::Value, usize)> {
    let _guard = user.sync_lock.lock().unwrap_or_else(|e| e.into_inner());
    let _ = user.store.flush_on_quit();
    let after = after.max(seed_seq(&user.root));
    let conn = connect(user)?;
    let last = sync::last_seq(&conn)?;
    let scope = slice.map(|node| Scope::for_node(&conn, "", node)).transpose()?;
    let changes = sync::export_changes_in(&conn, after, client, scope.as_ref())?;
    let n = changes.len();
    let mut v = serde_json::json!({ "last_seq": last, "changes": changes, "seed_id": seed_id(&user.root) });
    if let Some(scope) = &scope {
        v["slice_id"] = scope.id().into();
    }
    if let Some(secs) = notices_degraded_secs {
        v["notices_degraded_secs"] = secs.into();
    }
    Ok((v, n))
}

/// `GET /users/<u>/snapshot`: the user's whole database (the body), for a
/// node's supervisor to seed its local copy from. Taken under the sync lock,
/// so the snapshot's own `last_seq` is the feed cursor to pull after.
///
/// With `slice` (a node), that node's slice instead (`sync::slice`), which
/// records its own cursor.
pub fn snapshot(user: &UserData, slice: Option<Uuid>) -> Response {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    reply((|| {
        let _guard = user.sync_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = user.store.flush_on_quit();
        let out = user.root.join(format!("snapshot-{}.db", N.fetch_add(1, Ordering::Relaxed)));
        let taken = match slice {
            Some(node) => sync::slice::snapshot_slice(user.store.paths().db(), node, &out).map(|_| ()),
            None => sync::snapshot(user.store.paths().db(), &out),
        };
        let bytes = taken.and_then(|()| std::fs::read(&out).context("read the snapshot"));
        let _ = std::fs::remove_file(&out);
        Ok(Response::bytes(bytes?))
    })())
}

fn seed_seq(root: &Path) -> i64 {
    std::fs::read_to_string(root.join(SEED_SEQ_FILE)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

fn connect(user: &UserData) -> Result<Connection> {
    let conn = Connection::open(user.store.paths().db())?;
    conn.busy_timeout(Duration::from_secs(30))?;
    Ok(conn)
}

fn json(v: &serde_json::Value) -> Response {
    Response { status: 200, content_type: "application/json", body: v.to_string().into_bytes() }
}

fn reply(result: Result<Response>) -> Response {
    result.unwrap_or_else(|err| Response::text(500, format!("{err:#}")))
}
