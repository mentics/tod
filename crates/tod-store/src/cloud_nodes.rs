//! Which of a user's nodes run in the cloud, and when their context last
//! changed (`doc/cloud-sandboxes/autonomous-nodes.md`, "Changes that affect
//! a running node").
//!
//! One row per node the app sent to the cloud (`tod_core::cloud_sync::run_in_cloud`
//! writes it). The table is synced, so the orchestrator sees the user's
//! active cloud nodes, and each node's supervisor sees its own row.
//!
//! `context_changed_at` (ms) is set by the orchestrator when a change the
//! app (or another node) made affects the node (`tod_core::impact`). The
//! supervisor compares it with the value it last acted on, which it keeps
//! in its own state rather than writing back here: a write from the
//! supervisor would carry the whole row and could overwrite a newer mark.
//!
//! `lost_at` (ms, schema v70) is set by the orchestrator when it cannot poke
//! the node's sandbox because the sandbox is gone; the app replaces the
//! sandbox (it alone holds the user's tokens) and [`upsert`] clears it.
//!
//! Writes go through a plain connection (the caller's), like the rest of
//! sync; the store's read view is reloaded by the caller.

use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub const TABLE: &str = "cloud_nodes";

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS cloud_nodes (
        node_id            BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        sandbox            TEXT NOT NULL,
        user_name          TEXT NOT NULL,
        accepted_at        INTEGER NOT NULL,
        context_changed_at INTEGER
    );
";

/// The v70 migration: the `lost_at` column (idempotent).
pub fn add_lost_at(conn: &Connection) -> Result<()> {
    let has = conn
        .prepare("SELECT 1 FROM pragma_table_info('cloud_nodes') WHERE name = 'lost_at'")?
        .exists([])?;
    if !has {
        conn.execute_batch("ALTER TABLE cloud_nodes ADD COLUMN lost_at INTEGER")?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudNodeRow {
    pub node_id: Uuid,
    pub sandbox: String,
    pub user: String,
    pub accepted_at: i64,
    pub context_changed_at: Option<i64>,
    /// When the orchestrator found the sandbox gone (ms), until replaced.
    pub lost_at: Option<i64>,
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CloudNodeRow> {
    let blob: Vec<u8> = row.get(0)?;
    Ok(CloudNodeRow {
        node_id: blob_to_uuid_sql(&blob)?,
        sandbox: row.get(1)?,
        user: row.get(2)?,
        accepted_at: row.get(3)?,
        context_changed_at: row.get(4)?,
        lost_at: row.get(5)?,
    })
}

const COLUMNS: &str = "node_id, sandbox, user_name, accepted_at, context_changed_at, lost_at";

fn has_table(conn: &Connection) -> Result<bool> {
    Ok(conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'cloud_nodes'")?
        .exists([])?)
}

/// Records (or re-records) that `node` runs in `sandbox` for `user`. Keeps
/// an existing context mark; clears a lost mark (the sandbox was replaced).
pub fn upsert(conn: &Connection, node: Uuid, sandbox: &str, user: &str, accepted_at: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO cloud_nodes (node_id, sandbox, user_name, accepted_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(node_id) DO UPDATE SET sandbox = excluded.sandbox,
             user_name = excluded.user_name, accepted_at = excluded.accepted_at, lost_at = NULL",
        params![uuid_to_blob(node), sandbox, user, accepted_at],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, node: Uuid) -> Result<Option<CloudNodeRow>> {
    if !has_table(conn)? {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM cloud_nodes WHERE node_id = ?1"),
            params![uuid_to_blob(node)],
            from_row,
        )
        .optional()?)
}

/// Every cloud node, in no particular order.
pub fn list(conn: &Connection) -> Result<Vec<CloudNodeRow>> {
    if !has_table(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM cloud_nodes"))?;
    let rows = stmt.query_map([], from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Marks `node`'s context changed at `at` (never moving the mark back).
pub fn mark_context_changed(conn: &Connection, node: Uuid, at: i64) -> Result<()> {
    conn.execute(
        "UPDATE cloud_nodes SET context_changed_at = ?2
         WHERE node_id = ?1 AND (context_changed_at IS NULL OR context_changed_at < ?2)",
        params![uuid_to_blob(node), at],
    )?;
    Ok(())
}

/// Marks `node`'s sandbox lost at `at` (kept if already marked).
pub fn mark_lost(conn: &Connection, node: Uuid, at: i64) -> Result<()> {
    conn.execute(
        "UPDATE cloud_nodes SET lost_at = ?2 WHERE node_id = ?1 AND lost_at IS NULL",
        params![uuid_to_blob(node), at],
    )?;
    Ok(())
}

pub fn remove(conn: &Connection, node: Uuid) -> Result<()> {
    conn.execute("DELETE FROM cloud_nodes WHERE node_id = ?1", params![uuid_to_blob(node)])?;
    Ok(())
}
