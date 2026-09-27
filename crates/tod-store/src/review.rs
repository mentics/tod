//! Code review findings: what a review conversation's agent found in a node's
//! change, and the response each one gets.
//!
//! A finding belongs to its node, not to the conversation that recorded it:
//! the `review` → `approved` gate asks that every finding on the node has a
//! response, whichever review found it. The agent records findings through
//! `tod-cli review`; the user (or an agent addressing them) responds with a
//! status and a note. Spec: `doc/conversation/protocols.md` §4c.

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

/// Not yet responded to.
pub const FINDING_OPEN: &str = "open";
/// Fixed; the response points at the change.
pub const FINDING_FIXED: &str = "fixed";
/// Real, but not this node's to fix.
pub const FINDING_OUT_OF_SCOPE: &str = "out_of_scope";
/// Not critical, beyond the requirements, or not worth the cost.
pub const FINDING_DECLINED: &str = "declined";
/// Not a defect after all: the fix agent's pushback, with a note saying why.
pub const FINDING_REJECTED: &str = "rejected";

/// Every finding status, in the order a status menu lists them.
pub const FINDING_STATUSES: [&str; 5] = [
    FINDING_OPEN,
    FINDING_FIXED,
    FINDING_OUT_OF_SCOPE,
    FINDING_DECLINED,
    FINDING_REJECTED,
];

/// The statuses the user answers a finding with from its menu. `rejected` is
/// the fix agent's, and needs a note the menu cannot give; a finding already
/// rejected keeps it in its menu.
pub const USER_FINDING_STATUSES: [&str; 4] = [
    FINDING_OPEN,
    FINDING_FIXED,
    FINDING_OUT_OF_SCOPE,
    FINDING_DECLINED,
];

/// The statuses a fix conversation's agent may give a finding: it fixes it,
/// or pushes back with a note. Out of scope and declined are the user's call.
pub const FIX_AGENT_STATUSES: [&str; 2] = [FINDING_FIXED, FINDING_REJECTED];

/// How much a finding matters, most first.
pub const FINDING_SEVERITIES: [&str; 3] = ["high", "medium", "low"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFinding {
    pub id: Uuid,
    pub node_id: Uuid,
    /// The review conversation that recorded it, while that conversation
    /// exists.
    pub conversation_id: Option<Uuid>,
    /// Order of recording on the node, from 1.
    pub seq: i64,
    pub severity: String,
    /// Repo-relative path, when the finding is about one place.
    pub file: Option<String>,
    pub line: Option<i64>,
    /// The defect, in a sentence.
    pub summary: String,
    /// Why it matters: the inputs or state that go wrong, and how.
    pub detail: Option<String>,
    pub status: String,
    /// The response: a pointer to the fix, or why it is out of scope,
    /// declined, or rejected.
    pub response: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ReviewFinding {
    /// `file:line`, `file`, or nothing.
    pub fn location(&self) -> Option<String> {
        let file = self.file.as_deref()?;
        Some(match self.line {
            Some(line) => format!("{file}:{line}"),
            None => file.to_string(),
        })
    }

    pub fn is_open(&self) -> bool {
        self.status == FINDING_OPEN
    }
}

/// What an agent submits.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewFinding {
    pub severity: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<i64>,
    pub summary: String,
    #[serde(default)]
    pub detail: Option<String>,
}

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS review_findings (
        id              BLOB PRIMARY KEY NOT NULL,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        conversation_id BLOB REFERENCES conversations(id) ON DELETE SET NULL,
        seq             INTEGER NOT NULL,
        severity        TEXT NOT NULL CHECK (severity IN ('high','medium','low')),
        file            TEXT,
        line            INTEGER,
        summary         TEXT NOT NULL,
        detail          TEXT,
        status          TEXT NOT NULL DEFAULT 'open'
                            CHECK (status IN ('open','fixed','out_of_scope','declined','rejected')),
        response        TEXT,
        created_at      INTEGER NOT NULL,
        updated_at      INTEGER NOT NULL,
        UNIQUE (node_id, seq)
    );
    CREATE INDEX IF NOT EXISTS idx_review_findings_node ON review_findings(node_id, seq);
";

const COLUMNS: &str = "id, node_id, conversation_id, seq, severity, file, line, summary, detail, \
     status, response, created_at, updated_at";

/// `raw` as one of `allowed`, case-insensitively.
pub fn normalize<'s>(raw: &str, allowed: &[&'s str], what: &str) -> Result<&'s str> {
    let wanted = raw.trim().to_ascii_lowercase().replace('-', "_");
    allowed
        .iter()
        .find(|s| **s == wanted)
        .copied()
        .with_context(|| format!("unknown {what} `{raw}` (expected {})", allowed.join("|")))
}

pub struct ReviewRepo<'a> {
    conn: &'a Connection,
}

impl<'a> ReviewRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Record an open finding on `node_id`.
    pub fn add(
        &self,
        node_id: Uuid,
        conversation_id: Option<Uuid>,
        finding: &NewFinding,
    ) -> Result<ReviewFinding> {
        let severity = normalize(&finding.severity, &FINDING_SEVERITIES, "severity")?;
        let summary = finding.summary.trim();
        if summary.is_empty() {
            bail!("a finding needs a summary");
        }
        let file = finding
            .file
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty());
        if finding.line.is_some() && file.is_none() {
            bail!("a line needs a file");
        }
        let detail = finding
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        let seq: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM review_findings WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
            |row| row.get(0),
        )?;
        let id = Uuid::new_v4();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO review_findings
             (id, node_id, conversation_id, seq, severity, file, line, summary, detail,
              status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'open', ?10, ?10)",
            params![
                uuid_to_blob(id),
                uuid_to_blob(node_id),
                conversation_id.map(uuid_to_blob),
                seq,
                severity,
                file,
                finding.line,
                summary,
                detail,
                now,
            ],
        )?;
        self.get(id)?.context("finding vanished after insert")
    }

    /// Set a finding's status and response; `open` clears the response.
    /// The user answers from a status menu, so a response is optional here —
    /// `tod-cli review respond` asks an agent for one. `rejected` always needs
    /// one: a pushback nobody explained is not an answer.
    pub fn respond(&self, id: Uuid, status: &str, response: Option<&str>) -> Result<()> {
        let status = normalize(status, &FINDING_STATUSES, "status")?;
        let response = response
            .map(str::trim)
            .filter(|r| !r.is_empty() && status != FINDING_OPEN);
        if status == FINDING_REJECTED && response.is_none() {
            bail!("rejecting a finding needs a note saying why it is not a problem");
        }
        let changed = self.conn.execute(
            "UPDATE review_findings SET status = ?2, response = ?3, updated_at = ?4
             WHERE id = ?1",
            params![uuid_to_blob(id), status, response, now_ms()],
        )?;
        if changed == 0 {
            bail!("finding {id} not found");
        }
        Ok(())
    }

    pub fn get(&self, id: Uuid) -> Result<Option<ReviewFinding>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM review_findings WHERE id = ?1"),
                params![uuid_to_blob(id)],
                map_row,
            )
            .optional()?)
    }

    /// The node's findings, in the order they were recorded.
    pub fn list_for_node(&self, node_id: Uuid) -> Result<Vec<ReviewFinding>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM review_findings WHERE node_id = ?1 ORDER BY seq"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Open findings across every node in `node_ids`, oldest first, in one
    /// query — for a list view over many nodes at once (e.g.
    /// `tod_core::attention`).
    pub fn list_open_for_nodes(&self, node_ids: &[Uuid]) -> Result<Vec<ReviewFinding>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = node_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT {COLUMNS} FROM review_findings
             WHERE status = 'open' AND node_id IN ({placeholders})
             ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<Vec<u8>> = node_ids.iter().copied().map(uuid_to_blob).collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), map_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// A finding from its full id or the 8-character prefix listings show.
    pub fn resolve(&self, raw: &str) -> Result<Uuid> {
        let raw = raw.trim();
        if let Ok(id) = Uuid::parse_str(raw) {
            return Ok(id);
        }
        let prefix = raw.to_ascii_lowercase();
        if prefix.len() < 4 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("`{raw}` is not a finding id");
        }
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM review_findings WHERE lower(hex(id)) LIKE ?1 || '%'")?;
        let ids = stmt
            .query_map(params![prefix], |row| {
                blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        match ids.as_slice() {
            [id] => Ok(*id),
            [] => bail!("no finding matches `{raw}`"),
            _ => bail!(
                "`{raw}` matches {} findings; give more of the id",
                ids.len()
            ),
        }
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewFinding> {
    let conversation_id: Option<Vec<u8>> = row.get(2)?;
    Ok(ReviewFinding {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        conversation_id: conversation_id
            .as_deref()
            .map(blob_to_uuid_sql)
            .transpose()?,
        seq: row.get(3)?,
        severity: row.get(4)?,
        file: row.get(5)?,
        line: row.get(6)?,
        summary: row.get(7)?,
        detail: row.get(8)?,
        status: row.get(9)?,
        response: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{ConversationRepo, Focus, ProtocolKind};
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
        let dir = std::env::temp_dir().join(format!("tod-review-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn)
            .create_with_id(node, "reviewed", "Reviewed")
            .unwrap();
        Fx { dir, conn, node }
    }

    fn finding(summary: &str) -> NewFinding {
        NewFinding {
            severity: "High".into(),
            file: Some("src/lib.rs".into()),
            line: Some(12),
            summary: summary.into(),
            detail: Some("Empty input panics.".into()),
        }
    }

    #[test]
    fn findings_are_numbered_per_node_and_listed_in_order() {
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let first = repo.add(fx.node, None, &finding("First")).unwrap();
        let second = repo.add(fx.node, None, &finding("Second")).unwrap();
        assert_eq!((first.seq, second.seq), (1, 2));
        assert_eq!(first.severity, "high");
        assert_eq!(first.status, FINDING_OPEN);
        assert_eq!(first.location().as_deref(), Some("src/lib.rs:12"));
        let listed: Vec<_> = repo
            .list_for_node(fx.node)
            .unwrap()
            .into_iter()
            .map(|f| f.summary)
            .collect();
        assert_eq!(listed, ["First", "Second"]);
        let prefix = &first.id.simple().to_string()[..8];
        assert_eq!(repo.resolve(prefix).unwrap(), first.id);
    }

    #[test]
    fn a_finding_needs_a_summary_a_known_severity_and_a_file_for_its_line() {
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let mut bad = finding(" ");
        assert!(repo.add(fx.node, None, &bad).is_err());
        bad = finding("x");
        bad.severity = "critical".into();
        assert!(repo.add(fx.node, None, &bad).is_err());
        bad = finding("x");
        bad.file = None;
        assert!(repo.add(fx.node, None, &bad).is_err());
    }

    /// A response goes with any answer but `open`; reopening clears it.
    #[test]
    fn reopening_a_finding_clears_its_response() {
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let id = repo.add(fx.node, None, &finding("x")).unwrap().id;
        assert!(repo.respond(id, "wontfix", None).is_err());
        repo.respond(id, "out-of-scope", Some("Belongs to the parser node"))
            .unwrap();
        let got = repo.get(id).unwrap().unwrap();
        assert_eq!(got.status, FINDING_OUT_OF_SCOPE);
        assert_eq!(got.response.as_deref(), Some("Belongs to the parser node"));
        repo.respond(id, "open", Some("ignored")).unwrap();
        let got = repo.get(id).unwrap().unwrap();
        assert!(got.is_open());
        assert_eq!(got.response, None);
    }

    #[test]
    fn a_rejection_needs_a_note() {
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let id = repo.add(fx.node, None, &finding("x")).unwrap().id;
        assert!(repo.respond(id, "rejected", None).is_err());
        assert!(repo.respond(id, "rejected", Some("  ")).is_err());
        repo.respond(id, "rejected", Some("Input is validated upstream"))
            .unwrap();
        let got = repo.get(id).unwrap().unwrap();
        assert_eq!(got.status, FINDING_REJECTED);
        assert!(!got.is_open());
    }

    /// Findings belong to the node: deleting the conversation that recorded
    /// them keeps them.
    #[test]
    fn findings_outlive_their_conversation() {
        let fx = setup();
        let conversation = ConversationRepo::new(&fx.conn)
            .create(Focus::Node(fx.node), ProtocolKind::Review, None, None, None)
            .unwrap()
            .id;
        let repo = ReviewRepo::new(&fx.conn);
        repo.add(fx.node, Some(conversation), &finding("x"))
            .unwrap();
        fx.conn
            .execute(
                "DELETE FROM conversations WHERE id = ?1",
                params![uuid_to_blob(conversation)],
            )
            .unwrap();
        let findings = repo.list_for_node(fx.node).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].conversation_id, None);
    }
}
