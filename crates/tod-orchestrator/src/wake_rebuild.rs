//! Rebuilds the wakes a lost `wakes.json` held, from the waits the user's
//! database holds (the orchestrator's `/data` can come back empty, and the
//! app then re-seeds each user's database, which carries `waits` and
//! `cloud_nodes` but not the wakes).
//!
//! A pending wait of a node that has a `cloud_nodes` row (sandbox known, not
//! lost) and has no wake registered at all gets one, at the wait's `due_at`
//! (a time already past becomes [`LEAD_MS`] from now), under the wait's id,
//! as the supervisor's schedule request does (`tod_core::scheduler`). A node
//! with any wake is left alone, so running it again, or running it for a
//! node whose supervisor is scheduling its own, adds nothing. A node with no
//! row, or a lost one, gets none: there is no sandbox to wake.
//!
//! Runs after a seed, once at startup for every user, and for the nodes
//! whose waits a pushed batch changed.

use crate::users::{UserData, Users};
use crate::wakes::{Wake, Wakes, now_ms};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::BTreeSet;
use tod_store::cloud_nodes;
use tod_store::waits::WaitRepo;
use uuid::Uuid;

/// How soon a rebuilt wake for a wait already due is set.
pub const LEAD_MS: i64 = 5_000;

/// Registers the missing wakes for `user` (all its cloud nodes, or only
/// `only`); returns the wakes added.
pub fn rebuild(data: &UserData, user: &str, wakes: &Wakes, only: Option<&[Uuid]>, now: i64) -> Result<Vec<Wake>> {
    let conn = Connection::open(data.store.paths().db()).context("open the user's database")?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    let mut added = Vec::new();
    for row in cloud_nodes::list(&conn)? {
        if row.lost_at.is_some() || row.sandbox.is_empty() || only.is_some_and(|n| !n.contains(&row.node_id)) {
            continue;
        }
        let node = row.node_id.to_string();
        let new: Vec<Wake> = WaitRepo::new(&conn)
            .list_pending_for_node(row.node_id)?
            .into_iter()
            .map(|w| Wake {
                id: w.id.to_string(),
                user: user.to_string(),
                node: node.clone(),
                sandbox: row.sandbox.clone(),
                at: w.due_at.max(now + LEAD_MS),
            })
            .collect();
        // Under the registry's lock: all of the node's or none.
        if wakes.put_if_node_has_none(user, &node, new.clone())? > 0 {
            added.extend(new);
        }
    }
    Ok(added)
}

/// [`rebuild`], logging one line per wake and the error if it fails.
pub fn log_rebuild(data: &UserData, user: &str, wakes: &Wakes, only: Option<&[Uuid]>, when: &str) {
    match rebuild(data, user, wakes, only, now_ms()) {
        Ok(added) => {
            for w in added {
                eprintln!("tod-orchestrator: rebuilt the wake for {} at {} from its pending wait", w.sandbox, w.at);
            }
        }
        Err(err) => eprintln!("tod-orchestrator: could not rebuild {user}'s wakes {when}: {err:#}"),
    }
}

/// [`log_rebuild`] for every node of `user`, opening its store.
pub fn rebuild_user(users: &Users, user: &str, wakes: &Wakes, when: &str) {
    match users.get(user) {
        Ok(data) => log_rebuild(&data, user, wakes, None, when),
        Err(err) => eprintln!("tod-orchestrator: could not open {user}'s data to rebuild its wakes {when}: {err:#}"),
    }
}

/// Once at startup, for every user directory that was seeded.
pub fn rebuild_all(users: &Users, wakes: &Wakes) {
    let dir = users.base().join("users");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => return eprintln!("tod-orchestrator: cannot list {} to rebuild wakes: {e}", dir.display()),
    };
    let names: BTreeSet<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| crate::users::validate(n).is_ok() && crate::sync_backend::is_seeded(&dir.join(n)))
        .collect();
    for name in &names {
        rebuild_user(users, name, wakes, "at startup");
    }
}
