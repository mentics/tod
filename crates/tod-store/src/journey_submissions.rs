//! `journey_submissions`: the queue of bundles waiting to be (or having been)
//! sent to the journey backend (`doc/journeys/spec.md` §9.6).
//!
//! An entry is inserted when a bundle is built and queued (report-a-problem,
//! or future automatic submission), and its status moves forward as a
//! submission worker sends it and hears back. This module is plain CRUD, like
//! `crate::journey_changes`: it does not itself record anything to the
//! journey file — `tod-store` must not depend on `tod-core` (which owns the
//! journey writer), so recording the corresponding `Event::Submission` is the
//! caller's responsibility (see `tod-ui`'s `submit_report`).

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};

/// Queued, not yet sent.
pub const STATUS_QUEUED: &str = "queued";
/// Sent to the backend; waiting on acknowledgement.
pub const STATUS_SENT: &str = "sent";
/// The backend confirmed receipt.
pub const STATUS_ACKNOWLEDGED: &str = "acknowledged";
/// Given up on (e.g. too many failed attempts).
pub const STATUS_ABANDONED: &str = "abandoned";

/// Every status a submission can be in, in the order a queue listing would
/// show progress.
pub const STATUSES: [&str; 4] = [
    STATUS_QUEUED,
    STATUS_SENT,
    STATUS_ACKNOWLEDGED,
    STATUS_ABANDONED,
];

pub const CREATE_JOURNEY_SUBMISSIONS: &str = "
CREATE TABLE IF NOT EXISTS journey_submissions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    bundle_id    TEXT NOT NULL UNIQUE,
    node_id      BLOB,
    seq          INTEGER NOT NULL,
    reason       TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'queued'
                     CHECK (status IN ('queued', 'sent', 'acknowledged', 'abandoned')),
    attempts     INTEGER NOT NULL DEFAULT 0,
    first_queued TEXT NOT NULL,
    last_sent    TEXT
);
CREATE INDEX IF NOT EXISTS idx_journey_submissions_status ON journey_submissions(status, id);
";

/// One `journey_submissions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionEntry {
    pub id: i64,
    pub bundle_id: Uuid,
    /// `None` means the bundle is about the project as a whole.
    pub node_id: Option<Uuid>,
    pub seq: i64,
    pub reason: String,
    pub status: String,
    pub attempts: i64,
    pub first_queued: String,
    pub last_sent: Option<String>,
}

const COLUMNS: &str = "id, bundle_id, node_id, seq, reason, status, attempts, first_queued, last_sent";

pub struct JourneySubmissionRepo<'a> {
    conn: &'a Connection,
}

impl<'a> JourneySubmissionRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Queues a newly built bundle. `seq` is the journey seq the bundle was
    /// built through (`entry.seq` in the exporter); `reason` matches the
    /// journey event that prompted it (e.g. `"report"`).
    pub fn insert_queued(
        &self,
        bundle_id: Uuid,
        node_id: Option<Uuid>,
        seq: i64,
        reason: &str,
    ) -> Result<SubmissionEntry> {
        let now = now_ms().to_string();
        self.conn.execute(
            "INSERT INTO journey_submissions
             (bundle_id, node_id, seq, reason, status, attempts, first_queued, last_sent)
             VALUES (?1, ?2, ?3, ?4, 'queued', 0, ?5, NULL)",
            params![
                bundle_id.to_string(),
                node_id.map(uuid_to_blob),
                seq,
                reason,
                now,
            ],
        )?;
        self.get_by_bundle(bundle_id)?
            .context("submission entry vanished after insert")
    }

    /// Moves an entry to `status`. Moving to `sent` bumps `attempts` and sets
    /// `last_sent` to now; any other status leaves them alone.
    pub fn set_status(&self, bundle_id: Uuid, status: &str) -> Result<()> {
        if !STATUSES.contains(&status) {
            bail!("unknown submission status `{status}` (expected {})", STATUSES.join("|"));
        }
        let changed = if status == STATUS_SENT {
            self.conn.execute(
                "UPDATE journey_submissions
                 SET status = ?2, attempts = attempts + 1, last_sent = ?3
                 WHERE bundle_id = ?1",
                params![bundle_id.to_string(), status, now_ms().to_string()],
            )?
        } else {
            self.conn.execute(
                "UPDATE journey_submissions SET status = ?2 WHERE bundle_id = ?1",
                params![bundle_id.to_string(), status],
            )?
        };
        if changed == 0 {
            bail!("no queued submission for bundle {bundle_id}");
        }
        Ok(())
    }

    pub fn get_by_bundle(&self, bundle_id: Uuid) -> Result<Option<SubmissionEntry>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM journey_submissions WHERE bundle_id = ?1"),
                params![bundle_id.to_string()],
                map_row,
            )
            .optional()?)
    }

    /// Every entry with `status`, oldest first.
    pub fn list_by_status(&self, status: &str) -> Result<Vec<SubmissionEntry>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM journey_submissions WHERE status = ?1 ORDER BY id"
        ))?;
        let rows = stmt
            .query_map(params![status], map_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SubmissionEntry> {
    let bundle_id: String = row.get(1)?;
    let node_id: Option<Vec<u8>> = row.get(2)?;
    Ok(SubmissionEntry {
        id: row.get(0)?,
        bundle_id: Uuid::parse_str(&bundle_id).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?,
        node_id: node_id.as_deref().map(blob_to_uuid_sql).transpose()?,
        seq: row.get(3)?,
        reason: row.get(4)?,
        status: row.get(5)?,
        attempts: row.get(6)?,
        first_queued: row.get(7)?,
        last_sent: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;

    struct Fx {
        dir: std::path::PathBuf,
        conn: Connection,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn setup() -> Fx {
        let dir = std::env::temp_dir().join(format!("tod-journey-submissions-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        Fx { dir, conn }
    }

    #[test]
    fn insert_then_read_back_a_queued_entry() {
        let fx = setup();
        let repo = JourneySubmissionRepo::new(&fx.conn);
        let bundle_id = Uuid::new_v4();
        let node_id = Uuid::new_v4();
        let entry = repo.insert_queued(bundle_id, Some(node_id), 7, "report").unwrap();
        assert_eq!(entry.bundle_id, bundle_id);
        assert_eq!(entry.node_id, Some(node_id));
        assert_eq!(entry.seq, 7);
        assert_eq!(entry.reason, "report");
        assert_eq!(entry.status, STATUS_QUEUED);
        assert_eq!(entry.attempts, 0);
        assert!(entry.last_sent.is_none());

        let fetched = repo.get_by_bundle(bundle_id).unwrap().unwrap();
        assert_eq!(fetched, entry);
    }

    #[test]
    fn a_project_bundle_has_no_node_id() {
        let fx = setup();
        let repo = JourneySubmissionRepo::new(&fx.conn);
        let bundle_id = Uuid::new_v4();
        let entry = repo.insert_queued(bundle_id, None, 1, "report").unwrap();
        assert_eq!(entry.node_id, None);
    }

    #[test]
    fn set_status_to_sent_bumps_attempts_and_stamps_last_sent() {
        let fx = setup();
        let repo = JourneySubmissionRepo::new(&fx.conn);
        let bundle_id = Uuid::new_v4();
        repo.insert_queued(bundle_id, None, 1, "report").unwrap();

        repo.set_status(bundle_id, STATUS_SENT).unwrap();
        let after_first = repo.get_by_bundle(bundle_id).unwrap().unwrap();
        assert_eq!(after_first.status, STATUS_SENT);
        assert_eq!(after_first.attempts, 1);
        assert!(after_first.last_sent.is_some());

        repo.set_status(bundle_id, STATUS_SENT).unwrap();
        let after_second = repo.get_by_bundle(bundle_id).unwrap().unwrap();
        assert_eq!(after_second.attempts, 2);

        repo.set_status(bundle_id, STATUS_ACKNOWLEDGED).unwrap();
        let acked = repo.get_by_bundle(bundle_id).unwrap().unwrap();
        assert_eq!(acked.status, STATUS_ACKNOWLEDGED);
        // Acknowledging does not bump attempts or touch last_sent again.
        assert_eq!(acked.attempts, 2);
        assert_eq!(acked.last_sent, after_second.last_sent);
    }

    #[test]
    fn set_status_rejects_unknown_status_and_missing_bundle() {
        let fx = setup();
        let repo = JourneySubmissionRepo::new(&fx.conn);
        let bundle_id = Uuid::new_v4();
        repo.insert_queued(bundle_id, None, 1, "report").unwrap();
        assert!(repo.set_status(bundle_id, "bogus").is_err());
        assert!(repo.set_status(Uuid::new_v4(), STATUS_SENT).is_err());
    }

    #[test]
    fn list_by_status_is_oldest_first() {
        let fx = setup();
        let repo = JourneySubmissionRepo::new(&fx.conn);
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        repo.insert_queued(first, None, 1, "report").unwrap();
        repo.insert_queued(second, None, 2, "report").unwrap();
        repo.set_status(second, STATUS_SENT).unwrap();

        let queued: Vec<_> = repo
            .list_by_status(STATUS_QUEUED)
            .unwrap()
            .into_iter()
            .map(|e| e.bundle_id)
            .collect();
        assert_eq!(queued, [first]);

        let sent: Vec<_> = repo
            .list_by_status(STATUS_SENT)
            .unwrap()
            .into_iter()
            .map(|e| e.bundle_id)
            .collect();
        assert_eq!(sent, [second]);
    }
}
