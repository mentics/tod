//! Problems of a node that happen off any turn of the user's, kept where the
//! user can see them: a background job (the daemon's automatic delete of a
//! node's sandbox at `done`, the adoption of an old sandbox, a cloud record
//! that disagrees with the node's Files location) failed and nobody was
//! watching.
//!
//! These used to go through `session_log::report_problem`, a per-process list
//! that only a conversation driver drains, only into an existing
//! conversation: in the daemon, where most of them happen, the only trace was
//! a log line. A row here is durable (it survives a restart), is read by
//! every view of the node (the task panel shows it; `tod_core::next_actor`
//! counts the one kind that matters after `done`), and is cleared by whoever
//! next succeeds at the same thing.
//!
//! One row per (node, kind), so the same job failing again updates its row
//! (its message, whether it is transient) instead of piling up. Writing the same
//! message again changes nothing, so a check that runs after every sync does
//! not wake the views each time. Machine-local, not synced (it is this
//! machine's job that failed, and no orchestrator reads it), journaled.

use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, params};
use uuid::Uuid;

/// The daemon's delete of a `done` node's sandbox (`tod_core::sandbox_retire`).
pub const KIND_SANDBOX_RETIRE: &str = "sandbox_retire";
/// The automatic adoption of a node's old sandbox (`tod_core::cloud_sync::adopt`).
pub const KIND_ADOPT: &str = "adopt";
/// A cloud record that disagrees with the node's Files location
/// (`tod_core::cloud_sync::lost::located_rows`).
pub const KIND_CLOUD_RECORD: &str = "cloud_record";

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS node_problems (
        node_id    BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        kind       TEXT NOT NULL,
        message    TEXT NOT NULL,
        transient  INTEGER NOT NULL DEFAULT 0,
        first_at   INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (node_id, kind)
    );
";

/// Adds `transient` to a table made before it existed (idempotent, no schema
/// version). Such a table also keeps its old, now unused `attempts`,
/// `max_attempts`, `next_retry_at` and `gave_up` columns (they all have
/// defaults, so a write that does not name them works): dropping them would
/// need a table rebuild for no gain, since the table is machine-local and a
/// fresh one never has them.
pub fn ensure_columns(conn: &Connection) -> Result<()> {
    let has = conn.prepare("SELECT 1 FROM pragma_table_info('node_problems') WHERE name = 'transient'")?.exists([])?;
    if !has {
        conn.execute_batch("ALTER TABLE node_problems ADD COLUMN transient INTEGER NOT NULL DEFAULT 0")?;
    }
    Ok(())
}

/// One problem of a node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeProblem {
    pub node_id: Uuid,
    /// One of the `KIND_*` constants.
    pub kind: String,
    /// What failed and why, for the user; the row's text is selectable in the UI.
    pub message: String,
    /// The failure looks temporary and the daemon's own next pass tries the
    /// job again (shown as "will retry automatically"). False: nothing is
    /// retried until something changes, and it needs the user.
    pub transient: bool,
    /// When it first failed (ms); kept while the row is updated.
    pub first_at: i64,
    /// When it last failed (ms).
    pub updated_at: i64,
}

impl NodeProblem {
    /// A problem that needs the user (nothing retries it), failing now.
    pub fn new(node_id: Uuid, kind: &str, message: String, now_ms: i64) -> Self {
        Self { node_id, kind: kind.to_string(), message, transient: false, first_at: now_ms, updated_at: now_ms }
    }
}

pub struct NodeProblemRepo<'a> {
    conn: &'a Connection,
}

impl<'a> NodeProblemRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Records `problem`, replacing what the row says now but keeping when it
    /// first failed. A row that already says all of this is left untouched
    /// (no write, so no journey row and no store change for the views).
    pub fn record(&self, problem: &NodeProblem) -> Result<()> {
        self.conn.execute(
            "INSERT INTO node_problems (node_id, kind, message, transient, first_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(node_id, kind) DO UPDATE SET
                 message = excluded.message, transient = excluded.transient, updated_at = excluded.updated_at
             WHERE message IS NOT excluded.message OR transient IS NOT excluded.transient",
            params![
                uuid_to_blob(problem.node_id),
                problem.kind,
                problem.message,
                problem.transient,
                problem.first_at,
                problem.updated_at,
            ],
        )?;
        Ok(())
    }

    /// The job succeeded (or no longer applies): drop its problem.
    pub fn clear(&self, node_id: Uuid, kind: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM node_problems WHERE node_id = ?1 AND kind = ?2",
            params![uuid_to_blob(node_id), kind],
        )?;
        Ok(())
    }

    /// The problems of the nodes in `node_ids`, in one query.
    pub fn list_for_nodes(&self, node_ids: &[Uuid]) -> Result<Vec<NodeProblem>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = node_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let params: Vec<Vec<u8>> = node_ids.iter().copied().map(uuid_to_blob).collect();
        self.query(&format!("WHERE node_id IN ({placeholders})"), rusqlite::params_from_iter(params.iter()))
    }

    /// Every problem.
    pub fn list_all(&self) -> Result<Vec<NodeProblem>> {
        self.query("", [])
    }

    fn query(&self, filter: &str, params: impl rusqlite::Params) -> Result<Vec<NodeProblem>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT node_id, kind, message, transient, first_at, updated_at
             FROM node_problems {filter} ORDER BY first_at, kind"
        ))?;
        let rows = stmt.query_map(params, |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind, message, transient, first_at, updated_at) = row?;
            out.push(NodeProblem { node_id: blob_to_uuid_sql(&id)?, kind, message, transient, first_at, updated_at });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;

    fn setup() -> (std::path::PathBuf, Connection, Uuid) {
        let dir = std::env::temp_dir().join(format!("tod-node-problems-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn).create_with_id(node, "n", "n").unwrap();
        (dir, conn, node)
    }

    #[test]
    fn recording_again_updates_the_row_and_keeps_when_it_first_failed() {
        let (dir, conn, node) = setup();
        let repo = NodeProblemRepo::new(&conn);
        let mut p = NodeProblem::new(node, KIND_SANDBOX_RETIRE, "first".into(), 1_000);
        p.transient = true;
        repo.record(&p).unwrap();
        let again =
            NodeProblem { message: "second".into(), transient: false, updated_at: 62_000, first_at: 62_000, ..p.clone() };
        repo.record(&again).unwrap();
        let rows = repo.list_for_nodes(&[node]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message, "second");
        assert!(!rows[0].transient);
        assert_eq!((rows[0].first_at, rows[0].updated_at), (1_000, 62_000));
        // Another kind is its own row.
        repo.record(&NodeProblem::new(node, KIND_ADOPT, "x".into(), 5)).unwrap();
        assert_eq!(repo.list_all().unwrap().len(), 2);
        repo.clear(node, KIND_SANDBOX_RETIRE).unwrap();
        let left = repo.list_all().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].kind, KIND_ADOPT);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_same_report_again_writes_nothing() {
        let (dir, conn, node) = setup();
        let repo = NodeProblemRepo::new(&conn);
        repo.record(&NodeProblem::new(node, KIND_CLOUD_RECORD, "same".into(), 1)).unwrap();
        let count = |c: &Connection| -> i64 {
            c.query_row("SELECT count(*) FROM journey_changes WHERE tbl = 'node_problems'", [], |r| r.get(0)).unwrap()
        };
        let before = count(&conn);
        assert!(before > 0, "a new row is journaled");
        repo.record(&NodeProblem::new(node, KIND_CLOUD_RECORD, "same".into(), 99)).unwrap();
        assert_eq!(before, count(&conn), "no journey row for an unchanged report");
        assert_eq!(repo.list_all().unwrap()[0].updated_at, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_deleted_node_takes_its_problems_with_it() {
        let (dir, conn, node) = setup();
        let repo = NodeProblemRepo::new(&conn);
        repo.record(&NodeProblem::new(node, KIND_ADOPT, "x".into(), 1)).unwrap();
        conn.execute("DELETE FROM nodes WHERE id = ?1", [uuid_to_blob(node)]).unwrap();
        assert!(repo.list_all().unwrap().is_empty());
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(version, schema::CURRENT_USER_VERSION);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_table_from_before_transient_gets_it_and_still_works() {
        let (dir, conn, node) = setup();
        conn.execute_batch(
            "DROP TABLE node_problems;
             CREATE TABLE node_problems (
                node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE, kind TEXT NOT NULL,
                message TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 1, max_attempts INTEGER,
                next_retry_at INTEGER, gave_up INTEGER NOT NULL DEFAULT 0,
                first_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY (node_id, kind));",
        )
        .unwrap();
        ensure_columns(&conn).unwrap();
        ensure_columns(&conn).unwrap();
        let repo = NodeProblemRepo::new(&conn);
        let mut p = NodeProblem::new(node, KIND_SANDBOX_RETIRE, "x".into(), 1);
        p.transient = true;
        repo.record(&p).unwrap();
        assert_eq!(repo.list_all().unwrap(), vec![p]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
