//! The node's timeline of significant events that are not agent sessions: a
//! gate check, a round of the phase's configured commands, and a phase change.
//! The activity panel (`tod_ui::unified::panels::transcript`) interleaves them
//! with the node's conversations, grouped by stay in a phase.
//!
//! One row per event, append-only. `detail` is JSON, shaped by `kind`:
//!
//! - [`KIND_GATE`]: `Vec<`[`GateLine`]`>`, one per criterion the check read;
//! - [`KIND_CHECKS`]: `Vec<`[`CommandResult`]`>`, one per command that ran;
//! - [`KIND_PHASE`]: [`PhaseChange`], the state the node left.
//!
//! Synced, so a cloud node's supervisor shows its work in the app; not in
//! `journey_changes` (command output may hold source text, as in
//! `crate::check_runs`).

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const TABLE: &str = "node_timeline";

pub const KIND_GATE: &str = "gate";
pub const KIND_CHECKS: &str = "checks";
pub const KIND_PHASE: &str = "phase";

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS node_timeline (
        id      BLOB PRIMARY KEY NOT NULL,
        node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        kind    TEXT NOT NULL,
        phase   TEXT NOT NULL,
        title   TEXT NOT NULL,
        failed  INTEGER NOT NULL DEFAULT 0,
        by_user INTEGER NOT NULL DEFAULT 0,
        detail  TEXT NOT NULL DEFAULT '',
        at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_node_timeline_node ON node_timeline(node_id, at);
";

/// One criterion a gate check read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateLine {
    pub criterion: String,
    pub outcome: String,
    pub detail: String,
}

/// One configured command a round ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandResult {
    /// The check's label, else its command.
    pub name: String,
    pub command: String,
    /// `passed`, `failed` or `broken` (`crate::check_runs::Verdict`).
    pub verdict: String,
    pub output: String,
}

impl CommandResult {
    pub fn is_failure(&self) -> bool {
        self.verdict != "passed"
    }
}

/// What a phase change left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseChange {
    /// The state the node was in (the same as the new one for a rerun).
    pub from: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub id: Uuid,
    pub node_id: Uuid,
    pub kind: String,
    /// The state the node was in when it happened (for a phase change, the
    /// one it entered).
    pub phase: String,
    pub title: String,
    pub failed: bool,
    /// A phase change the user made, rather than the lifecycle.
    pub by_user: bool,
    pub detail: String,
    pub at: i64,
}

impl TimelineEntry {
    pub fn new(node_id: Uuid, kind: &str, phase: &str, title: impl Into<String>, failed: bool, detail: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            node_id,
            kind: kind.to_string(),
            phase: phase.to_string(),
            title: title.into(),
            failed,
            by_user: false,
            detail,
            at: now_ms(),
        }
    }

    pub fn gate_lines(&self) -> Vec<GateLine> {
        serde_json::from_str(&self.detail).unwrap_or_default()
    }

    pub fn commands(&self) -> Vec<CommandResult> {
        serde_json::from_str(&self.detail).unwrap_or_default()
    }

    pub fn phase_change(&self) -> Option<PhaseChange> {
        serde_json::from_str(&self.detail).ok()
    }
}

pub struct TimelineRepo<'a> {
    conn: &'a Connection,
}

impl<'a> TimelineRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn record(&self, e: &TimelineEntry) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO node_timeline (id, node_id, kind, phase, title, failed, by_user, detail, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                uuid_to_blob(e.id),
                uuid_to_blob(e.node_id),
                e.kind,
                e.phase,
                e.title,
                e.failed,
                e.by_user,
                e.detail,
                e.at
            ],
        )?;
        Ok(())
    }

    /// A gate check, unless it read exactly what the node's latest gate check
    /// in the same phase did: a runner that polls a waiting gate would
    /// otherwise fill the timeline with the same answer. Returns whether it
    /// was recorded.
    pub fn record_gate_if_changed(&self, e: &TimelineEntry) -> Result<bool> {
        let latest: Option<(String, String, bool)> = self
            .conn
            .query_row(
                "SELECT phase, detail, failed FROM node_timeline WHERE node_id = ?1 AND kind = ?2
                 ORDER BY at DESC, rowid DESC LIMIT 1",
                params![uuid_to_blob(e.node_id), KIND_GATE],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((phase, detail, failed)) = latest
            && phase == e.phase
            && detail == e.detail
            && failed == e.failed
            && !self.phase_changed_since_latest_gate(e.node_id)?
        {
            return Ok(false);
        }
        self.record(e)?;
        Ok(true)
    }

    /// A phase change after the node's latest gate check: the next check is
    /// news even if it reads the same.
    fn phase_changed_since_latest_gate(&self, node: Uuid) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM node_timeline p WHERE p.node_id = ?1 AND p.kind = ?2
                 AND p.at >= (SELECT MAX(g.at) FROM node_timeline g WHERE g.node_id = ?1 AND g.kind = ?3))",
            params![uuid_to_blob(node), KIND_PHASE, KIND_GATE],
            |r| r.get(0),
        )?)
    }

    /// The node's entries, oldest first.
    pub fn list(&self, node: Uuid) -> Result<Vec<TimelineEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, kind, phase, title, failed, by_user, detail, at FROM node_timeline
             WHERE node_id = ?1 ORDER BY at, rowid",
        )?;
        let rows = stmt.query_map(params![uuid_to_blob(node)], row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r??);
        }
        Ok(out)
    }

    /// A change of phase the node's lifecycle made (or, with `by_user`, the
    /// user). Nothing when the state did not change and this is not a rerun.
    pub fn record_phase_change(&self, node: Uuid, from: &str, to: &str, by_user: bool) -> Result<()> {
        let mut entry = TimelineEntry::new(
            node,
            KIND_PHASE,
            to,
            if from == to { format!("{to} run again") } else { format!("{from} \u{2192} {to}") },
            false,
            serde_json::to_string(&PhaseChange { from: from.to_string() })?,
        );
        entry.by_user = by_user;
        self.record(&entry)
    }
}

fn row(r: &Row<'_>) -> rusqlite::Result<Result<TimelineEntry>> {
    let id: Vec<u8> = r.get(0)?;
    let node: Vec<u8> = r.get(1)?;
    Ok(Ok(TimelineEntry {
        id: blob_to_uuid_sql(&id)?,
        node_id: blob_to_uuid_sql(&node)?,
        kind: r.get(2)?,
        phase: r.get(3)?,
        title: r.get(4)?,
        failed: r.get(5)?,
        by_user: r.get(6)?,
        detail: r.get(7)?,
        at: r.get(8)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> (Connection, Uuid) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        conn.execute_batch(CREATE_TABLE).unwrap();
        (conn, Uuid::new_v4())
    }

    #[test]
    fn an_unchanged_gate_check_is_recorded_once() {
        let (conn, node) = conn();
        let repo = TimelineRepo::new(&conn);
        let gate = |detail: &str| TimelineEntry::new(node, KIND_GATE, "active", "Gate", true, detail.to_string());
        assert!(repo.record_gate_if_changed(&gate("[]")).unwrap());
        assert!(!repo.record_gate_if_changed(&gate("[]")).unwrap());
        assert!(repo.record_gate_if_changed(&gate("[{\"criterion\":\"a\",\"outcome\":\"fail\",\"detail\":\"\"}]")).unwrap());
        assert_eq!(repo.list(node).unwrap().len(), 2);
    }

    #[test]
    fn a_phase_change_makes_the_next_identical_gate_check_news() {
        let (conn, node) = conn();
        let repo = TimelineRepo::new(&conn);
        let gate = || TimelineEntry::new(node, KIND_GATE, "active", "Gate", true, "[]".to_string());
        assert!(repo.record_gate_if_changed(&gate()).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(2));
        repo.record_phase_change(node, "active", "active", true).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(repo.record_gate_if_changed(&gate()).unwrap());
    }

    #[test]
    fn phase_changes_round_trip_with_who_made_them() {
        let (conn, node) = conn();
        let repo = TimelineRepo::new(&conn);
        repo.record_phase_change(node, "design", "planning", false).unwrap();
        repo.record_phase_change(node, "planning", "design", true).unwrap();
        let list = repo.list(node).unwrap();
        assert_eq!(list[0].phase_change().unwrap().from, "design");
        assert!(!list[0].by_user && list[1].by_user);
        assert_eq!(list[1].phase, "design");
    }
}
