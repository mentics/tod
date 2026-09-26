//! What the orchestrator does when a client's changes answer one of the
//! questions a node's supervisor or the watchdog asked
//! (`tod_core::stop_questions`): poke the node's sandbox, so its supervisor
//! wakes and acts on the answer (carries on, or stays stopped).
//!
//! Decisions are not context (`tod_core::impact::IGNORED_TABLES`), so this
//! is its own small handler beside [`crate::impact_handler`]: it finds the
//! nodes under the user's sync lock, right after the apply, and the pokes run
//! after the lock is released.

use crate::wakes::{Wake, Wakes};
use anyhow::Result;
use rusqlite::Connection;
use tod_store::cloud_nodes::{self, CloudNodeRow};
use tod_store::sync::{Change, SqlValue};
use uuid::Uuid;

/// The cloud nodes whose stop questions `changes` answer.
pub fn record(conn: &Connection, changes: &[Change]) -> Result<Vec<CloudNodeRow>> {
    let mut out: Vec<CloudNodeRow> = Vec::new();
    for change in changes.iter().filter(|c| c.table == "decision_answers") {
        let Some(SqlValue::Blob(hex)) = change.after.as_ref().and_then(|row| row.get("decision_id")) else {
            continue;
        };
        let Some(decision) = decode_hex(hex).and_then(|b| Uuid::from_slice(&b).ok()) else {
            continue;
        };
        let Some(node) = tod_core::stop_questions::node_of(conn, decision)? else {
            continue;
        };
        if out.iter().any(|r| r.node_id == node) {
            continue;
        }
        if let Some(row) = cloud_nodes::get(conn, node)? {
            eprintln!("tod-orchestrator: stop question answered on node {node}");
            out.push(row);
        }
    }
    Ok(out)
}

/// Pokes each node's sandbox (a failed poke is left to the wake timer).
pub fn poke(wakes: &Wakes, user: &str, nodes: &[CloudNodeRow]) {
    for row in nodes {
        wakes.poke_now(Wake {
            id: format!("answer-{}", row.node_id),
            user: user.to_string(),
            node: row.node_id.to_string(),
            sandbox: row.sandbox.clone(),
            at: 0,
        });
    }
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect()
}
