//! Explicit user requests to a node, as consumable rows (schema v89;
//! `doc/cloud-sandboxes/robustness-plan.md`, 3.4).
//!
//! "Run", "Run with a new session", "Check now" and "Keep going" used to be
//! marker files in the supervisor's state directory and pokes: a wake that
//! exited early left a marker behind, or a request was lost with the poke.
//! Each is now a row on the node, written by the app, synced to wherever the
//! node's supervisor (or local runner) is, and *consumed* at the first
//! boundary after it wakes: `consumed_at` / `consumed_by` record when and by
//! whom, and the app can show "consumed at T". A row never consumed stays
//! pending, so a wake that exits early (or an old supervisor that cannot read
//! the table) leaves it for the next.
//!
//! A pending request supersedes any older stop answer: the user's latest word
//! is the request (`tod_supervisor::answers`).
//!
//! The marker files remain, written beside the rows, for a supervisor built
//! before this table; a new one takes both.

use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use uuid::Uuid;

pub const TABLE: &str = "node_requests";

/// Run the node now (its supervisor looks again and goes on).
pub const KIND_RUN: &str = "run";
/// Run its current step in a new agent session.
pub const KIND_FRESH_SESSION: &str = "fresh_session";
/// Look at what the node waits on now.
pub const KIND_CHECK_NOW: &str = "check_now";
/// Go on past a stop question (answered to continue).
pub const KIND_KEEP_GOING: &str = "keep_going";
pub const KINDS: [&str; 4] = [KIND_RUN, KIND_FRESH_SESSION, KIND_CHECK_NOW, KIND_KEEP_GOING];

/// Table of schema v89, self-contained: nothing else is touched by its migration.
pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS node_requests (
        id           BLOB PRIMARY KEY NOT NULL,
        node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        kind         TEXT NOT NULL CHECK (kind IN ('run','fresh_session','check_now','keep_going')),
        requested_by TEXT NOT NULL,
        requested_at INTEGER NOT NULL,
        consumed_at  INTEGER,
        consumed_by  TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_node_requests_node ON node_requests(node_id, consumed_at, requested_at);
";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeRequest {
    pub id: Uuid,
    pub node_id: Uuid,
    pub kind: String,
    pub requested_by: String,
    pub requested_at: i64,
    pub consumed_at: Option<i64>,
    pub consumed_by: Option<String>,
}

impl NodeRequest {
    pub fn is_pending(&self) -> bool {
        self.consumed_at.is_none()
    }
}

/// Records a request of `kind` on `node` by `by`; returns its id.
pub fn record(conn: &Connection, node: Uuid, kind: &str, by: &str, at: i64) -> Result<Uuid> {
    if !KINDS.contains(&kind) {
        bail!("unknown request kind {kind:?}");
    }
    let id = Uuid::new_v4();
    conn.execute(
        "INSERT INTO node_requests (id, node_id, kind, requested_by, requested_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![uuid_to_blob(id), uuid_to_blob(node), kind, by, at],
    )?;
    Ok(id)
}

/// Marks `id` consumed by `by` at `at`. Already consumed: left as it was (the
/// first consumption is the record); returns whether this call consumed it.
pub fn consume(conn: &Connection, id: Uuid, by: &str, at: i64) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE node_requests SET consumed_at = ?2, consumed_by = ?3 WHERE id = ?1 AND consumed_at IS NULL",
        params![uuid_to_blob(id), at, by],
    )?;
    Ok(changed > 0)
}

fn read(row: &rusqlite::Row) -> rusqlite::Result<NodeRequest> {
    let id: Vec<u8> = row.get(0)?;
    let node: Vec<u8> = row.get(1)?;
    Ok(NodeRequest {
        id: blob_to_uuid_sql(&id)?,
        node_id: blob_to_uuid_sql(&node)?,
        kind: row.get(2)?,
        requested_by: row.get(3)?,
        requested_at: row.get(4)?,
        consumed_at: row.get(5)?,
        consumed_by: row.get(6)?,
    })
}

const COLUMNS: &str = "id, node_id, kind, requested_by, requested_at, consumed_at, consumed_by";

/// `node`'s requests not yet consumed, oldest first.
pub fn pending_for_node(conn: &Connection, node: Uuid) -> Result<Vec<NodeRequest>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM node_requests WHERE node_id = ?1 AND consumed_at IS NULL ORDER BY requested_at, rowid"
    ))?;
    let rows = stmt.query_map(params![uuid_to_blob(node)], read)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every request on `node`, oldest first (what the app shows).
pub fn list_for_node(conn: &Connection, node: Uuid) -> Result<Vec<NodeRequest>> {
    let mut stmt = conn
        .prepare(&format!("SELECT {COLUMNS} FROM node_requests WHERE node_id = ?1 ORDER BY requested_at, rowid"))?;
    let rows = stmt.query_map(params![uuid_to_blob(node)], read)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Whether any of `requests` is of `kind`.
pub fn any_of(requests: &[NodeRequest], kind: &str) -> bool {
    requests.iter().any(|r| r.kind == kind)
}

/// Whether any of `requests` asks the node to run (every kind but
/// `check_now`): each supersedes an older stop answer.
pub fn asks_to_run(requests: &[NodeRequest]) -> bool {
    requests.iter().any(|r| r.kind != KIND_CHECK_NOW)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> (Connection, Uuid) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE nodes (id BLOB PRIMARY KEY);").unwrap();
        conn.execute_batch(CREATE_TABLE).unwrap();
        let node = Uuid::new_v4();
        conn.execute("INSERT INTO nodes (id) VALUES (?1)", [uuid_to_blob(node)]).unwrap();
        (conn, node)
    }

    #[test]
    fn a_request_stays_pending_until_it_is_consumed_and_records_when() {
        let (conn, node) = conn();
        let a = record(&conn, node, KIND_RUN, "app", 10).unwrap();
        let b = record(&conn, node, KIND_CHECK_NOW, "app", 20).unwrap();
        assert_eq!(pending_for_node(&conn, node).unwrap().len(), 2);
        assert!(consume(&conn, a, "supervisor", 30).unwrap());
        // The first consumption is the record.
        assert!(!consume(&conn, a, "other", 99).unwrap());
        let all = list_for_node(&conn, node).unwrap();
        assert_eq!((all[0].consumed_at, all[0].consumed_by.as_deref()), (Some(30), Some("supervisor")));
        let pending = pending_for_node(&conn, node).unwrap();
        assert_eq!(pending.iter().map(|r| r.id).collect::<Vec<_>>(), [b]);
        assert!(!asks_to_run(&pending), "a check does not supersede a stop answer");
        assert!(record(&conn, node, "bogus", "app", 1).is_err());
    }
}
