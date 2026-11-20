//! Suggestions: non-blocking actions filed on a node (schema v109;
//! `doc/requests/design.md` section 9).
//!
//! Something out of scope but worth doing, the commonest being a follow-up
//! ticket, is a **suggestion**, never a decision. It is filed by an agent
//! (`tod-cli suggest ticket`), by triage converting a question, or by the app
//! itself (a check failing on main), and it waits in the task panel's
//! Suggestions section until the user accepts it (the ticket is created
//! exactly as written) or dismisses it.
//!
//! A suggestion never makes the node `You` (`tod_core::next_actor` does not
//! read this table), never stops a runner, and outlives the run: a node at
//! `done` keeps its open suggestions. Synced and journaled like the node's
//! other rows.
//!
//! Filing is idempotent: an open suggestion of the same kind with the same
//! title and body (compared ignoring case and spacing) on the node is
//! returned instead of a second one ([`file`]).
//!
//! Possible duplicates (tickets already in the tracker that look like it) are
//! found mechanically by the filer, off the UI thread, and stored with the
//! suggestion; [`Suggestion::duplicates_checked_at`] says whether the search
//! ran, and [`Suggestion::duplicates_note`] why it did not (no tracker
//! configured, the search failed).

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const TABLE: &str = "node_suggestions";

/// A follow-up ticket: title and body are the exact ticket text.
pub const KIND_FOLLOW_UP_TICKET: &str = "follow_up_ticket";
pub const KINDS: [&str; 1] = [KIND_FOLLOW_UP_TICKET];

pub const STATUS_OPEN: &str = "open";
pub const STATUS_ACCEPTED: &str = "accepted";
pub const STATUS_DISMISSED: &str = "dismissed";

pub const FILED_BY_AGENT: &str = "agent";
pub const FILED_BY_APP: &str = "app";
pub const FILED_BY_TRIAGE: &str = "triage";
pub const FILERS: [&str; 3] = [FILED_BY_AGENT, FILED_BY_APP, FILED_BY_TRIAGE];

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS node_suggestions (
        id                    BLOB PRIMARY KEY NOT NULL,
        node_id               BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        kind                  TEXT NOT NULL CHECK (kind IN ('follow_up_ticket')),
        title                 TEXT NOT NULL,
        body                  TEXT NOT NULL DEFAULT '',
        duplicates            TEXT NOT NULL DEFAULT '[]',
        duplicates_checked_at INTEGER,
        duplicates_note       TEXT NOT NULL DEFAULT '',
        status                TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'accepted', 'dismissed')),
        conversation_id       BLOB,
        filed_by              TEXT NOT NULL CHECK (filed_by IN ('agent', 'app', 'triage')),
        result_id             TEXT NOT NULL DEFAULT '',
        result_url            TEXT NOT NULL DEFAULT '',
        created_at            INTEGER NOT NULL,
        updated_at            INTEGER NOT NULL,
        resolved_at           INTEGER,
        resolved_by           TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_node_suggestions_node ON node_suggestions(node_id, status, created_at);
";

const COLUMNS: &str = "id, node_id, kind, title, body, duplicates, duplicates_checked_at, duplicates_note, status, \
    conversation_id, filed_by, result_id, result_url, created_at, updated_at, resolved_at, resolved_by";

/// A ticket already in the tracker that looks like a suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Duplicate {
    /// The tracker's id for it (`ENG-3557`).
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub url: String,
}

/// One suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    pub id: Uuid,
    pub node_id: Uuid,
    pub kind: String,
    pub title: String,
    /// The ticket's description and acceptance criteria, as it will be created.
    pub body: String,
    pub duplicates: Vec<Duplicate>,
    /// When the tracker was searched; `None` when it was not.
    pub duplicates_checked_at: Option<i64>,
    /// Why the search did not run or failed (empty when it ran).
    pub duplicates_note: String,
    pub status: String,
    /// The conversation whose agent filed it.
    pub conversation_id: Option<Uuid>,
    pub filed_by: String,
    /// The created ticket's id (`ENG-4001`), once accepted.
    pub result_id: String,
    pub result_url: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub resolved_at: Option<i64>,
    pub resolved_by: Option<String>,
}

impl Suggestion {
    pub fn is_open(&self) -> bool {
        self.status == STATUS_OPEN
    }
}

/// A suggestion as asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSuggestion {
    pub node_id: Uuid,
    pub kind: String,
    pub title: String,
    pub body: String,
    /// What the tracker search found; ignored unless `duplicates_checked`.
    #[serde(default)]
    pub duplicates: Vec<Duplicate>,
    /// Whether the tracker search ran (an empty `duplicates` then means none found).
    #[serde(default)]
    pub duplicates_checked: bool,
    /// Why it did not run or failed.
    #[serde(default)]
    pub duplicates_note: String,
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
    pub filed_by: String,
}

impl NewSuggestion {
    /// A follow-up ticket filed by `filed_by`, with no duplicate search yet.
    pub fn follow_up_ticket(node_id: Uuid, title: &str, body: &str, filed_by: &str) -> Self {
        Self {
            node_id,
            kind: KIND_FOLLOW_UP_TICKET.into(),
            title: title.into(),
            body: body.into(),
            duplicates: Vec::new(),
            duplicates_checked: false,
            duplicates_note: String::new(),
            conversation_id: None,
            filed_by: filed_by.into(),
        }
    }
}

/// What [`file`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filed {
    pub id: Uuid,
    /// An identical open suggestion already existed and is `id`.
    pub already_open: bool,
}

/// Compares two texts ignoring case and runs of whitespace.
fn same_text(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    norm(a) == norm(b)
}

/// Files a suggestion on a node: the one API for the app, the CLI and triage.
/// An identical open suggestion on the node is returned instead of a
/// duplicate of it. Never fails for a missing duplicate search.
pub fn file(conn: &Connection, new: &NewSuggestion) -> Result<Filed> {
    if !KINDS.contains(&new.kind.as_str()) {
        bail!("unknown suggestion kind {:?} (expected: {})", new.kind, KINDS.join(", "));
    }
    if !FILERS.contains(&new.filed_by.as_str()) {
        bail!("unknown filer {:?} (expected: {})", new.filed_by, FILERS.join(", "));
    }
    if new.title.trim().is_empty() {
        bail!("a suggestion needs a title");
    }
    if new.body.trim().is_empty() {
        bail!("a suggestion needs a body: the ticket's description and acceptance criteria");
    }
    if let Some(existing) = open_for_node(conn, new.node_id)?
        .into_iter()
        .find(|s| s.kind == new.kind && same_text(&s.title, &new.title) && same_text(&s.body, &new.body))
    {
        return Ok(Filed { id: existing.id, already_open: true });
    }
    let id = Uuid::new_v4();
    let now = now_ms();
    conn.execute(
        &format!("INSERT INTO node_suggestions ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9, ?10, '', '', ?11, ?11, NULL, NULL)"),
        params![
            uuid_to_blob(id),
            uuid_to_blob(new.node_id),
            new.kind,
            new.title.trim(),
            new.body.trim(),
            serde_json::to_string(&new.duplicates)?,
            new.duplicates_checked.then_some(now),
            new.duplicates_note,
            new.conversation_id.map(uuid_to_blob),
            new.filed_by,
            now,
        ],
    )?;
    Ok(Filed { id, already_open: false })
}

fn map_row(row: &Row) -> rusqlite::Result<Suggestion> {
    let duplicates: String = row.get(5)?;
    Ok(Suggestion {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        kind: row.get(2)?,
        title: row.get(3)?,
        body: row.get(4)?,
        duplicates: serde_json::from_str(&duplicates).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?,
        duplicates_checked_at: row.get(6)?,
        duplicates_note: row.get(7)?,
        status: row.get(8)?,
        conversation_id: row.get::<_, Option<Vec<u8>>>(9)?.map(|b| blob_to_uuid_sql(&b)).transpose()?,
        filed_by: row.get(10)?,
        result_id: row.get(11)?,
        result_url: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        resolved_at: row.get(15)?,
        resolved_by: row.get(16)?,
    })
}

pub fn get(conn: &Connection, id: Uuid) -> Result<Option<Suggestion>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM node_suggestions WHERE id = ?1"),
            params![uuid_to_blob(id)],
            map_row,
        )
        .optional()?)
}

/// Every suggestion on `node`, oldest first.
pub fn list_for_node(conn: &Connection, node: Uuid) -> Result<Vec<Suggestion>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM node_suggestions WHERE node_id = ?1 ORDER BY created_at, rowid"
    ))?;
    let rows = stmt.query_map(params![uuid_to_blob(node)], map_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `node`'s open suggestions, oldest first.
pub fn open_for_node(conn: &Connection, node: Uuid) -> Result<Vec<Suggestion>> {
    Ok(list_for_node(conn, node)?.into_iter().filter(Suggestion::is_open).collect())
}

/// Records the result of a tracker search on a suggestion filed without one.
pub fn set_duplicates(conn: &Connection, id: Uuid, duplicates: &[Duplicate], checked: bool, note: &str) -> Result<()> {
    let now = now_ms();
    let changed = conn.execute(
        "UPDATE node_suggestions SET duplicates = ?2, duplicates_checked_at = ?3, duplicates_note = ?4, updated_at = ?5
         WHERE id = ?1",
        params![uuid_to_blob(id), serde_json::to_string(duplicates)?, checked.then_some(now), note, now],
    )?;
    if changed == 0 {
        bail!("no suggestion {id}");
    }
    Ok(())
}

/// Accepts (with the created ticket) or dismisses an open suggestion.
/// A suggestion already decided is an error, never silently changed again.
pub fn resolve(conn: &Connection, id: Uuid, status: &str, result_id: &str, result_url: &str, by: &str) -> Result<()> {
    if status != STATUS_ACCEPTED && status != STATUS_DISMISSED {
        bail!("a suggestion is resolved as `accepted` or `dismissed`, not {status:?}");
    }
    if status == STATUS_ACCEPTED && result_id.trim().is_empty() {
        bail!("an accepted suggestion records the ticket created for it");
    }
    let Some(current) = get(conn, id)? else {
        bail!("no suggestion {id}");
    };
    if !current.is_open() {
        bail!("suggestion {id} is already {}", current.status);
    }
    let now = now_ms();
    conn.execute(
        "UPDATE node_suggestions SET status = ?2, result_id = ?3, result_url = ?4, resolved_at = ?5,
             resolved_by = ?6, updated_at = ?5 WHERE id = ?1",
        params![uuid_to_blob(id), status, result_id, result_url, now, by],
    )?;
    Ok(())
}

/// Runs `file` for an `InterviewCommand`: the reply names the suggestion.
pub fn file_json(conn: &Connection, new: &NewSuggestion) -> Result<serde_json::Value> {
    let filed = file(conn, new)?;
    Ok(serde_json::json!({ "id": filed.id.to_string(), "already_open": filed.already_open }))
}

#[cfg(test)]
mod tests;
