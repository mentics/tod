//! A count of the transient git failures tod retried (shepherd gap 49).
//!
//! `tod_store::fleet::workdir` retries `fetch`, `clone`, `ls-remote`, `pull`
//! and `submodule update` when git loses a ref-lock race or hits a network
//! error (`retry_network_git`, the one loop every Rust caller shares: the
//! app, the daemon and the supervisor). Each git call that needed at least
//! one retry leaves one row here: which node (when the caller was working for
//! one, `workdir::scope_retries_to_node`), where (the workdir), the operation,
//! the error class, how many retries, and whether it recovered or gave up. A
//! call that succeeds first time leaves nothing, so the cost is nil on the
//! normal path. `tod-cli stats --section git-retries` reads it; a call that
//! gave up, with a node known, also leaves a note on the node's activity.
//!
//! The shell scripts' own retry (`tod_sandbox::git_retry::FETCH_RETRY_SH`:
//! the quick branch script, the sandbox checkout, the golden refresh) runs
//! where Rust cannot wrap it and is not counted here.
//!
//! Machine-local, append-only, not synced, and not in `journey_changes`: it
//! is operational telemetry about this machine's network and repositories, not
//! a change to the node, and a trigger would only duplicate the row.

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const TABLE: &str = "git_retries";

pub const OUTCOME_RECOVERED: &str = "recovered";
pub const OUTCOME_GAVE_UP: &str = "gave_up";
pub const CLASS_LOCK_RACE: &str = "lock_race";
pub const CLASS_NETWORK: &str = "network";

/// The title of the note a call that gave up leaves on the node's activity.
pub const GAVE_UP_TITLE: &str = "Git gave up retrying";

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS git_retries (
        id      BLOB PRIMARY KEY NOT NULL,
        node_id BLOB,
        workdir TEXT NOT NULL,
        op      TEXT NOT NULL,
        class   TEXT NOT NULL,
        retries INTEGER NOT NULL,
        outcome TEXT NOT NULL,
        error   TEXT NOT NULL DEFAULT '',
        at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_git_retries_node ON git_retries(node_id, at);
";

/// One git call that was retried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRetry {
    pub id: Uuid,
    pub node_id: Option<Uuid>,
    pub workdir: String,
    /// `fetch`, `clone`, `ls-remote`, `pull` or `submodule update`.
    pub op: String,
    /// [`CLASS_LOCK_RACE`] or [`CLASS_NETWORK`] (the first failure's).
    pub class: String,
    pub retries: u32,
    /// [`OUTCOME_RECOVERED`] or [`OUTCOME_GAVE_UP`].
    pub outcome: String,
    /// git's last error, trimmed.
    pub error: String,
    pub at: i64,
}

impl GitRetry {
    pub fn new(node_id: Option<Uuid>, workdir: &str, op: &str, class: &str, retries: u32, recovered: bool, error: &str) -> Self {
        Self {
            id: Uuid::new_v4(),
            node_id,
            workdir: workdir.to_string(),
            op: op.to_string(),
            class: class.to_string(),
            retries,
            outcome: if recovered { OUTCOME_RECOVERED } else { OUTCOME_GAVE_UP }.to_string(),
            error: error.trim().chars().take(400).collect(),
            at: now_ms(),
        }
    }
}

pub struct GitRetryRepo<'a> {
    conn: &'a Connection,
}

impl<'a> GitRetryRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn record(&self, r: &GitRetry) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO git_retries (id, node_id, workdir, op, class, retries, outcome, error, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                uuid_to_blob(r.id),
                r.node_id.map(uuid_to_blob),
                r.workdir,
                r.op,
                r.class,
                r.retries,
                r.outcome,
                r.error,
                r.at
            ],
        )?;
        // A call that gave up is also a note on the node's activity (a node
        // deleted since has none to leave it on).
        if r.outcome == OUTCOME_GAVE_UP
            && let Some(node) = r.node_id
        {
            let exists: i64 =
                self.conn.query_row("SELECT COUNT(*) FROM nodes WHERE id = ?1", [uuid_to_blob(node)], |row| row.get(0))?;
            if exists > 0 {
                let lifecycle: String = self
                    .conn
                    .query_row("SELECT state FROM node_lifecycle WHERE node_id = ?1", [uuid_to_blob(node)], |row| row.get(0))
                    .unwrap_or_default();
                let detail = format!(
                    "git {} in {} failed ({}) and still failed after {} retries: {}",
                    r.op, r.workdir, r.class, r.retries, r.error
                );
                let entry = crate::node_timeline::TimelineEntry::new(node, crate::node_timeline::KIND_NOTE, &lifecycle, GAVE_UP_TITLE, true, detail);
                crate::node_timeline::TimelineRepo::new(self.conn).record(&entry)?;
            }
        }
        Ok(())
    }

    pub fn list_for_node(&self, node: Uuid) -> Result<Vec<GitRetry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, workdir, op, class, retries, outcome, error, at FROM git_retries WHERE node_id = ?1 ORDER BY at",
        )?;
        let rows = stmt.query_map([uuid_to_blob(node)], |row| {
            Ok(GitRetry {
                id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
                node_id: row.get::<_, Option<Vec<u8>>>(1)?.and_then(|b| Uuid::from_slice(&b).ok()),
                workdir: row.get(2)?,
                op: row.get(3)?,
                class: row.get(4)?,
                retries: row.get(5)?,
                outcome: row.get(6)?,
                error: row.get(7)?,
                at: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;

    #[test]
    fn a_retry_is_kept_and_one_that_gave_up_is_also_a_note_on_the_node() {
        let dir = tod_test_util::scratch_dir(format!("tod-git-retries-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn).create_with_id(node, "alpha", "alpha").unwrap();
        let repo = GitRetryRepo::new(&conn);
        let notes = |conn: &Connection| -> i64 {
            conn.query_row("SELECT COUNT(*) FROM node_timeline WHERE title = ?1", [GAVE_UP_TITLE], |r| r.get(0)).unwrap()
        };
        repo.record(&GitRetry::new(Some(node), "/r", "fetch", CLASS_LOCK_RACE, 2, true, "")).unwrap();
        assert_eq!(notes(&conn), 0, "a recovered retry is counted, not announced");
        repo.record(&GitRetry::new(Some(node), "/r", "clone", CLASS_NETWORK, 3, false, "fatal: early EOF\n")).unwrap();
        assert_eq!(notes(&conn), 1);
        // A retry for no node, and one for a node since deleted, are kept too.
        repo.record(&GitRetry::new(None, "/r2", "pull", CLASS_NETWORK, 1, true, "")).unwrap();
        repo.record(&GitRetry::new(Some(Uuid::new_v4()), "/r3", "pull", CLASS_NETWORK, 3, false, "x")).unwrap();
        assert_eq!(notes(&conn), 1);
        let rows = repo.list_for_node(node).unwrap();
        assert_eq!(
            rows.iter().map(|r| (r.op.as_str(), r.outcome.as_str(), r.retries)).collect::<Vec<_>>(),
            [("fetch", "recovered", 2), ("clone", "gave_up", 3)]
        );
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM git_retries", [], |r| r.get(0)).unwrap();
        assert_eq!(total, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_store_counts_the_retries_made_through_its_writer() {
        use std::os::unix::process::ExitStatusExt;
        let dir = tod_test_util::scratch_dir(format!("tod-git-retries-store-{}", Uuid::new_v4()));
        let store = crate::fleet::FleetStore::open_without_reload(&dir).unwrap();
        let mut calls = 0;
        crate::fleet::workdir::retry_ref_lock_race_in("/r/store-counted", &["fetch"], &[std::time::Duration::ZERO; 3], || {
            calls += 1;
            let code = if calls < 2 { 128 } else { 0 };
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: vec![],
                stderr: b"fatal: early EOF".to_vec(),
            })
        })
        .unwrap();
        store.writer().flush().unwrap();
        let n: i64 = store
            .read(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM git_retries WHERE workdir = '/r/store-counted' AND outcome = 'recovered'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(n, 1);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
