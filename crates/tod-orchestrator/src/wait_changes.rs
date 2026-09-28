//! What the orchestrator does when a client other than a node's own
//! supervisor changes one of that node's waits (the user cancels, satisfies,
//! or reschedules it in the app): poke the node's sandbox, so its supervisor
//! wakes, sees the wait as it now is, and carries on or reschedules its wake.
//!
//! Without the poke a sleeping node would not notice until the wake it
//! scheduled for the old time: a cancelled one-day wait would hold the node
//! for the day, and a wait moved sooner would still wake it late.
//!
//! Waits are not context (`tod_core::impact::IGNORED_TABLES`), so, like
//! [`crate::answers`], this is its own small handler: it finds the nodes
//! under the user's sync lock, right after the apply, and the pokes run after
//! the lock is released. The supervisor's own changes to its waits (it
//! settles them as it goes) never poke it.

use crate::wakes::{Wake, Wakes};
use anyhow::Result;
use rusqlite::Connection;
use tod_store::cloud_nodes::{self, CloudNodeRow};
use tod_store::sync::Change;

/// The cloud nodes whose waits `changes` (sent by `client`) touched.
pub fn record(conn: &Connection, changes: &[Change], client: &str) -> Result<Vec<CloudNodeRow>> {
    let mut out: Vec<CloudNodeRow> = Vec::new();
    for change in changes.iter().filter(|c| c.table == "waits") {
        let Some(node) = change.node_id else { continue };
        if client == tod_core::impact::supervisor_client(node) || out.iter().any(|r| r.node_id == node) {
            continue;
        }
        if let Some(row) = cloud_nodes::get(conn, node)? {
            eprintln!("tod-orchestrator: a wait changed on node {node}");
            out.push(row);
        }
    }
    Ok(out)
}

/// Pokes each node's sandbox (a failed poke is left to the wake timer).
pub fn poke(wakes: &Wakes, user: &str, nodes: &[CloudNodeRow]) {
    for row in nodes {
        wakes.poke_now(Wake {
            id: format!("wait-changed-{}", row.node_id),
            user: user.to_string(),
            node: row.node_id.to_string(),
            sandbox: row.sandbox.clone(),
            at: 0,
        });
    }
}
