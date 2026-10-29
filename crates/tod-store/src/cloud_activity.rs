//! The proof that a node's supervisor is working on it in the cloud
//! (`doc/lifecycle/next-actor.md`, "`Working` must be proved, not claimed").
//!
//! A node's activity leases (`crate::activity`) are machine-local, so the app
//! never sees the ones its supervisor holds in its sandbox. This table is the
//! synced counterpart: one row per node, written only by that node's
//! supervisor while its autopilot runs. It is made when the run starts,
//! refreshed ([`beat`]) every [`BEAT_EVERY_MS`], and deleted ([`end`]) when
//! the run stops, so:
//!
//! - a row whose `heartbeat_at` is fresh ([`CloudBeat::is_live`]) means the
//!   supervisor is at work on the node, as far as the app can tell;
//! - a row that has stopped beating and was not marked [`idle`] means the
//!   supervisor (or its sandbox) died mid-run;
//! - an [`idle`] row means the supervisor woke and went back to sleep (its
//!   `heartbeat_at` says when), which is how the app hears of a wake that
//!   found only a wait.
//!
//! The app compares `heartbeat_at` with its own clock, so the window is wide
//! ([`STALE_AFTER_MS`]) to absorb clock skew and the lag of a sync.
//!
//! Synced, but not in `journey_changes`: a heartbeat is high-frequency
//! bookkeeping, not a change to the node (the same reason as
//! `crate::activity`). It has its own table, rather than columns on
//! `cloud_nodes`, because a supervisor's write carries the whole row and
//! would overwrite the marks the orchestrator and the app keep there
//! (`context_changed_at`, `lost_at`).

use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub const TABLE: &str = "cloud_activity";

/// How often the supervisor refreshes its row (ms).
pub const BEAT_EVERY_MS: i64 = 15_000;
/// A row not refreshed for this long is stale: four missed beats, which also
/// covers the app pulling up to [`crate::sync`]'s poll interval late and a
/// few seconds of clock skew between the sandbox and this machine.
pub const STALE_AFTER_MS: i64 = 60_000;

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS cloud_activity (
        node_id      BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        started_at   INTEGER NOT NULL,
        heartbeat_at INTEGER NOT NULL,
        what         TEXT NOT NULL DEFAULT ''
    );
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudBeat {
    pub node_id: Uuid,
    /// When this run of the supervisor began (ms).
    pub started_at: i64,
    /// The last time it said it was still working (ms).
    pub heartbeat_at: i64,
    /// What it is doing, in a few words (the node's lifecycle state).
    pub what: String,
}

/// What a run that ended leaves in `what`: the supervisor woke, did what
/// there was to do (often nothing: the node waits on something outside), and
/// went back to sleep. The row is kept rather than deleted, so a wake that
/// came and went between two syncs is still heard by the app.
pub const IDLE_WHAT: &str = "idle";

impl CloudBeat {
    /// The supervisor said it was working recently enough to still be.
    pub fn is_live(&self, now_ms: i64) -> bool {
        !self.is_idle() && now_ms - self.heartbeat_at <= STALE_AFTER_MS
    }

    /// The last run ended ([`idle`]); the supervisor is asleep, not lost.
    pub fn is_idle(&self) -> bool {
        self.what == IDLE_WHAT
    }
}

fn has_table(conn: &Connection) -> Result<bool> {
    Ok(conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'cloud_activity'")?
        .exists([])?)
}

/// The supervisor is working on `node`: makes the row (keeping `started_at`
/// of a run already recorded) and refreshes it.
pub fn beat(conn: &Connection, node: Uuid, what: &str, now_ms: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO cloud_activity (node_id, started_at, heartbeat_at, what) VALUES (?1, ?2, ?2, ?3)
         ON CONFLICT(node_id) DO UPDATE SET heartbeat_at = excluded.heartbeat_at, what = excluded.what",
        params![uuid_to_blob(node), now_ms, what],
    )?;
    Ok(())
}

/// The run ended and the supervisor is going back to sleep: keeps the row,
/// stamped now, as [`IDLE_WHAT`]. Not a heartbeat of work, and never stale.
pub fn idle(conn: &Connection, node: Uuid, now_ms: i64) -> Result<()> {
    beat(conn, node, IDLE_WHAT, now_ms)
}

/// The node left the cloud: drops the row.
pub fn end(conn: &Connection, node: Uuid) -> Result<()> {
    conn.execute("DELETE FROM cloud_activity WHERE node_id = ?1", params![uuid_to_blob(node)])?;
    Ok(())
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CloudBeat> {
    let blob: Vec<u8> = row.get(0)?;
    Ok(CloudBeat {
        node_id: blob_to_uuid_sql(&blob)?,
        started_at: row.get(1)?,
        heartbeat_at: row.get(2)?,
        what: row.get(3)?,
    })
}

pub fn get(conn: &Connection, node: Uuid) -> Result<Option<CloudBeat>> {
    if !has_table(conn)? {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT node_id, started_at, heartbeat_at, what FROM cloud_activity WHERE node_id = ?1",
            params![uuid_to_blob(node)],
            from_row,
        )
        .optional()?)
}

/// The rows of any of `node_ids`, live or stale.
pub fn list_for_nodes(conn: &Connection, node_ids: &[Uuid]) -> Result<Vec<CloudBeat>> {
    if node_ids.is_empty() || !has_table(conn)? {
        return Ok(Vec::new());
    }
    let placeholders = node_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let params: Vec<Vec<u8>> = node_ids.iter().copied().map(uuid_to_blob).collect();
    let mut stmt = conn.prepare(&format!(
        "SELECT node_id, started_at, heartbeat_at, what FROM cloud_activity WHERE node_id IN ({placeholders})"
    ))?;
    Ok(stmt
        .query_map(rusqlite::params_from_iter(params.iter()), from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
