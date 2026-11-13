//! What a node that reached `done` leaves behind: nothing that waits on the
//! user. Entering `done` (`NodeRepo::set_lifecycle_by`, the one place every
//! route goes through: the gate's advance, the state chip, `tod-cli
//! lifecycle set`, the supervisor's mirror) calls [`close`], which withdraws
//! the node's pending requests and moves a stale run state (`waiting`,
//! `stopped`) to `done`, so a finished node never reads "the agent asked you
//! a question". [`sweep`] does the same for rows left by an older build or
//! written after the node finished (a supervisor's last beat, a sync).

use crate::decisions::DecisionRepo;
use crate::outline::uuid_blob::blob_to_uuid_sql;
use crate::run_state::{self, Draft, Phase, RESUMABLE_BY_NOBODY};
use anyhow::Result;
use rusqlite::{Connection, params};
use uuid::Uuid;

/// The run-state kind and text a closed node gets.
const KIND: &str = "done";
const TEXT: &str = "the node reached done; its open requests were withdrawn";

/// Withdraws `node`'s pending decisions and settles its run state as `done`.
/// Returns how many rows changed.
pub fn close(conn: &Connection, node: Uuid, at: i64) -> Result<usize> {
    let mut changed = DecisionRepo::new(conn).withdraw_pending_for_node(node)?;
    if let Some(state) = run_state::get(conn, node)?
        && state.phase != Phase::Done
    {
        let draft = Draft {
            phase: Phase::Done,
            reason_kind: KIND.into(),
            reason_text: TEXT.into(),
            resumable_by: RESUMABLE_BY_NOBODY.into(),
        };
        if run_state::set(conn, node, &draft, "app", at)? {
            changed += 1;
        }
    }
    Ok(changed)
}

/// [`close`] for every `done` node that still has a pending request or a run
/// state that is not `done`. Returns the nodes it changed.
pub fn sweep(conn: &Connection, at: i64) -> Result<Vec<Uuid>> {
    let mut stmt = conn.prepare(
        "SELECT l.node_id FROM node_lifecycle l WHERE l.state = 'done' AND (
           EXISTS (SELECT 1 FROM decisions d WHERE d.node_id = l.node_id AND d.status = 'pending')
           OR EXISTS (SELECT 1 FROM run_state r WHERE r.node_id = l.node_id AND r.phase <> 'done'))",
    )?;
    let nodes: Vec<Uuid> = stmt
        .query_map(params![], |row| blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut changed = Vec::new();
    for node in nodes {
        if close(conn, node, at)? > 0 {
            changed.push(node);
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests;
