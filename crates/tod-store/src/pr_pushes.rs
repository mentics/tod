//! Every head a node's pull request moved to while tod watched it, and whether
//! tod's own review had cleared it (schema v102;
//! `doc/lifecycle/find-it-first.md`, "Nothing reaches the PR unreviewed").
//!
//! With the review steps on in `pr_draft`, nothing should reach the pull
//! request that tod's review has not cleared: the pull request agent commits,
//! tod reviews the local head, and only then is the agent told to push. So
//! around each agent turn the autopilot runs on a node in `pr_draft` or
//! `pr_review` (and when a conversation the user directs ends), the pull
//! request's head is read again (`tod_core::pr_pushes`). A head it has not
//! seen before is recorded here, once, with what made it acceptable:
//!
//! - [`VERDICT_BASELINE`]: the first head seen in a `pr_draft` stay (the `pr`
//!   state's work, or a node moved back), the starting point;
//! - [`VERDICT_REVIEWED`]: a head a clean review of tod's recorded;
//! - [`VERDICT_TOD`]: a head tod itself made or had its agent push in the
//!   reviewed flow (GitHub's Update branch, the push turn after a review);
//! - [`VERDICT_MECHANICAL`] (schema v106): a head tod made by running a
//!   configured fixer (a formatter, a lint autofix) and committing what it
//!   changed; no review is owed for it, and a review that covered the head
//!   before it covers it ([`PrPushRepo::only_mechanical_since`]);
//! - [`VERDICT_UNREVIEWED`]: anything else, a process failure: a push nothing
//!   reviewed, recorded with the turn it appeared around (none: outside any
//!   turn) so the `learn` phase can account for it ([`PrPushRepo::unreviewed`]).
//!
//! Append-only. Synced, so a cloud node's supervisor's records show in the
//! app; journaled (`journey_changes`), since a push is something that
//! happened to the node.

use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const TABLE: &str = "pr_pushes";

pub const VERDICT_BASELINE: &str = "baseline";
pub const VERDICT_REVIEWED: &str = "reviewed";
pub const VERDICT_TOD: &str = "pushed_by_tod";
pub const VERDICT_UNREVIEWED: &str = "unreviewed";
/// A commit tod made by running a configured fixer; exempt from review.
pub const VERDICT_MECHANICAL: &str = "mechanical";

/// Table of schema v102, self-contained.
pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS pr_pushes (
        id              BLOB PRIMARY KEY NOT NULL,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        pr              TEXT NOT NULL,
        head            TEXT NOT NULL,
        previous        TEXT,
        verdict         TEXT NOT NULL CHECK (verdict IN ('baseline','reviewed','pushed_by_tod','unreviewed','mechanical')),
        lifecycle       TEXT NOT NULL,
        conversation_id BLOB,
        protocol        TEXT,
        seen_at         INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_pr_pushes_node ON pr_pushes(node_id, pr, seen_at);
";

/// One head a pull request was seen at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrPush {
    pub id: Uuid,
    pub node_id: Uuid,
    /// The pull request, `owner/repo#number`.
    pub pr: String,
    /// The commit its head was at.
    pub head: String,
    /// The head recorded before it, when there was one.
    pub previous: Option<String>,
    /// One of the `VERDICT_*` constants.
    pub verdict: String,
    /// The node's lifecycle state when it was seen.
    pub lifecycle: String,
    /// The conversation whose turn it appeared around; `None` when it
    /// appeared outside any turn (between turns, or while nothing ran).
    pub conversation_id: Option<Uuid>,
    /// That conversation's protocol (`ProtocolKind::as_str`).
    pub protocol: Option<String>,
    /// When it was seen (ms since the epoch).
    pub seen_at: i64,
}

impl PrPush {
    pub fn is_mechanical(&self) -> bool {
        self.verdict == VERDICT_MECHANICAL
    }

    pub fn is_unreviewed(&self) -> bool {
        self.verdict == VERDICT_UNREVIEWED
    }

    /// Where it appeared, in words: "around a pr turn (conversation 1a2b3c4d)",
    /// or "outside any turn".
    pub fn around(&self) -> String {
        match (self.conversation_id, self.protocol.as_deref()) {
            (Some(id), protocol) => format!(
                "around a {} turn (conversation {})",
                protocol.unwrap_or("conversation"),
                crate::interview::short_id(id)
            ),
            (None, _) => "outside any turn".to_string(),
        }
    }
}

/// Makes `pr_pushes` accept [`VERDICT_MECHANICAL`] (schema v106): SQLite cannot
/// alter a CHECK, so an older table is renamed, remade and copied (as
/// `node_requests::ensure_kinds` does). The caller's schema pass remakes the
/// triggers, which went with the old table. Idempotent.
pub fn ensure_mechanical(conn: &Connection) -> Result<()> {
    let sql: Option<String> = conn
        .query_row("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'pr_pushes'", [], |r| r.get(0))
        .optional()?;
    let Some(sql) = sql else {
        conn.execute_batch(CREATE_TABLE)?;
        return Ok(());
    };
    if sql.contains(VERDICT_MECHANICAL) {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("DROP INDEX IF EXISTS idx_pr_pushes_node; ALTER TABLE pr_pushes RENAME TO pr_pushes_old;")?;
    tx.execute_batch(CREATE_TABLE)?;
    tx.execute_batch(&format!(
        "INSERT INTO pr_pushes ({COLUMNS}) SELECT {COLUMNS} FROM pr_pushes_old ORDER BY rowid;
         DROP TABLE pr_pushes_old;"
    ))?;
    tx.commit()?;
    Ok(())
}

pub struct PrPushRepo<'a> {
    conn: &'a Connection,
}

const COLUMNS: &str = "id, node_id, pr, head, previous, verdict, lifecycle, conversation_id, protocol, seen_at";

impl<'a> PrPushRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn record(&self, p: &PrPush) -> Result<()> {
        if ![VERDICT_BASELINE, VERDICT_REVIEWED, VERDICT_TOD, VERDICT_UNREVIEWED, VERDICT_MECHANICAL].contains(&p.verdict.as_str()) {
            anyhow::bail!("unknown pull request push verdict {:?}", p.verdict);
        }
        self.conn.execute(
            &format!("INSERT INTO pr_pushes ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"),
            params![
                uuid_to_blob(p.id),
                uuid_to_blob(p.node_id),
                p.pr,
                p.head,
                p.previous,
                p.verdict,
                p.lifecycle,
                p.conversation_id.map(uuid_to_blob),
                p.protocol,
                p.seen_at,
            ],
        )?;
        Ok(())
    }

    /// Whether every head recorded after `head` (the one a review covered)
    /// up to the latest is [`VERDICT_MECHANICAL`], and there is at least one:
    /// the pull request has moved only by tod's own fixer commits since.
    pub fn only_mechanical_since(&self, node: Uuid, pr: &str, head: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM pr_pushes WHERE node_id = ?1 AND pr = ?2 ORDER BY seen_at ASC, rowid ASC"
        ))?;
        let all = stmt
            .query_map(params![uuid_to_blob(node), pr], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        let Some(at) = all.iter().rposition(|p| p.head == head) else {
            return Ok(false);
        };
        let after = &all[at + 1..];
        Ok(!after.is_empty() && after.iter().all(PrPush::is_mechanical))
    }

    /// The latest head recorded for `pr` on `node`.
    pub fn latest(&self, node: Uuid, pr: &str) -> Result<Option<PrPush>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM pr_pushes WHERE node_id = ?1 AND pr = ?2 ORDER BY seen_at DESC, rowid DESC LIMIT 1"
                ),
                params![uuid_to_blob(node), pr],
                row,
            )
            .optional()?
            .transpose()
    }

    /// Every head recorded on `node`, oldest first.
    pub fn list_for_node(&self, node: Uuid) -> Result<Vec<PrPush>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM pr_pushes WHERE node_id = ?1 ORDER BY seen_at, rowid"))?;
        let rows = stmt.query_map(params![uuid_to_blob(node)], row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r??);
        }
        Ok(out)
    }

    /// The pushes nothing reviewed on `node`, oldest first: the process
    /// failures the `learn` phase accounts for.
    pub fn unreviewed(&self, node: Uuid) -> Result<Vec<PrPush>> {
        Ok(self.list_for_node(node)?.into_iter().filter(PrPush::is_unreviewed).collect())
    }
}

fn row(r: &Row<'_>) -> rusqlite::Result<Result<PrPush>> {
    let id: Vec<u8> = r.get(0)?;
    let node: Vec<u8> = r.get(1)?;
    let conversation: Option<Vec<u8>> = r.get(7)?;
    Ok(Ok(PrPush {
        id: blob_to_uuid_sql(&id)?,
        node_id: blob_to_uuid_sql(&node)?,
        pr: r.get(2)?,
        head: r.get(3)?,
        previous: r.get(4)?,
        verdict: r.get(5)?,
        lifecycle: r.get(6)?,
        conversation_id: conversation.as_deref().map(blob_to_uuid_sql).transpose()?,
        protocol: r.get(8)?,
        seen_at: r.get(9)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(node: Uuid, head: &str, verdict: &str, at: i64) -> PrPush {
        PrPush {
            id: Uuid::new_v4(),
            node_id: node,
            pr: "acme/app#42".into(),
            head: head.into(),
            previous: None,
            verdict: verdict.into(),
            lifecycle: "pr_draft".into(),
            conversation_id: None,
            protocol: None,
            seen_at: at,
        }
    }

    #[test]
    fn the_latest_head_and_the_unreviewed_ones_are_read_back() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute_batch(CREATE_TABLE).unwrap();
        let node = Uuid::new_v4();
        let repo = PrPushRepo::new(&conn);
        repo.record(&push(node, "aaa", VERDICT_BASELINE, 1)).unwrap();
        let mut bad = push(node, "bbb", VERDICT_UNREVIEWED, 2);
        bad.conversation_id = Some(Uuid::new_v4());
        bad.protocol = Some("fix".into());
        bad.previous = Some("aaa".into());
        repo.record(&bad).unwrap();
        repo.record(&push(node, "ccc", VERDICT_REVIEWED, 3)).unwrap();
        assert_eq!(repo.latest(node, "acme/app#42").unwrap().unwrap().head, "ccc");
        assert!(repo.latest(node, "acme/app#7").unwrap().is_none());
        let unreviewed = repo.unreviewed(node).unwrap();
        assert_eq!(unreviewed, vec![bad.clone()]);
        assert!(unreviewed[0].around().starts_with("around a fix turn"), "{}", unreviewed[0].around());
        assert!(repo.record(&push(node, "ddd", "maybe", 4)).is_err(), "an unknown verdict is refused");
    }

    #[test]
    fn only_mechanical_heads_since_a_reviewed_one_are_told_from_any_other() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute_batch(CREATE_TABLE).unwrap();
        let node = Uuid::new_v4();
        let repo = PrPushRepo::new(&conn);
        let pr = "acme/app#42";
        repo.record(&push(node, "aaa", VERDICT_REVIEWED, 1)).unwrap();
        assert!(!repo.only_mechanical_since(node, pr, "aaa").unwrap(), "nothing after it");
        repo.record(&push(node, "bbb", VERDICT_MECHANICAL, 2)).unwrap();
        repo.record(&push(node, "ccc", VERDICT_MECHANICAL, 3)).unwrap();
        assert!(repo.only_mechanical_since(node, pr, "aaa").unwrap());
        assert!(repo.latest(node, pr).unwrap().unwrap().is_mechanical());
        repo.record(&push(node, "ddd", VERDICT_UNREVIEWED, 4)).unwrap();
        assert!(!repo.only_mechanical_since(node, pr, "aaa").unwrap(), "someone else's head in between");
        assert!(!repo.only_mechanical_since(node, pr, "zzz").unwrap(), "an unknown head proves nothing");
    }

    #[test]
    fn an_older_table_is_remade_to_accept_mechanical_and_keeps_its_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute_batch(&CREATE_TABLE.replace(",'mechanical'", "")).unwrap();
        let node = Uuid::new_v4();
        let repo = PrPushRepo::new(&conn);
        repo.record(&push(node, "aaa", VERDICT_REVIEWED, 1)).unwrap();
        assert!(repo.record(&push(node, "bbb", VERDICT_MECHANICAL, 2)).is_err(), "the old CHECK refuses it");
        ensure_mechanical(&conn).unwrap();
        ensure_mechanical(&conn).unwrap();
        repo.record(&push(node, "bbb", VERDICT_MECHANICAL, 2)).unwrap();
        assert_eq!(repo.latest(node, "acme/app#42").unwrap().unwrap().head, "bbb");
        assert!(repo.only_mechanical_since(node, "acme/app#42", "aaa").unwrap(), "the old row survived");
    }
}
