//! One synced `run_state` row per node: what its runner says it is doing and
//! why (schema v91; `doc/cloud-sandboxes/robustness-plan.md`, 3.1).
//!
//! The app used to infer a cloud node's state from a heartbeat, waits,
//! decisions and plan steps, and the supervisor's own conclusion went to
//! stderr. Now whoever runs the node (the cloud supervisor, the local
//! autopilot runner, or the app itself while it sends a node to the cloud)
//! writes `{phase, reason_kind, reason_text, since, resumable_by}` at every
//! transition, and `tod_core::next_actor` reads it for cloud and local alike.
//!
//! The row is *what the runner said*, not proof that it is alive: `working`
//! is still proved by a live lease or heartbeat, and the heartbeat
//! (`crate::cloud_activity`) stays as the fallback for a supervisor built
//! before this table, which writes no row.
//!
//! A write that changes nothing (same phase, kind and text) leaves the row
//! alone, so `since` is when this state began and a repeat does not churn
//! the sync log or the journey.

use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub const TABLE: &str = "run_state";

/// Table of schema v91, self-contained: nothing else is touched by its migration.
pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS run_state (
        node_id      BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        phase        TEXT NOT NULL CHECK (phase IN ('starting','working','waiting','stopped','done')),
        reason_kind  TEXT NOT NULL,
        reason_text  TEXT NOT NULL,
        since        INTEGER NOT NULL,
        resumable_by TEXT NOT NULL,
        updated_at   INTEGER NOT NULL,
        writer       TEXT NOT NULL
    );
";

/// Where a node's run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Phase {
    /// The run was asked for and has not reported in (the app sending the
    /// node to the cloud, a supervisor seeding its copy).
    Starting,
    /// The runner is at work on the node.
    Working,
    /// The run ended or paused for something that arrives by itself (a bot's
    /// review, CI, a retry time) or for a request the user answers where
    /// requests are answered.
    Waiting,
    /// The run ended and the node needs someone: `resumable_by` says who.
    Stopped,
    /// The node reached `done`.
    Done,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Stopped => "stopped",
            Self::Done => "done",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "starting" => Self::Starting,
            "working" => Self::Working,
            "waiting" => Self::Waiting,
            "stopped" => Self::Stopped,
            "done" => Self::Done,
            other => bail!("unknown run phase {other:?}"),
        })
    }
}

/// Who can move a stopped or waiting run on: the user.
pub const RESUMABLE_BY_YOU: &str = "you";
/// The runner itself, when its schedule or a poll comes round.
pub const RESUMABLE_BY_RUNNER: &str = "runner";
/// Nobody: the run is over.
pub const RESUMABLE_BY_NOBODY: &str = "nobody";

/// What a transition says, before it is stamped.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Draft {
    pub phase: Phase,
    /// A stable machine word for why (`tod_core::run_state::kind`).
    pub reason_kind: String,
    /// The reason in plain words, as the user is shown it.
    pub reason_text: String,
    pub resumable_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunState {
    pub node_id: Uuid,
    pub phase: Phase,
    pub reason_kind: String,
    pub reason_text: String,
    /// When this state began (ms, the writer's clock).
    pub since: i64,
    pub resumable_by: String,
    /// The last write (ms, the writer's clock).
    pub updated_at: i64,
    /// Who wrote it (`app`, `supervisor:<sandbox>`, `local`).
    pub writer: String,
}

/// Records `draft` for `node` as written by `writer` at `at`. The same state
/// again keeps the row (and its `since`) as it was. Returns whether the row
/// changed.
pub fn set(conn: &Connection, node: Uuid, draft: &Draft, writer: &str, at: i64) -> Result<bool> {
    if let Some(now) = get(conn, node)?
        && now.phase == draft.phase
        && now.reason_kind == draft.reason_kind
        && now.reason_text == draft.reason_text
        && now.resumable_by == draft.resumable_by
    {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO run_state (node_id, phase, reason_kind, reason_text, since, resumable_by, updated_at, writer)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5, ?7)
         ON CONFLICT(node_id) DO UPDATE SET phase = excluded.phase, reason_kind = excluded.reason_kind,
             reason_text = excluded.reason_text, since = excluded.since, resumable_by = excluded.resumable_by,
             updated_at = excluded.updated_at, writer = excluded.writer",
        params![
            uuid_to_blob(node),
            draft.phase.as_str(),
            draft.reason_kind,
            draft.reason_text,
            at,
            draft.resumable_by,
            writer
        ],
    )?;
    Ok(true)
}

const COLUMNS: &str = "node_id, phase, reason_kind, reason_text, since, resumable_by, updated_at, writer";

fn has_table(conn: &Connection) -> Result<bool> {
    Ok(conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'run_state'")?.exists([])?)
}

/// A row as stored; the phase text is parsed by [`finish`], which turns an
/// unknown one into an error rather than a guess.
type Raw = (String, RunState);

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Raw> {
    let blob: Vec<u8> = row.get(0)?;
    let phase: String = row.get(1)?;
    Ok((
        phase,
        RunState {
            node_id: blob_to_uuid_sql(&blob)?,
            phase: Phase::Working,
            reason_kind: row.get(2)?,
            reason_text: row.get(3)?,
            since: row.get(4)?,
            resumable_by: row.get(5)?,
            updated_at: row.get(6)?,
            writer: row.get(7)?,
        },
    ))
}

fn finish((phase, mut state): Raw) -> Result<RunState> {
    state.phase = Phase::parse(&phase)?;
    Ok(state)
}

pub fn get(conn: &Connection, node: Uuid) -> Result<Option<RunState>> {
    if !has_table(conn)? {
        return Ok(None);
    }
    conn.query_row(&format!("SELECT {COLUMNS} FROM run_state WHERE node_id = ?1"), params![uuid_to_blob(node)], read)
        .optional()?
        .map(finish)
        .transpose()
}

/// The rows of any of `node_ids`.
pub fn list_for_nodes(conn: &Connection, node_ids: &[Uuid]) -> Result<Vec<RunState>> {
    if node_ids.is_empty() || !has_table(conn)? {
        return Ok(Vec::new());
    }
    let placeholders = node_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let params: Vec<Vec<u8>> = node_ids.iter().copied().map(uuid_to_blob).collect();
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM run_state WHERE node_id IN ({placeholders})"))?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), read)?.collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(finish).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;

    fn draft(phase: Phase, text: &str) -> Draft {
        Draft { phase, reason_kind: "k".into(), reason_text: text.into(), resumable_by: RESUMABLE_BY_YOU.into() }
    }

    #[test]
    fn a_state_round_trips_keeps_since_when_repeated_and_travels_in_the_sync_log() {
        let dir = std::env::temp_dir().join(format!("tod-run-state-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn).create_with_id(node, "n", "N").unwrap();
        assert!(get(&conn, node).unwrap().is_none());
        assert!(set(&conn, node, &draft(Phase::Starting, "sending"), "app", 10).unwrap());
        assert!(!set(&conn, node, &draft(Phase::Starting, "sending"), "app", 20).unwrap(), "a repeat changes nothing");
        let row = get(&conn, node).unwrap().unwrap();
        assert_eq!((row.phase, row.since, row.updated_at, row.writer.as_str()), (Phase::Starting, 10, 10, "app"));
        assert!(set(&conn, node, &draft(Phase::Stopped, "why"), "supervisor:s", 30).unwrap());
        let row = list_for_nodes(&conn, &[node]).unwrap().remove(0);
        assert_eq!((row.phase, row.since, row.reason_text.as_str()), (Phase::Stopped, 30, "why"));
        let changes = crate::sync::export_changes(&conn, 0).unwrap();
        assert!(changes.iter().any(|c| c.table == TABLE), "run_state is a synced table");
        let journaled: i64 = conn
            .query_row("SELECT COUNT(*) FROM journey_changes WHERE tbl = 'run_state'", [], |r| r.get(0))
            .unwrap();
        assert!(journaled >= 2, "a node-keyed trigger journals the writes: {journaled}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_phase_is_an_error_not_a_guess() {
        assert!(Phase::parse("sleeping").is_err());
        for p in [Phase::Starting, Phase::Working, Phase::Waiting, Phase::Stopped, Phase::Done] {
            assert_eq!(Phase::parse(p.as_str()).unwrap(), p);
        }
    }

    #[test]
    fn migration_from_v90_makes_the_table_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!("tod-run-state-migrate-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("tod.db");
        let node = Uuid::new_v4();
        {
            let conn = schema::open_writer_connection(&db).unwrap();
            NodeRepo::new(&conn).create_with_id(node, "n", "N").unwrap();
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS trg_sync_run_state_insert; DROP TRIGGER IF EXISTS trg_sync_run_state_update;
                 DROP TRIGGER IF EXISTS trg_sync_run_state_delete; DROP TRIGGER IF EXISTS trg_journey_run_state_insert;
                 DROP TRIGGER IF EXISTS trg_journey_run_state_update; DROP TRIGGER IF EXISTS trg_journey_run_state_delete;
                 DROP TABLE run_state;",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 90).unwrap();
        }
        let conn = schema::open_writer_connection(&db).unwrap();
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, schema::CURRENT_USER_VERSION);
        assert!(set(&conn, node, &draft(Phase::Working, "w"), "local", 5).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
