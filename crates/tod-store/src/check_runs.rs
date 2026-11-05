//! The latest run of each check a phase's gate runs (`lifecycle_config::Check`):
//! one row per node, phase and command, holding what that run found and the
//! fingerprint of the files it ran against.
//!
//! A run says [`Verdict::Passed`] (exit 0), [`Verdict::Failed`] (any other
//! exit, with its output) or [`Verdict::Broken`] (the command could not run at
//! all: not found, timed out, the sandbox failed). Only the first two are
//! about the code; a broken one is never sent to an agent to fix.
//!
//! Machine-local: not synced, and not in `journey_changes` (the output may
//! hold source text, and the gate's own result already journals pass or fail).

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS check_runs (
        node_id     BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        phase       TEXT NOT NULL,
        command     TEXT NOT NULL,
        label       TEXT NOT NULL,
        fingerprint TEXT NOT NULL,
        verdict     TEXT NOT NULL CHECK (verdict IN ('passed','failed','broken')),
        output      TEXT NOT NULL,
        ran_at      INTEGER NOT NULL,
        PRIMARY KEY (node_id, phase, command)
    );";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Verdict {
    Passed,
    Failed,
    Broken,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Passed => "passed",
            Verdict::Failed => "failed",
            Verdict::Broken => "broken",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "passed" => Ok(Verdict::Passed),
            "failed" => Ok(Verdict::Failed),
            "broken" => Ok(Verdict::Broken),
            other => anyhow::bail!("unknown check verdict {other:?}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckRun {
    pub node_id: Uuid,
    pub phase: String,
    pub command: String,
    pub label: String,
    /// What the files looked like when it ran (`tod_core::checks::fingerprint`).
    pub fingerprint: String,
    pub verdict: Verdict,
    /// The tail of its output, or why it could not run.
    pub output: String,
    pub ran_at: i64,
}

pub struct CheckRunRepo<'a> {
    conn: &'a Connection,
}

impl<'a> CheckRunRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Record a run, replacing the last one of the same command in the phase.
    pub fn record(&self, run: &CheckRun) -> Result<()> {
        self.conn.execute(
            "INSERT INTO check_runs (node_id, phase, command, label, fingerprint, verdict, output, ran_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(node_id, phase, command) DO UPDATE SET
                 label = excluded.label, fingerprint = excluded.fingerprint, verdict = excluded.verdict,
                 output = excluded.output, ran_at = excluded.ran_at",
            params![
                uuid_to_blob(run.node_id),
                run.phase,
                run.command,
                run.label,
                run.fingerprint,
                run.verdict.as_str(),
                run.output,
                run.ran_at
            ],
        )?;
        Ok(())
    }

    /// The last run of `command` in `phase` for the node.
    pub fn latest(&self, node_id: Uuid, phase: &str, command: &str) -> Result<Option<CheckRun>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM check_runs WHERE node_id = ?1 AND phase = ?2 AND command = ?3"),
                params![uuid_to_blob(node_id), phase, command],
                row,
            )
            .optional()?
            .transpose()
    }

    /// Forget a phase's runs (its checks were edited away).
    pub fn clear_phase(&self, node_id: Uuid, phase: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM check_runs WHERE node_id = ?1 AND phase = ?2", params![uuid_to_blob(node_id), phase])?;
        Ok(())
    }
}

const COLUMNS: &str = "node_id, phase, command, label, fingerprint, verdict, output, ran_at";

fn row(r: &Row<'_>) -> rusqlite::Result<Result<CheckRun>> {
    let node: Vec<u8> = r.get(0)?;
    let verdict: String = r.get(5)?;
    let run = CheckRun {
        node_id: blob_to_uuid_sql(&node)?,
        phase: r.get(1)?,
        command: r.get(2)?,
        label: r.get(3)?,
        fingerprint: r.get(4)?,
        verdict: Verdict::Passed,
        output: r.get(6)?,
        ran_at: r.get(7)?,
    };
    Ok(Verdict::parse(&verdict).map(|verdict| CheckRun { verdict, ..run }))
}

/// A run stamped now.
pub fn now() -> i64 {
    now_ms()
}
