//! What moving a node back a lifecycle state throws away.
//!
//! Sending a node back means "restart from there", so everything that
//! certified the states from the one it returns to up to the one it left no
//! longer counts: the gate outcomes recorded for those states' transitions,
//! the reports their protocol conversations recorded (a review's `done`, an
//! implementation's), and, once it is back at `verifying` or earlier, the
//! `verified` verdicts. Phase certificates need nothing here: they only
//! count within the stay that recorded them (`crate::phase`), and a node
//! that leaves a state starts a new stay when it comes back.
//!
//! Transcripts, review findings and the verdict history stay: they are what
//! happened, not what currently holds.
//! [`NodeRepo::set_lifecycle`](crate::outline::repos::NodeRepo::set_lifecycle)
//! calls [`clear_certifications`] for every backward move, so it is one rule
//! for the UI, `tod-cli` and the autopilot alike.

use crate::outline::uuid_blob::uuid_to_blob;
use crate::settings::VALID_LIFECYCLE_STATES;
use anyhow::Result;
use rusqlite::{Connection, params};
use uuid::Uuid;

fn rank(state: &str) -> Option<usize> {
    VALID_LIFECYCLE_STATES.iter().position(|s| *s == state)
}

/// The lifecycle state whose work a protocol's conversation certifies.
fn protocol_state(protocol: &str) -> Option<&'static str> {
    match protocol {
        "implementation" => Some("active"),
        "verification" => Some("verifying"),
        "review" | "fix" => Some("review"),
        "pr" => Some("pr"),
        _ => None,
    }
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?
        .exists(params![name])?)
}

/// Whether `from` to `to` is a move back, and so clears certifications.
pub fn is_move_back(from: &str, to: &str) -> bool {
    matches!((rank(from), rank(to)), (Some(from), Some(to)) if to < from)
}

/// Drop what certified the states from `to` through `from`, for a node that
/// moved back from `from` to `to`. Does nothing for any other move.
pub fn clear_certifications(conn: &Connection, node_id: Uuid, from: &str, to: &str) -> Result<()> {
    if !is_move_back(from, to) {
        return Ok(());
    }
    let (Some(from_rank), Some(to_rank)) = (rank(from), rank(to)) else {
        return Ok(());
    };
    let node = uuid_to_blob(node_id);

    // Gate outcomes for each transition out of a state being redone.
    let redone: Vec<&str> = VALID_LIFECYCLE_STATES[to_rank..=from_rank].to_vec();
    for state in &redone {
        conn.execute(
            "DELETE FROM node_gate_evaluations
             WHERE node_id = ?1
               AND criterion_id IN (SELECT id FROM gate_criteria WHERE from_state = ?2)",
            params![node, state],
        )?;
    }

    if table_exists(conn, "conversation_reports")? && table_exists(conn, "conversations")? {
        for protocol in ["implementation", "verification", "review", "fix", "pr"] {
            let Some(state) = protocol_state(protocol) else {
                continue;
            };
            if !redone.contains(&state) {
                continue;
            }
            conn.execute(
                "DELETE FROM conversation_reports
                 WHERE conversation_id IN (
                     SELECT id FROM conversations
                     WHERE (focus_node_id = ?1 OR (focus_kind = 'node' AND focus_id = ?1))
                       AND protocol = ?2)",
                params![node, protocol],
            )?;
        }
    }

    if to_rank <= rank("verifying").unwrap_or(usize::MAX) && table_exists(conn, "obligation_verdicts")? {
        crate::verification::VerdictRepo::new(conn).reopen_verified(
            node_id,
            &format!("The node moved back from {from} to {to}: verify again."),
        )?;
    }
    Ok(())
}
