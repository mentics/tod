//! What the orchestrator does with a client's changes once applied: find the
//! user's running cloud nodes they affect (`tod_core::impact`), mark each
//! one's context changed (`tod_store::cloud_nodes`, synced, so the node's
//! supervisor sees it on its next pull), and poke its sandbox so a sleeping
//! node wakes to take it.
//!
//! The mark is written in the same connection, under the user's sync lock,
//! right after the apply; the pokes run after the lock is released
//! ([`poke`]), and a failed one is retried by the wake timer.

use crate::wakes::{Wake, Wakes};
use anyhow::Result;
use rusqlite::Connection;
use tod_core::impact::{self, SqlOutline};
use tod_store::cloud_nodes::{self, CloudNodeRow};
use tod_store::sync::Change;

/// Marks every active cloud node `changes` (sent by `client`) affect as
/// changed at `now`. Returns those nodes.
pub fn record(conn: &Connection, changes: &[Change], client: &str, now: i64) -> Result<Vec<CloudNodeRow>> {
    let active = cloud_nodes::list(conn)?;
    if active.is_empty() || changes.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<_> = active.iter().map(|r| r.node_id).collect();
    let hit = impact::affected(changes, Some(client), &ids, &SqlOutline(conn));
    let mut out = Vec::new();
    for row in active {
        if let Some(reasons) = hit.get(&row.node_id) {
            eprintln!("tod-orchestrator: context changed for node {} ({reasons:?})", row.node_id);
            cloud_nodes::mark_context_changed(conn, row.node_id, now)?;
            out.push(row);
        }
    }
    Ok(out)
}

/// Pokes each node's sandbox (a failed poke is left to the wake timer).
pub fn poke(wakes: &Wakes, user: &str, nodes: &[CloudNodeRow]) {
    for row in nodes {
        wakes.poke_now(Wake {
            id: format!("context-{}", row.node_id),
            user: user.to_string(),
            node: row.node_id.to_string(),
            sandbox: row.sandbox.clone(),
            at: 0,
        });
    }
}

/// Marks the wake's node lost in its user's database (under the user's sync
/// lock), so the app sees `lost_at` on its next sync and replaces the sandbox.
pub fn mark_lost(users: &crate::users::Users, wake: &Wake, now: i64) -> Result<()> {
    let node = uuid::Uuid::parse_str(&wake.node)?;
    let user = users.get(&wake.user)?;
    let _guard = user.sync_lock.lock().unwrap_or_else(|e| e.into_inner());
    let _ = user.store.flush_on_quit();
    let conn = Connection::open(user.store.paths().db())?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    cloud_nodes::mark_lost(&conn, node, now)?;
    drop(conn);
    let _ = user.store.reload_if_stale();
    Ok(())
}

/// The [`Wakes`] lost handler that calls [`mark_lost`].
pub fn lost_handler(users: std::sync::Arc<crate::users::Users>) -> crate::wakes::LostHandler {
    Box::new(move |wake: &Wake| {
        if let Err(err) = mark_lost(&users, wake, crate::wakes::now_ms()) {
            eprintln!("tod-orchestrator: mark node {} lost: {err:#}", wake.node);
        }
    })
}
