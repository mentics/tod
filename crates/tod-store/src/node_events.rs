//! Events that reached a node from outside: webhooks the orchestrator routed
//! to it (`doc/cloud-sandboxes/orchestrator.md`, "Webhooks").
//!
//! One row per event per node. The table is synced, so the node's supervisor
//! sees what woke it, and the app can show it. `keys` are the event's match
//! keys (space-separated, e.g. `github:pr:12:checks github:branch:feat:checks`),
//! the same keys an `event` wait's `match_spec` is matched against;
//! `summary` is one human line; `payload` is the raw JSON as received.
//!
//! Written by the orchestrator through a plain connection, like the rest of
//! sync; the caller reloads the store's read view.

use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, params};
use uuid::Uuid;

pub const TABLE: &str = "node_events";

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS node_events (
        id          BLOB PRIMARY KEY NOT NULL,
        node_id     BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        source      TEXT NOT NULL,
        kind        TEXT NOT NULL,
        keys        TEXT NOT NULL DEFAULT '',
        summary     TEXT NOT NULL DEFAULT '',
        payload     TEXT NOT NULL DEFAULT '',
        received_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_node_events_node ON node_events(node_id, received_at);
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeEvent {
    pub id: Uuid,
    pub node_id: Uuid,
    pub source: String,
    pub kind: String,
    pub keys: String,
    pub summary: String,
    pub payload: String,
    pub received_at: i64,
}

/// Records an event on `node`; returns its id.
#[allow(clippy::too_many_arguments)]
pub fn record(
    conn: &Connection,
    node: Uuid,
    source: &str,
    kind: &str,
    keys: &str,
    summary: &str,
    payload: &str,
    received_at: i64,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    conn.execute(
        "INSERT INTO node_events (id, node_id, source, kind, keys, summary, payload, received_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![uuid_to_blob(id), uuid_to_blob(node), source, kind, keys, summary, payload, received_at],
    )?;
    Ok(id)
}

/// `node`'s events, oldest first.
pub fn list_for_node(conn: &Connection, node: Uuid) -> Result<Vec<NodeEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, node_id, source, kind, keys, summary, payload, received_at
         FROM node_events WHERE node_id = ?1 ORDER BY received_at, rowid",
    )?;
    let rows = stmt
        .query_map(params![uuid_to_blob(node)], |row| {
            let id: Vec<u8> = row.get(0)?;
            let node: Vec<u8> = row.get(1)?;
            Ok(NodeEvent {
                id: blob_to_uuid_sql(&id)?,
                node_id: blob_to_uuid_sql(&node)?,
                source: row.get(2)?,
                kind: row.get(3)?,
                keys: row.get(4)?,
                summary: row.get(5)?,
                payload: row.get(6)?,
                received_at: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
