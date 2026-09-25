//! Decisions: what the user answers. A structured agent (see
//! `doc/ui/unified-view.md` "Structured agents") records a question with
//! options and evidence links through `tod-cli decisions ask`; the user (or
//! an agent addressing them on the user's behalf) answers it. An answer never
//! updates or deletes a prior one — a change of mind is a new
//! `decision_answers` row, so the log stays append-only and it is plain the
//! user changed their mind (`doc/ui/unified-view.md` "Decisions").

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

/// Not yet answered.
pub const DECISION_PENDING: &str = "pending";
/// Has at least one answer.
pub const DECISION_ANSWERED: &str = "answered";
/// Withdrawn: no longer needs an answer.
pub const DECISION_WITHDRAWN: &str = "withdrawn";

/// Every decision status.
pub const DECISION_STATUSES: [&str; 3] = [DECISION_PENDING, DECISION_ANSWERED, DECISION_WITHDRAWN];

/// The kinds of thing a decision's evidence can point at.
pub const EVIDENCE_KINDS: [&str; 6] = [
    "obligation",
    "plan_step",
    "test_run",
    "conversation",
    "finding",
    "node",
];

/// A typed pointer to whatever backs up a decision's question: an
/// obligation, a plan step, a test run, a conversation, a review finding, or
/// another node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EvidenceRef {
    pub kind: String,
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub id: Uuid,
    pub node_id: Uuid,
    /// The conversation that asked, while that conversation exists.
    pub conversation_id: Option<Uuid>,
    /// The protocol running that conversation, e.g. `implement`, `verify`.
    pub protocol: Option<String>,
    pub question: String,
    pub options: Vec<String>,
    pub evidence: Vec<EvidenceRef>,
    pub status: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionAnswer {
    pub id: i64,
    pub decision_id: Uuid,
    /// 1-based index into the decision's `options`, when the answer picked
    /// one.
    pub option: Option<i64>,
    /// Free text, when the answer was not (only) a pick.
    pub text: Option<String>,
    /// `user`, or the agent role that answered on the user's behalf.
    pub actor: String,
    pub answered_at: i64,
}

/// A decision with its full answer history, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionWithAnswers {
    pub decision: Decision,
    pub answers: Vec<DecisionAnswer>,
}

/// What an agent submits when asking.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewDecision {
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
}

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS decisions (
        id              BLOB PRIMARY KEY NOT NULL,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        conversation_id BLOB REFERENCES conversations(id) ON DELETE SET NULL,
        protocol        TEXT,
        question        TEXT NOT NULL,
        options         TEXT NOT NULL DEFAULT '[]',
        evidence        TEXT NOT NULL DEFAULT '[]',
        status          TEXT NOT NULL DEFAULT 'pending'
                            CHECK (status IN ('pending','answered','withdrawn')),
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_decisions_node ON decisions(node_id, created_at);

    CREATE TABLE IF NOT EXISTS decision_answers (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        decision_id  BLOB NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
        option_index INTEGER,
        text         TEXT,
        actor        TEXT NOT NULL,
        answered_at  INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_decision_answers_decision ON decision_answers(decision_id, id);
";

const DECISION_COLUMNS: &str =
    "id, node_id, conversation_id, protocol, question, options, evidence, status, created_at";

const ANSWER_COLUMNS: &str = "id, decision_id, option_index, text, actor, answered_at";

pub struct DecisionRepo<'a> {
    conn: &'a Connection,
}

impl<'a> DecisionRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Record a new pending decision on `node_id`.
    pub fn create(
        &self,
        node_id: Uuid,
        conversation_id: Option<Uuid>,
        protocol: Option<&str>,
        decision: &NewDecision,
    ) -> Result<Decision> {
        let question = decision.question.trim();
        if question.is_empty() {
            bail!("a decision needs a question");
        }
        let options: Vec<String> = decision
            .options
            .iter()
            .map(|o| o.trim().to_string())
            .filter(|o| !o.is_empty())
            .collect();
        for evidence in &decision.evidence {
            if !EVIDENCE_KINDS.contains(&evidence.kind.as_str()) {
                bail!(
                    "unknown evidence kind `{}` (expected {})",
                    evidence.kind,
                    EVIDENCE_KINDS.join("|")
                );
            }
        }
        let id = Uuid::new_v4();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO decisions
             (id, node_id, conversation_id, protocol, question, options, evidence, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8)",
            params![
                uuid_to_blob(id),
                uuid_to_blob(node_id),
                conversation_id.map(uuid_to_blob),
                protocol,
                question,
                serde_json::to_string(&options)?,
                serde_json::to_string(&decision.evidence)?,
                now,
            ],
        )?;
        self.get(id)?.context("decision vanished after insert")
    }

    pub fn get(&self, id: Uuid) -> Result<Option<Decision>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {DECISION_COLUMNS} FROM decisions WHERE id = ?1"),
                params![uuid_to_blob(id)],
                map_decision,
            )
            .optional()?)
    }

    /// A decision with its full answer log, oldest answer first.
    pub fn get_with_answers(&self, id: Uuid) -> Result<Option<DecisionWithAnswers>> {
        let Some(decision) = self.get(id)? else {
            return Ok(None);
        };
        let answers = self.list_answers(id)?;
        Ok(Some(DecisionWithAnswers { decision, answers }))
    }

    fn list_answers(&self, decision_id: Uuid) -> Result<Vec<DecisionAnswer>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ANSWER_COLUMNS} FROM decision_answers WHERE decision_id = ?1 ORDER BY id"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(decision_id)], map_answer)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Pending decisions on one node, oldest first.
    pub fn list_pending_for_node(&self, node_id: Uuid) -> Result<Vec<Decision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions
             WHERE node_id = ?1 AND status = 'pending' ORDER BY created_at"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_decision)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Pending decisions across every node in `node_ids`, oldest first, in
    /// one query — for a list view over many nodes at once.
    pub fn list_pending_for_nodes(&self, node_ids: &[Uuid]) -> Result<Vec<Decision>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = node_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT {DECISION_COLUMNS} FROM decisions
             WHERE status = 'pending' AND node_id IN ({placeholders})
             ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<Vec<u8>> = node_ids.iter().copied().map(uuid_to_blob).collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), map_decision)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every decision on a node, newest first — for `tod-cli decisions list --all`.
    pub fn list_for_node(&self, node_id: Uuid) -> Result<Vec<Decision>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions WHERE node_id = ?1 ORDER BY created_at DESC"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_decision)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Record an answer: an option pick, free text, or both. The decision
    /// becomes `answered` on its first answer; later answers append without
    /// touching earlier ones, per the append-only answer log.
    pub fn answer(
        &self,
        decision_id: Uuid,
        option: Option<i64>,
        text: Option<&str>,
        actor: &str,
    ) -> Result<DecisionAnswer> {
        let decision = self
            .get(decision_id)?
            .with_context(|| format!("decision {decision_id} not found"))?;
        if decision.status == DECISION_WITHDRAWN {
            bail!("decision {decision_id} was withdrawn");
        }
        let text = text.map(str::trim).filter(|t| !t.is_empty());
        if let Some(option) = option {
            if option < 1 || option > decision.options.len() as i64 {
                bail!("decision {decision_id} has no option {option}");
            }
        }
        if option.is_none() && text.is_none() {
            bail!("an answer needs an option or text");
        }
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO decision_answers (decision_id, option_index, text, actor, answered_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![uuid_to_blob(decision_id), option, text, actor, now],
        )?;
        if decision.status == DECISION_PENDING {
            self.conn.execute(
                "UPDATE decisions SET status = 'answered' WHERE id = ?1",
                params![uuid_to_blob(decision_id)],
            )?;
        }
        let id = self.conn.last_insert_rowid();
        self.conn
            .query_row(
                &format!("SELECT {ANSWER_COLUMNS} FROM decision_answers WHERE id = ?1"),
                params![id],
                map_answer,
            )
            .context("answer vanished after insert")
    }

    /// Withdraw a decision: it no longer needs an answer.
    pub fn withdraw(&self, id: Uuid) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE decisions SET status = 'withdrawn' WHERE id = ?1 AND status = 'pending'",
            params![uuid_to_blob(id)],
        )?;
        if changed == 0 {
            bail!("decision {id} not found or not pending");
        }
        Ok(())
    }

    /// A decision from its full id or the 8-character prefix listings show.
    pub fn resolve(&self, raw: &str) -> Result<Uuid> {
        let raw = raw.trim();
        if let Ok(id) = Uuid::parse_str(raw) {
            return Ok(id);
        }
        let prefix = raw.to_ascii_lowercase();
        if prefix.len() < 4 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("`{raw}` is not a decision id");
        }
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM decisions WHERE lower(hex(id)) LIKE ?1 || '%'")?;
        let ids = stmt
            .query_map(params![prefix], |row| {
                blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        match ids.as_slice() {
            [id] => Ok(*id),
            [] => bail!("no decision matches `{raw}`"),
            _ => bail!(
                "`{raw}` matches {} decisions; give more of the id",
                ids.len()
            ),
        }
    }
}

fn map_decision(row: &rusqlite::Row<'_>) -> rusqlite::Result<Decision> {
    let conversation_id: Option<Vec<u8>> = row.get(2)?;
    let options: String = row.get(5)?;
    let evidence: String = row.get(6)?;
    Ok(Decision {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        conversation_id: conversation_id
            .as_deref()
            .map(blob_to_uuid_sql)
            .transpose()?,
        protocol: row.get(3)?,
        question: row.get(4)?,
        options: serde_json::from_str(&options).unwrap_or_default(),
        evidence: serde_json::from_str(&evidence).unwrap_or_default(),
        status: row.get(7)?,
        created_at: row.get(8)?,
    })
}

fn map_answer(row: &rusqlite::Row<'_>) -> rusqlite::Result<DecisionAnswer> {
    let decision_id: Vec<u8> = row.get(1)?;
    Ok(DecisionAnswer {
        id: row.get(0)?,
        decision_id: blob_to_uuid_sql(&decision_id)?,
        option: row.get(2)?,
        text: row.get(3)?,
        actor: row.get(4)?,
        answered_at: row.get(5)?,
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
        let dir = std::env::temp_dir().join(format!("tod-decisions-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn)
            .create_with_id(node, "decided", "Decided")
            .unwrap();
        Fx { dir, conn, node }
    }

    fn decision(question: &str, options: &[&str]) -> NewDecision {
        NewDecision {
            question: question.into(),
            options: options.iter().map(|s| s.to_string()).collect(),
            evidence: vec![EvidenceRef {
                kind: "obligation".into(),
                id: Uuid::new_v4(),
            }],
        }
    }

    #[test]
    fn create_defaults_to_pending_and_round_trips_options_and_evidence() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(
                fx.node,
                None,
                Some("implement"),
                &decision("Round per line or per invoice?", &["per line", "per invoice"]),
            )
            .unwrap();
        assert_eq!(d.status, DECISION_PENDING);
        assert_eq!(d.options, ["per line", "per invoice"]);
        assert_eq!(d.evidence.len(), 1);
        assert_eq!(d.evidence[0].kind, "obligation");
        assert_eq!(d.protocol.as_deref(), Some("implement"));
        let got = repo.get(d.id).unwrap().unwrap();
        assert_eq!(got, d);
    }

    #[test]
    fn a_decision_needs_a_question_and_known_evidence_kinds() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        assert!(repo.create(fx.node, None, None, &decision(" ", &[])).is_err());
        let mut bad = decision("x?", &[]);
        bad.evidence = vec![EvidenceRef {
            kind: "spaceship".into(),
            id: Uuid::new_v4(),
        }];
        assert!(repo.create(fx.node, None, None, &bad).is_err());
    }

    #[test]
    fn pending_per_node_lists_only_pending_oldest_first() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let first = repo
            .create(fx.node, None, None, &decision("First?", &["a", "b"]))
            .unwrap();
        let second = repo
            .create(fx.node, None, None, &decision("Second?", &["a", "b"]))
            .unwrap();
        repo.answer(first.id, Some(1), None, "user").unwrap();
        let pending: Vec<_> = repo
            .list_pending_for_node(fx.node)
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(pending, [second.id]);
    }

    #[test]
    fn pending_for_many_nodes_is_one_query_across_all_of_them() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let other = Uuid::new_v4();
        NodeRepo::new(&fx.conn)
            .create_with_id(other, "other", "Other")
            .unwrap();
        let a = repo
            .create(fx.node, None, None, &decision("A?", &["x"]))
            .unwrap();
        let b = repo
            .create(other, None, None, &decision("B?", &["x"]))
            .unwrap();
        let mut ids: Vec<_> = repo
            .list_pending_for_nodes(&[fx.node, other])
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect();
        ids.sort();
        let mut expected = [a.id, b.id];
        expected.sort();
        assert_eq!(ids, expected);
        assert!(repo.list_pending_for_nodes(&[]).unwrap().is_empty());
    }

    #[test]
    fn answering_sets_status_and_changing_an_answer_appends_never_updates() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &decision("Which?", &["a", "b"]))
            .unwrap();
        let first = repo.answer(d.id, Some(1), None, "user").unwrap();
        assert_eq!(repo.get(d.id).unwrap().unwrap().status, DECISION_ANSWERED);
        let second = repo
            .answer(d.id, Some(2), Some("changed my mind"), "user")
            .unwrap();
        assert_ne!(first.id, second.id);
        let with_answers = repo.get_with_answers(d.id).unwrap().unwrap();
        assert_eq!(with_answers.answers.len(), 2);
        assert_eq!(with_answers.answers[0].option, Some(1));
        assert_eq!(with_answers.answers[1].option, Some(2));
        assert_eq!(with_answers.answers[1].text.as_deref(), Some("changed my mind"));
        // Still answered, not reset.
        assert_eq!(repo.get(d.id).unwrap().unwrap().status, DECISION_ANSWERED);
    }

    #[test]
    fn an_answer_needs_an_option_in_range_or_text() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &decision("Which?", &["a", "b"]))
            .unwrap();
        assert!(repo.answer(d.id, None, None, "user").is_err());
        assert!(repo.answer(d.id, Some(3), None, "user").is_err());
        assert!(repo.answer(d.id, Some(0), None, "user").is_err());
        repo.answer(d.id, None, Some("free text is fine too"), "user")
            .unwrap();
    }

    #[test]
    fn withdrawing_stops_it_being_pending_and_refuses_a_second_withdraw() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &decision("Which?", &["a", "b"]))
            .unwrap();
        repo.withdraw(d.id).unwrap();
        assert_eq!(repo.get(d.id).unwrap().unwrap().status, DECISION_WITHDRAWN);
        assert!(repo.list_pending_for_node(fx.node).unwrap().is_empty());
        assert!(repo.withdraw(d.id).is_err());
    }

    #[test]
    fn resolve_finds_by_full_id_or_prefix() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &decision("Which?", &["a", "b"]))
            .unwrap();
        let prefix = &d.id.simple().to_string()[..8];
        assert_eq!(repo.resolve(prefix).unwrap(), d.id);
        assert_eq!(repo.resolve(&d.id.to_string()).unwrap(), d.id);
    }

    #[test]
    fn deleting_the_node_cascades_the_decision_and_its_answers() {
        let fx = setup();
        let repo = DecisionRepo::new(&fx.conn);
        let d = repo
            .create(fx.node, None, None, &decision("Which?", &["a", "b"]))
            .unwrap();
        repo.answer(d.id, Some(1), None, "user").unwrap();
        fx.conn
            .execute("DELETE FROM nodes WHERE id = ?1", params![uuid_to_blob(fx.node)])
            .unwrap();
        assert!(repo.get(d.id).unwrap().is_none());
        assert!(
            repo.list_answers(d.id).unwrap().is_empty(),
            "answers should cascade with the decision"
        );
    }

    #[test]
    fn migration_from_previous_version_adds_the_tables() {
        let dir = std::env::temp_dir().join(format!("tod-decisions-migrate-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("tod.db");
        {
            // A fresh store already at the current version; wind it back and
            // drop the tables this version adds, as if it predated them.
            let conn = schema::open_writer_connection(&db_path).unwrap();
            // v64 added them.
            conn.pragma_update(None, "user_version", 63).unwrap();
            conn.execute_batch(
                "DROP TABLE IF EXISTS decision_answers; DROP TABLE IF EXISTS decisions;",
            )
            .unwrap();
        }
        // Reopening runs the migrations again, including the one that
        // recreates decisions/decision_answers.
        let conn = schema::open_writer_connection(&db_path).unwrap();
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::CURRENT_USER_VERSION);
        let node = Uuid::new_v4();
        NodeRepo::new(&conn)
            .create_with_id(node, "post-migrate", "Post migrate")
            .unwrap();
        let repo = DecisionRepo::new(&conn);
        let d = repo
            .create(node, None, None, &decision("Works?", &["yes"]))
            .unwrap();
        assert_eq!(d.status, DECISION_PENDING);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
