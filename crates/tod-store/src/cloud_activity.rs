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
//! - an [`idle`] row with a `stopped` reason ([`idle_stopped`]) means the
//!   wake ended with the node needing someone (a stuck pull request, a spent
//!   budget, a stop question answered "leave it stopped"): the app shows the
//!   reason instead of "not started";
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

/// The v87 migration: the `stopped` column (idempotent). Added to an
/// existing table, so it is not in [`CREATE_TABLE`] (which an older store
/// runs first, and which must stay valid for it).
pub fn add_stopped(conn: &Connection) -> Result<()> {
    // A store older than the table has not made it yet when this runs.
    conn.execute_batch(CREATE_TABLE)?;
    let has = conn
        .prepare("SELECT 1 FROM pragma_table_info('cloud_activity') WHERE name = 'stopped'")?
        .exists([])?;
    if !has {
        conn.execute_batch("ALTER TABLE cloud_activity ADD COLUMN stopped TEXT")?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudBeat {
    pub node_id: Uuid,
    /// When this run of the supervisor began (ms).
    pub started_at: i64,
    /// The last time it said it was still working (ms).
    pub heartbeat_at: i64,
    /// What it is doing, in a few words (the node's lifecycle state).
    pub what: String,
    /// Why the last wake ended without the node finishing or waiting on
    /// something outside, in plain words, when it did (schema v87). Set by
    /// [`idle_stopped`], cleared by the next [`beat`]. `None` for a wake that
    /// finished, or only found a wait (the wait row says so).
    pub stopped: Option<String>,
}

/// What a run that ended leaves in `what`: the supervisor woke, did what
/// there was to do (often nothing: the node waits on something outside), and
/// went back to sleep. The row is kept rather than deleted, so a wake that
/// came and went between two syncs is still heard by the app.
pub const IDLE_WHAT: &str = "idle";

/// What the supervisor's first word of a wake says it is doing: it has begun
/// and is seeding its copy of the database, before the first real beat.
pub const STARTING_WHAT: &str = "starting";

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
        "INSERT INTO cloud_activity (node_id, started_at, heartbeat_at, what, stopped) VALUES (?1, ?2, ?2, ?3, NULL)
         ON CONFLICT(node_id) DO UPDATE SET heartbeat_at = excluded.heartbeat_at, what = excluded.what, stopped = NULL",
        params![uuid_to_blob(node), now_ms, what],
    )?;
    Ok(())
}

/// The run ended and the supervisor is going back to sleep: keeps the row,
/// stamped now, as [`IDLE_WHAT`]. Not a heartbeat of work, and never stale.
pub fn idle(conn: &Connection, node: Uuid, now_ms: i64) -> Result<()> {
    idle_stopped(conn, node, None, now_ms)
}

/// [`idle`], leaving `stopped` (why the wake ended with the node still
/// needing someone) for the app to show. `None` or a blank reason is no stop.
pub fn idle_stopped(conn: &Connection, node: Uuid, stopped: Option<&str>, now_ms: i64) -> Result<()> {
    let stopped = stopped.map(str::trim).filter(|s| !s.is_empty());
    beat(conn, node, IDLE_WHAT, now_ms)?;
    conn.execute(
        "UPDATE cloud_activity SET stopped = ?2 WHERE node_id = ?1",
        params![uuid_to_blob(node), stopped],
    )?;
    Ok(())
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
        stopped: row.get(4)?,
    })
}

pub fn get(conn: &Connection, node: Uuid) -> Result<Option<CloudBeat>> {
    if !has_table(conn)? {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT node_id, started_at, heartbeat_at, what, stopped FROM cloud_activity WHERE node_id = ?1",
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
        "SELECT node_id, started_at, heartbeat_at, what, stopped FROM cloud_activity WHERE node_id IN ({placeholders})"
    ))?;
    Ok(stmt
        .query_map(rusqlite::params_from_iter(params.iter()), from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;

    #[test]
    fn a_stop_reason_round_trips_and_the_next_beat_clears_it() {
        let dir = std::env::temp_dir().join(format!("tod-cloud-activity-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn).create_with_id(node, "n", "N").unwrap();
        beat(&conn, node, "planning", 10).unwrap();
        assert_eq!(get(&conn, node).unwrap().unwrap().stopped, None);
        idle_stopped(&conn, node, Some("  the pull request needs a person  "), 20).unwrap();
        let row = get(&conn, node).unwrap().unwrap();
        assert!(row.is_idle());
        assert_eq!(row.stopped.as_deref(), Some("the pull request needs a person"));
        assert_eq!(list_for_nodes(&conn, &[node]).unwrap()[0].stopped, row.stopped);
        // A blank reason is no stop; idle() is a wake that stopped for nothing.
        idle_stopped(&conn, node, Some("  "), 25).unwrap();
        assert_eq!(get(&conn, node).unwrap().unwrap().stopped, None);
        idle_stopped(&conn, node, Some("budget"), 30).unwrap();
        beat(&conn, node, "planning", 40).unwrap();
        assert_eq!(get(&conn, node).unwrap().unwrap().stopped, None, "working again clears the reason");
        idle_stopped(&conn, node, Some("budget"), 50).unwrap();
        idle(&conn, node, 60).unwrap();
        assert_eq!(get(&conn, node).unwrap().unwrap().stopped, None);
        // It travels in the synced change log with the rest of the row.
        let changes = crate::sync::export_changes(&conn, 0).unwrap();
        assert!(changes.iter().any(|c| c.table == TABLE));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_from_v86_adds_the_stopped_column_and_keeps_its_rows() {
        let dir = std::env::temp_dir().join(format!("tod-cloud-activity-migrate-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("tod.db");
        let node = Uuid::new_v4();
        {
            let conn = schema::open_writer_connection(&db).unwrap();
            NodeRepo::new(&conn).create_with_id(node, "n", "N").unwrap();
            beat(&conn, node, "review", 5).unwrap();
            conn.execute_batch("DROP TRIGGER IF EXISTS trg_sync_cloud_activity_insert; DROP TRIGGER IF EXISTS trg_sync_cloud_activity_update; DROP TRIGGER IF EXISTS trg_sync_cloud_activity_delete; ALTER TABLE cloud_activity DROP COLUMN stopped;").unwrap();
            conn.pragma_update(None, "user_version", 86).unwrap();
        }
        let conn = schema::open_writer_connection(&db).unwrap();
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, schema::CURRENT_USER_VERSION);
        let row = get(&conn, node).unwrap().unwrap();
        assert_eq!((row.what.as_str(), row.stopped), ("review", None));
        idle_stopped(&conn, node, Some("why"), 9).unwrap();
        assert_eq!(get(&conn, node).unwrap().unwrap().stopped.as_deref(), Some("why"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
