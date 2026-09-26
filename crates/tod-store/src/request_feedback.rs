//! Request feedback: the user telling the app that a request for their
//! attention (a decision, a plan step handoff, a review finding, or a gate
//! blocker) should not have been asked at all — as opposed to answering it,
//! which is the normal path. `doc/ui/task-panel.md` "Shouldn't have asked":
//! a verdict of `should_not_ask` means the agent had what it needed and
//! should have decided on its own; `bad_question` means the request itself
//! was malformed or unclear. Either can carry a free-text note. This is
//! feedback on the *asking*, not an answer to the question, so it is its own
//! table rather than living on `decisions` or the plan step.
//!
//! The request kinds mirror `tod_core::attention::AttentionKind`, which this
//! crate cannot depend on (policy depends on transport, never the reverse).

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

/// The kinds of request feedback can be given on, mirroring
/// `tod_core::attention::AttentionKind`.
pub const KIND_DECISION: &str = "decision";
pub const KIND_PLAN_STEP_HANDOFF: &str = "plan_step_handoff";
pub const KIND_REVIEW_FINDING: &str = "review_finding";
pub const KIND_GATE_BLOCKER: &str = "gate_blocker";

/// Every request kind.
pub const REQUEST_KINDS: [&str; 4] = [
    KIND_DECISION,
    KIND_PLAN_STEP_HANDOFF,
    KIND_REVIEW_FINDING,
    KIND_GATE_BLOCKER,
];

/// The request had everything it needed; the agent should have decided
/// itself rather than asking.
pub const VERDICT_SHOULD_NOT_ASK: &str = "should_not_ask";
/// The request itself was malformed, unclear, or otherwise a bad question.
pub const VERDICT_BAD_QUESTION: &str = "bad_question";

/// Every verdict.
pub const VERDICTS: [&str; 2] = [VERDICT_SHOULD_NOT_ASK, VERDICT_BAD_QUESTION];

/// One piece of feedback on a request for the user's attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestFeedback {
    pub id: Uuid,
    pub node_id: Uuid,
    /// One of [`REQUEST_KINDS`].
    pub request_kind: String,
    /// The id of the underlying decision, plan step, finding, or gate-check
    /// conversation — matches `tod_core::attention::AttentionItem::id`.
    pub request_id: Uuid,
    /// Why the agent asked, from the same vocabulary as
    /// `tod_store::decisions::DECISION_REASONS`.
    pub reason: String,
    pub conversation_id: Option<Uuid>,
    pub protocol: Option<String>,
    /// One of [`VERDICTS`].
    pub verdict: String,
    pub note: Option<String>,
    pub created_at: i64,
}

/// What the UI submits to record feedback on a request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewRequestFeedback {
    pub node_id: Uuid,
    pub request_kind: String,
    pub request_id: Uuid,
    pub reason: String,
    pub conversation_id: Option<Uuid>,
    pub protocol: Option<String>,
    pub verdict: String,
    pub note: Option<String>,
}

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS request_feedback (
        id              BLOB PRIMARY KEY NOT NULL,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        request_kind    TEXT NOT NULL,
        request_id      BLOB NOT NULL,
        reason          TEXT NOT NULL,
        conversation_id BLOB REFERENCES conversations(id) ON DELETE SET NULL,
        protocol        TEXT,
        verdict         TEXT NOT NULL CHECK (verdict IN ('should_not_ask','bad_question')),
        note            TEXT,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_request_feedback_node ON request_feedback(node_id, created_at);
";

const COLUMNS: &str = "id, node_id, request_kind, request_id, reason, conversation_id, protocol, verdict, note, created_at";

pub struct RequestFeedbackRepo<'a> {
    conn: &'a Connection,
}

impl<'a> RequestFeedbackRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Record a click: the user's first signal (should-not-ask or
    /// bad-question) about a request, before any note is added.
    pub fn record(&self, feedback: &NewRequestFeedback) -> Result<Uuid> {
        if !REQUEST_KINDS.contains(&feedback.request_kind.as_str()) {
            bail!(
                "unknown request kind `{}` (expected {})",
                feedback.request_kind,
                REQUEST_KINDS.join("|")
            );
        }
        if !VERDICTS.contains(&feedback.verdict.as_str()) {
            bail!(
                "unknown verdict `{}` (expected {})",
                feedback.verdict,
                VERDICTS.join("|")
            );
        }
        let reason = feedback.reason.trim();
        if reason.is_empty() {
            bail!("request feedback needs a reason");
        }
        let note = feedback
            .note
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty());
        let id = Uuid::new_v4();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO request_feedback
             (id, node_id, request_kind, request_id, reason, conversation_id, protocol, verdict, note, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                uuid_to_blob(id),
                uuid_to_blob(feedback.node_id),
                feedback.request_kind,
                uuid_to_blob(feedback.request_id),
                reason,
                feedback.conversation_id.map(uuid_to_blob),
                feedback.protocol,
                feedback.verdict,
                note,
                now,
            ],
        )?;
        Ok(id)
    }

    /// Update a feedback row's verdict and note — the UI records one click
    /// first, then lets the user add a note or switch to "bad question".
    pub fn update(&self, id: Uuid, verdict: &str, note: Option<&str>) -> Result<()> {
        if !VERDICTS.contains(&verdict) {
            bail!("unknown verdict `{verdict}` (expected {})", VERDICTS.join("|"));
        }
        let note = note.map(str::trim).filter(|n| !n.is_empty());
        let changed = self.conn.execute(
            "UPDATE request_feedback SET verdict = ?1, note = ?2 WHERE id = ?3",
            params![verdict, note, uuid_to_blob(id)],
        )?;
        if changed == 0 {
            bail!("request feedback {id} not found");
        }
        Ok(())
    }

    pub fn get(&self, id: Uuid) -> Result<Option<RequestFeedback>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM request_feedback WHERE id = ?1"),
                params![uuid_to_blob(id)],
                map_row,
            )
            .optional()?)
    }

    /// All feedback on one node, oldest first.
    pub fn for_node(&self, node_id: Uuid) -> Result<Vec<RequestFeedback>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM request_feedback WHERE node_id = ?1 ORDER BY created_at"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RequestFeedback> {
    let conversation_id: Option<Vec<u8>> = row.get(5)?;
    Ok(RequestFeedback {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        request_kind: row.get(2)?,
        request_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(3)?)?,
        reason: row.get(4)?,
        conversation_id: conversation_id.as_deref().map(blob_to_uuid_sql).transpose()?,
        protocol: row.get(6)?,
        verdict: row.get(7)?,
        note: row.get(8)?,
        created_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;

    struct Fx {
        dir: std::path::PathBuf,
        conn: Connection,
        node: Uuid,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn setup() -> Fx {
        let dir = std::env::temp_dir().join(format!("tod-request-feedback-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn)
            .create_with_id(node, "waiting", "Waiting")
            .unwrap();
        Fx { dir, conn, node }
    }

    fn new_feedback(node: Uuid, request_id: Uuid) -> NewRequestFeedback {
        NewRequestFeedback {
            node_id: node,
            request_kind: KIND_DECISION.into(),
            request_id,
            reason: "missing_rule".into(),
            conversation_id: None,
            protocol: Some("implement".into()),
            verdict: VERDICT_SHOULD_NOT_ASK.into(),
            note: None,
        }
    }

    #[test]
    fn record_and_read_back_with_reason_and_protocol() {
        let fx = setup();
        let repo = RequestFeedbackRepo::new(&fx.conn);
        let request_id = Uuid::new_v4();
        let id = repo.record(&new_feedback(fx.node, request_id)).unwrap();
        let got = repo.get(id).unwrap().unwrap();
        assert_eq!(got.node_id, fx.node);
        assert_eq!(got.request_kind, KIND_DECISION);
        assert_eq!(got.request_id, request_id);
        assert_eq!(got.reason, "missing_rule");
        assert_eq!(got.protocol.as_deref(), Some("implement"));
        assert_eq!(got.verdict, VERDICT_SHOULD_NOT_ASK);
        assert_eq!(got.note, None);
    }

    #[test]
    fn record_rejects_unknown_kind_or_verdict_or_missing_reason() {
        let fx = setup();
        let repo = RequestFeedbackRepo::new(&fx.conn);
        let mut bad_kind = new_feedback(fx.node, Uuid::new_v4());
        bad_kind.request_kind = "spaceship".into();
        assert!(repo.record(&bad_kind).is_err());

        let mut bad_verdict = new_feedback(fx.node, Uuid::new_v4());
        bad_verdict.verdict = "spaceship".into();
        assert!(repo.record(&bad_verdict).is_err());

        let mut no_reason = new_feedback(fx.node, Uuid::new_v4());
        no_reason.reason = "  ".into();
        assert!(repo.record(&no_reason).is_err());
    }

    #[test]
    fn update_changes_verdict_and_note() {
        let fx = setup();
        let repo = RequestFeedbackRepo::new(&fx.conn);
        let id = repo
            .record(&new_feedback(fx.node, Uuid::new_v4()))
            .unwrap();
        repo.update(id, VERDICT_BAD_QUESTION, Some("Ambiguous options")).unwrap();
        let got = repo.get(id).unwrap().unwrap();
        assert_eq!(got.verdict, VERDICT_BAD_QUESTION);
        assert_eq!(got.note.as_deref(), Some("Ambiguous options"));

        assert!(repo.update(id, "spaceship", None).is_err());
        assert!(repo.update(Uuid::new_v4(), VERDICT_SHOULD_NOT_ASK, None).is_err());
    }

    #[test]
    fn for_node_orders_oldest_first_and_scopes_to_the_node() {
        let fx = setup();
        let repo = RequestFeedbackRepo::new(&fx.conn);
        let other = Uuid::new_v4();
        NodeRepo::new(&fx.conn).create_with_id(other, "other", "Other").unwrap();

        let a = repo.record(&new_feedback(fx.node, Uuid::new_v4())).unwrap();
        let b = repo.record(&new_feedback(fx.node, Uuid::new_v4())).unwrap();
        repo.record(&new_feedback(other, Uuid::new_v4())).unwrap();

        let for_node: Vec<Uuid> = repo.for_node(fx.node).unwrap().into_iter().map(|f| f.id).collect();
        assert_eq!(for_node, [a, b]);
    }

    #[test]
    fn deleting_the_node_cascades_the_feedback() {
        let fx = setup();
        let repo = RequestFeedbackRepo::new(&fx.conn);
        let id = repo
            .record(&new_feedback(fx.node, Uuid::new_v4()))
            .unwrap();
        fx.conn
            .execute("DELETE FROM nodes WHERE id = ?1", params![uuid_to_blob(fx.node)])
            .unwrap();
        assert!(repo.get(id).unwrap().is_none());
    }

    #[test]
    fn migration_from_previous_version_adds_the_table() {
        let dir = std::env::temp_dir().join(format!("tod-request-feedback-migrate-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("tod.db");
        {
            let conn = schema::open_writer_connection(&db_path).unwrap();
            // v67 added it.
            conn.pragma_update(None, "user_version", 66).unwrap();
            conn.execute_batch("DROP TABLE IF EXISTS request_feedback;")
                .unwrap();
        }
        let conn = schema::open_writer_connection(&db_path).unwrap();
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::CURRENT_USER_VERSION);
        let node = Uuid::new_v4();
        NodeRepo::new(&conn)
            .create_with_id(node, "post-migrate", "Post migrate")
            .unwrap();
        let repo = RequestFeedbackRepo::new(&conn);
        let id = repo.record(&new_feedback(node, Uuid::new_v4())).unwrap();
        assert!(repo.get(id).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
