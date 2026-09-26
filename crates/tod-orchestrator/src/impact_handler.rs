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
