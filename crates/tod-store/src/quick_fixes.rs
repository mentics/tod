//! Quick-fix attempts (schema v110; `doc/requests/design.md` section 5).
//!
//! Rung 1 of the escalation ladder: before a failing check goes to the Fix
//! protocol, a cheap session (`quick_fix`, Haiku at low effort) gets one try
//! at that one check. Every attempt is recorded here, per check, with how it
//! ended:
//!
//! - [`OUTCOME_OPEN`]: the session is running (or was, when the app stopped);
//! - [`OUTCOME_SUCCEEDED`]: tod ran the check again and it passed (a CI check:
//!   the pushed commit's CI run passed);
//! - [`OUTCOME_ESCALATED`]: it did not, and the check went to the Fix protocol
//!   (the pull request agent for a CI check) with the session's `notes`.
//!
//! The rows are what [`stats`] measures: how often each check's quick fixes
//! fail, so a check whose quick fixes usually fail can be set to skip rung 1
//! ([`crate::lifecycle_config::Check::skip_quick_fix`]).
//!
//! Synced and journaled like the node's other rows.

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const TABLE: &str = "node_quick_fixes";

/// The failing thing was one of tod's own checks (`lifecycle_config::Check`).
pub const SOURCE_CHECK: &str = "check";
/// The failing thing was a CI check on a pull request.
pub const SOURCE_CI: &str = "ci";

pub const OUTCOME_OPEN: &str = "open";
pub const OUTCOME_SUCCEEDED: &str = "succeeded";
pub const OUTCOME_ESCALATED: &str = "escalated";

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS node_quick_fixes (
        id              BLOB PRIMARY KEY NOT NULL,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        source          TEXT NOT NULL CHECK (source IN ('check', 'ci')),
        phase           TEXT NOT NULL,
        check_name      TEXT NOT NULL,
        check_key       TEXT NOT NULL,
        command         TEXT NOT NULL DEFAULT '',
        output          TEXT NOT NULL DEFAULT '',
        fingerprint     TEXT NOT NULL DEFAULT '',
        conversation_id BLOB,
        outcome         TEXT NOT NULL CHECK (outcome IN ('open', 'succeeded', 'escalated')),
        notes           TEXT NOT NULL DEFAULT '',
        turns           INTEGER NOT NULL DEFAULT 0,
        head            TEXT NOT NULL DEFAULT '',
        started_at      INTEGER NOT NULL,
        finished_at     INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_node_quick_fixes_node ON node_quick_fixes(node_id, started_at);
    CREATE INDEX IF NOT EXISTS idx_node_quick_fixes_check ON node_quick_fixes(check_key, outcome);
";

const COLUMNS: &str = "id, node_id, source, phase, check_name, check_key, command, output, fingerprint, \
    conversation_id, outcome, notes, turns, head, started_at, finished_at";

/// One quick-fix attempt on one failing check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickFix {
    pub id: Uuid,
    pub node_id: Uuid,
    /// [`SOURCE_CHECK`] or [`SOURCE_CI`].
    pub source: String,
    /// The lifecycle state it was tried in.
    pub phase: String,
    /// What messages call the check.
    pub check_name: String,
    /// A stable key for the check across attempts and nodes: the directory and
    /// command of one of tod's own checks, a CI check's name.
    pub check_key: String,
    /// The command that failed (a CI check has none here).
    pub command: String,
    /// The failing output's tail, as the session was given it.
    pub output: String,
    /// What the failure was on: the round's files fingerprint, or the head.
    pub fingerprint: String,
    /// The session that tried.
    pub conversation_id: Option<Uuid>,
    pub outcome: String,
    /// What the session did and left undone, for the Fix session on escalation.
    pub notes: String,
    pub turns: i64,
    /// The commit it made, when it made one.
    pub head: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

impl QuickFix {
    /// A new attempt, open, started now.
    pub fn open(node_id: Uuid, source: &str, phase: &str, check_name: &str, check_key: &str, command: &str, output: &str, fingerprint: &str) -> Self {
        Self {
            id: Uuid::new_v4(),
            node_id,
            source: source.to_string(),
            phase: phase.to_string(),
            check_name: check_name.to_string(),
            check_key: check_key.to_string(),
            command: command.to_string(),
            output: output.to_string(),
            fingerprint: fingerprint.to_string(),
            conversation_id: None,
            outcome: OUTCOME_OPEN.to_string(),
            notes: String::new(),
            turns: 0,
            head: String::new(),
            started_at: now_ms(),
            finished_at: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.outcome == OUTCOME_OPEN
    }

    /// Ends the attempt with `outcome` ([`OUTCOME_SUCCEEDED`] or
    /// [`OUTCOME_ESCALATED`]), finished now.
    pub fn finish(&mut self, outcome: &str) {
        self.outcome = outcome.to_string();
        self.finished_at = Some(now_ms());
    }
}

/// The key a tod check is known by across attempts: where it runs and what.
pub fn check_key(dir: &str, command: &str) -> String {
    format!("{}\u{1f}{}", dir.trim(), command.trim())
}

fn row(r: &Row<'_>) -> rusqlite::Result<QuickFix> {
    let id: Vec<u8> = r.get(0)?;
    let node: Vec<u8> = r.get(1)?;
    let conversation: Option<Vec<u8>> = r.get(9)?;
    Ok(QuickFix {
        id: blob_to_uuid_sql(&id)?,
        node_id: blob_to_uuid_sql(&node)?,
        source: r.get(2)?,
        phase: r.get(3)?,
        check_name: r.get(4)?,
        check_key: r.get(5)?,
        command: r.get(6)?,
        output: r.get(7)?,
        fingerprint: r.get(8)?,
        conversation_id: conversation.as_deref().map(blob_to_uuid_sql).transpose()?,
        outcome: r.get(10)?,
        notes: r.get(11)?,
        turns: r.get(12)?,
        head: r.get(13)?,
        started_at: r.get(14)?,
        finished_at: r.get(15)?,
    })
}

/// Records `fix`: inserts it, or updates the row of the same id (the session
/// started, a turn ran, the outcome is known).
pub fn upsert(conn: &Connection, fix: &QuickFix) -> Result<()> {
    if ![SOURCE_CHECK, SOURCE_CI].contains(&fix.source.as_str()) {
        bail!("unknown quick fix source {:?}", fix.source);
    }
    if ![OUTCOME_OPEN, OUTCOME_SUCCEEDED, OUTCOME_ESCALATED].contains(&fix.outcome.as_str()) {
        bail!("unknown quick fix outcome {:?}", fix.outcome);
    }
    conn.execute(
        &format!(
            "INSERT INTO node_quick_fixes ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(id) DO UPDATE SET conversation_id = excluded.conversation_id, outcome = excluded.outcome,
                 notes = excluded.notes, turns = excluded.turns, head = excluded.head, finished_at = excluded.finished_at"
        ),
        params![
            uuid_to_blob(fix.id),
            uuid_to_blob(fix.node_id),
            fix.source,
            fix.phase,
            fix.check_name,
            fix.check_key,
            fix.command,
            fix.output,
            fix.fingerprint,
            fix.conversation_id.map(uuid_to_blob),
            fix.outcome,
            fix.notes,
            fix.turns,
            fix.head,
            fix.started_at,
            fix.finished_at,
        ],
    )?;
    // A climb up the ladder is a lesson (`doc/requests/design.md` 11.1).
    if fix.outcome == OUTCOME_ESCALATED {
        crate::lessons::record_quick_fix_escalation(conn, fix)?;
    }
    Ok(())
}

/// The attempt `id`.
pub fn get(conn: &Connection, id: Uuid) -> Result<Option<QuickFix>> {
    Ok(conn
        .query_row(&format!("SELECT {COLUMNS} FROM node_quick_fixes WHERE id = ?1"), params![uuid_to_blob(id)], row)
        .optional()?)
}

/// The attempt a conversation is working, when it is one.
pub fn for_conversation(conn: &Connection, conversation: Uuid) -> Result<Option<QuickFix>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM node_quick_fixes WHERE conversation_id = ?1 ORDER BY started_at DESC LIMIT 1"),
            params![uuid_to_blob(conversation)],
            row,
        )
        .optional()?)
}

/// The node's attempt still open, the newest.
pub fn open_for_node(conn: &Connection, node: Uuid) -> Result<Option<QuickFix>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM node_quick_fixes WHERE node_id = ?1 AND outcome = 'open' ORDER BY started_at DESC, rowid DESC LIMIT 1"),
            params![uuid_to_blob(node)],
            row,
        )
        .optional()?)
}

/// Every attempt on the node, oldest first.
pub fn list_for_node(conn: &Connection, node: Uuid) -> Result<Vec<QuickFix>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM node_quick_fixes WHERE node_id = ?1 ORDER BY started_at, rowid"))?;
    let rows = stmt.query_map(params![uuid_to_blob(node)], row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The node's attempts on one check, oldest first.
pub fn for_check(conn: &Connection, node: Uuid, source: &str, check_key: &str) -> Result<Vec<QuickFix>> {
    Ok(list_for_node(conn, node)?.into_iter().filter(|q| q.source == source && q.check_key == check_key).collect())
}

/// How one check's quick fixes have gone, over every node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckStat {
    pub source: String,
    pub check_name: String,
    pub check_key: String,
    pub succeeded: u32,
    pub escalated: u32,
}

impl CheckStat {
    /// Attempts that ended (an open one is neither).
    pub fn finished(&self) -> u32 {
        self.succeeded + self.escalated
    }

    /// The share of finished attempts that escalated, 0.0 to 1.0; `None`
    /// with no finished attempt.
    pub fn failure_rate(&self) -> Option<f64> {
        (self.finished() > 0).then(|| f64::from(self.escalated) / f64::from(self.finished()))
    }
}

/// Per check, over every node: how many quick fixes succeeded and how many
/// escalated, most escalated first.
pub fn stats(conn: &Connection) -> Result<Vec<CheckStat>> {
    let mut stmt = conn.prepare(
        "SELECT source, check_key, MAX(check_name),
                SUM(CASE WHEN outcome = 'succeeded' THEN 1 ELSE 0 END),
                SUM(CASE WHEN outcome = 'escalated' THEN 1 ELSE 0 END)
         FROM node_quick_fixes GROUP BY source, check_key",
    )?;
    let mut out: Vec<CheckStat> = stmt
        .query_map([], |r| {
            Ok(CheckStat {
                source: r.get(0)?,
                check_key: r.get(1)?,
                check_name: r.get(2)?,
                succeeded: r.get(3)?,
                escalated: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    out.sort_by(|a, b| b.escalated.cmp(&a.escalated).then(a.check_name.cmp(&b.check_name)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;

    fn setup() -> (Connection, Uuid) {
        let dir = tod_test_util::scratch_dir(format!("tod-quick-fixes-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn).create_with_id(node, "n", "N").unwrap();
        (conn, node)
    }

    fn attempt(node: Uuid, check: &str) -> QuickFix {
        QuickFix::open(node, SOURCE_CHECK, "review", check, &check_key("", check), check, "boom", "fp")
    }

    #[test]
    fn an_attempt_is_opened_then_finished_and_read_back() {
        let (conn, node) = setup();
        let mut q = attempt(node, "lint");
        upsert(&conn, &q).unwrap();
        assert_eq!(open_for_node(&conn, node).unwrap().unwrap().id, q.id);
        q.outcome = OUTCOME_ESCALATED.into();
        q.notes = "tried x".into();
        q.turns = 3;
        q.finished_at = Some(5);
        upsert(&conn, &q).unwrap();
        let back = get(&conn, q.id).unwrap().unwrap();
        assert_eq!((back.outcome.as_str(), back.notes.as_str(), back.turns), (OUTCOME_ESCALATED, "tried x", 3));
        assert!(open_for_node(&conn, node).unwrap().is_none());
        assert_eq!(for_check(&conn, node, SOURCE_CHECK, &check_key("", "lint")).unwrap().len(), 1);
    }

    #[test]
    fn the_failure_rate_is_counted_per_check_over_finished_attempts() {
        let (conn, node) = setup();
        for outcome in [OUTCOME_SUCCEEDED, OUTCOME_ESCALATED, OUTCOME_ESCALATED, OUTCOME_OPEN] {
            let mut q = attempt(node, "types");
            q.outcome = outcome.into();
            upsert(&conn, &q).unwrap();
        }
        let mut other = attempt(node, "lint");
        other.outcome = OUTCOME_SUCCEEDED.into();
        upsert(&conn, &other).unwrap();
        let stats = stats(&conn).unwrap();
        assert_eq!(stats[0].check_name, "types");
        assert_eq!((stats[0].succeeded, stats[0].escalated, stats[0].finished()), (1, 2, 3));
        assert!((stats[0].failure_rate().unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(stats[1].failure_rate(), Some(0.0));
    }

    #[test]
    fn an_unknown_outcome_or_source_is_refused() {
        let (conn, node) = setup();
        let mut q = attempt(node, "lint");
        q.outcome = "maybe".into();
        assert!(upsert(&conn, &q).is_err());
        let mut q = attempt(node, "lint");
        q.source = "elsewhere".into();
        assert!(upsert(&conn, &q).is_err());
    }
}
