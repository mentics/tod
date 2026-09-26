//! Sync between a user's app and their copy here (`tod_store::sync`).
//!
//! - `POST /users/<u>/seed`: the body is a database snapshot
//!   (`tod_store::sync::snapshot`). It replaces the user's database; the
//!   snapshot's `last_seq` is recorded as the feed's start, since the
//!   snapshot carries the app's own log, which the app must not get back.
//!   Reply: `{"last_seq": n}`.
//! - `POST /users/<u>/changes`: the body is a client's changes (the app's,
//!   or a node supervisor's), a JSON array of `tod_store::sync::Change`.
//!   Logged, tagged with the client (`X-Tod-Client`, else `?client=`, else
//!   `unknown`), so every other client gets them. Reply: the `ApplyReport`
//!   and `last_seq`.
//! - `GET /users/<u>/changes?after=<n>`: the changes after `n` (never from
//!   before the seed), without the requesting client's own. Reply:
//!   `{"last_seq": n, "changes": [...]}`; the client pulls again with
//!   `after` = `last_seq`.
//! - `GET /users/<u>/snapshot`: the whole database, for a node supervisor's
//!   local copy (its `last_seq` is where that copy pulls from).
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
use tod_store::sync::{self, Change};

/// Where the seed's cursor is kept, in the user's data root.
const SEED_SEQ_FILE: &str = "orchestrator-seed-seq";

pub fn seed(users: &Users, user: &str, snapshot: &[u8]) -> Response {
    reply(seed_inner(users, user, snapshot))
}

fn seed_inner(users: &Users, user: &str, snapshot: &[u8]) -> Result<Response> {
    let root = users.root_of(user)?;
    // The store must be closed while its database is replaced.
    if !users.close_if_idle(user) {
        return Ok(Response::text(409, "the user's store is in use; retry the seed"));
    }
    std::fs::create_dir_all(&root)?;
    let incoming = root.join("seed-incoming.db");
    std::fs::write(&incoming, snapshot).context("write the snapshot")?;
    let restored = sync::restore(&incoming, &root.join("tod.db"));
    let _ = std::fs::remove_file(&incoming);
    restored.context("restore the snapshot")?;
    // Reopen (migrating the snapshot to this build's schema) before reading
    // the cursor.
    let data = users.get(user)?;
    let last = sync::last_seq(&connect(&data)?)?;
    std::fs::write(root.join(SEED_SEQ_FILE), last.to_string())?;
    Ok(json(&serde_json::json!({ "last_seq": last })))
}

/// Logged, tagged with the sending `client`, so every other client gets
/// them through the feed and `client` does not get them back.
pub fn apply_changes(user: &UserData, body: &[u8], client: &str) -> Response {
    reply((|| {
        let changes: Vec<Change> = serde_json::from_slice(body).context("changes: a JSON array of Change")?;
        let _guard = user.sync_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = user.store.flush_on_quit();
        let mut conn = connect(user)?;
        let report = sync::apply_changes_from(&mut conn, &changes, client)?;
        let last = sync::last_seq(&conn)?;
        drop(conn);
        let _ = user.store.reload_if_stale();
        let mut v = serde_json::to_value(&report)?;
        v["last_seq"] = last.into();
        Ok(json(&v))
    })())
}

/// `client`'s own changes are left out.
pub fn export_changes(user: &UserData, after: i64, client: Option<&str>) -> Response {
    reply((|| {
        let _guard = user.sync_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = user.store.flush_on_quit();
        let after = after.max(seed_seq(&user.root));
        let conn = connect(user)?;
        let last = sync::last_seq(&conn)?;
        let changes = sync::export_changes_for(&conn, after, client)?;
        Ok(json(&serde_json::json!({ "last_seq": last, "changes": changes })))
    })())
}

/// `GET /users/<u>/snapshot`: the user's whole database (the body), for a
/// node's supervisor to seed its local copy from. Taken under the sync lock,
/// so the snapshot's own `last_seq` is the feed cursor to pull after.
pub fn snapshot(user: &UserData) -> Response {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    reply((|| {
        let _guard = user.sync_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = user.store.flush_on_quit();
        let out = user.root.join(format!("snapshot-{}.db", N.fetch_add(1, Ordering::Relaxed)));
        let taken = sync::snapshot(user.store.paths().db(), &out);
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
