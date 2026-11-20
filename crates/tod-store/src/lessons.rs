//! Improvement lessons (schema v112; `doc/requests/design.md` 11.1): what went
//! wrong or took longer than it should have, and what would have prevented
//! it, recorded where it was learned so a later process review can group them
//! by target and cause. Synced and journaled.
//!
//! The writers, every one mechanical (no model), each idempotent per source
//! event through the lesson's `event_key` ([`record_once`]):
//!
//! - triage ([`record_triage_lesson`]): the one reflection a closed request gets;
//! - send-back ([`record_dispute_lesson`]): every send-back and dispute, made
//!   when it is and revised when its resolution is written;
//! - ladder ([`record_quick_fix_escalation`], [`record_fixer_failure`]): a
//!   quick fix that had to escalate, a configured fixer that failed;
//! - incident ([`record_tool_incident`]): a `tool_broken` incident opening and
//!   resolving (a tod bug candidate);
//! - user ([`Op::Add`], and [`migrate_request_feedback`] for the "Shouldn't have
//!   asked" clicks of before): the user's own words.
//!
//! The process review (`tod_core::process_review`) reads them: a run groups the
//! new ones (`run_node_id`, `grouped_at`), and what it acted on is marked
//! `applied` with the commit or node that did ([`Op::Apply`]). `tod-cli lessons`
//! lists them, declines one and shows the measures ([`crate::lesson_stats`]).
//!
//! A lesson belongs to a node (an incident's, to the node that met it), so it
//! is journaled and synced with it; its other links are optional.

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

/// Where a lesson came from.
pub const SOURCE_TRIAGE: &str = "triage";
pub const SOURCE_SEND_BACK: &str = "send_back";
pub const SOURCE_INCIDENT: &str = "incident";
pub const SOURCE_LADDER: &str = "ladder";
pub const SOURCE_USER: &str = "user";
pub const SOURCES: [&str; 5] = [SOURCE_TRIAGE, SOURCE_SEND_BACK, SOURCE_INCIDENT, SOURCE_LADDER, SOURCE_USER];

/// The asker could have decided this itself.
pub const CATEGORY_ASKER_COULD_DECIDE: &str = "asker_could_decide";
/// A rule the app could apply or a check it could run settles it.
pub const CATEGORY_APP_SHOULD_HANDLE: &str = "app_should_handle";
/// An earlier phase left the point unclear.
pub const CATEGORY_UPSTREAM_UNCLEAR: &str = "upstream_unclear";
/// The request, or triage's brief, was hard to take in.
pub const CATEGORY_BRIEF_UNCLEAR: &str = "brief_unclear";
/// Only the user could answer it: asking was right.
pub const CATEGORY_NECESSARY: &str = "necessary";
/// A dispute resolved by a fix: the obligation was wrong.
pub const CATEGORY_UPSTREAM_WRONG: &str = "upstream_wrong";
pub const CATEGORIES: [&str; 6] = [
    CATEGORY_ASKER_COULD_DECIDE,
    CATEGORY_APP_SHOULD_HANDLE,
    CATEGORY_UPSTREAM_UNCLEAR,
    CATEGORY_BRIEF_UNCLEAR,
    CATEGORY_NECESSARY,
    CATEGORY_UPSTREAM_WRONG,
];
/// The categories a triage reflection may use (8.7).
pub const TRIAGE_CATEGORIES: [&str; 5] = [
    CATEGORY_ASKER_COULD_DECIDE,
    CATEGORY_APP_SHOULD_HANDLE,
    CATEGORY_UPSTREAM_UNCLEAR,
    CATEGORY_BRIEF_UNCLEAR,
    CATEGORY_NECESSARY,
];

pub const STATUS_NEW: &str = "new";
pub const STATUS_GROUPED: &str = "grouped";
pub const STATUS_APPLIED: &str = "applied";
pub const STATUS_DECLINED: &str = "declined";
pub const STATUSES: [&str; 4] = [STATUS_NEW, STATUS_GROUPED, STATUS_APPLIED, STATUS_DECLINED];

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS improvement_lessons (
        id              BLOB PRIMARY KEY NOT NULL,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        source          TEXT NOT NULL
                            CHECK (source IN ('triage','send_back','incident','ladder','user')),
        decision_id     BLOB,
        conversation_id BLOB,
        send_back_id    BLOB,
        incident_id     BLOB,
        category        TEXT NOT NULL
                            CHECK (category IN ('asker_could_decide','app_should_handle','upstream_unclear',
                                                'brief_unclear','necessary','upstream_wrong')),
        target          TEXT NOT NULL DEFAULT '',
        summary         TEXT NOT NULL,
        user_quote      TEXT NOT NULL DEFAULT '',
        status          TEXT NOT NULL DEFAULT 'new'
                            CHECK (status IN ('new','grouped','applied','declined')),
        applied_ref     TEXT NOT NULL DEFAULT '',
        created_at      INTEGER NOT NULL,
        updated_at      INTEGER NOT NULL,
        event_key       TEXT NOT NULL DEFAULT '',
        run_node_id     BLOB,
        grouped_at      INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_improvement_lessons_node ON improvement_lessons(node_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_improvement_lessons_decision ON improvement_lessons(decision_id);
    CREATE INDEX IF NOT EXISTS idx_improvement_lessons_status ON improvement_lessons(status, created_at);
";

/// Schema v113: the columns v112's table did not have (`event_key`, which
/// makes each writer idempotent per source event; the process-review run a
/// lesson was grouped into), the unique index on the key, and the rows of the
/// retired `request_feedback`. Idempotent, so it is also run on every open.
pub fn ensure_columns(conn: &Connection) -> Result<()> {
    let has = |column: &str| -> Result<bool> {
        Ok(conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('improvement_lessons') WHERE name = ?1",
            params![column],
            |row| row.get(0),
        )?)
    };
    for (column, ddl) in [("event_key", "TEXT NOT NULL DEFAULT ''"), ("run_node_id", "BLOB"), ("grouped_at", "INTEGER")] {
        if !has(column)? {
            conn.execute_batch(&format!("ALTER TABLE improvement_lessons ADD COLUMN {column} {ddl};"))?;
        }
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_improvement_lessons_event
             ON improvement_lessons(event_key) WHERE event_key <> '';
         CREATE INDEX IF NOT EXISTS idx_improvement_lessons_run ON improvement_lessons(run_node_id);",
    )?;
    Ok(())
}

/// One recorded lesson.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Lesson {
    pub id: Uuid,
    pub node_id: Uuid,
    pub source: String,
    pub decision_id: Option<Uuid>,
    pub conversation_id: Option<Uuid>,
    pub send_back_id: Option<Uuid>,
    pub incident_id: Option<Uuid>,
    pub category: String,
    pub target: String,
    pub summary: String,
    pub user_quote: String,
    pub status: String,
    pub applied_ref: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// The source event it was written for (empty for a lesson without one).
    pub event_key: String,
    /// The process-review node that took it in.
    pub run_node_id: Option<Uuid>,
    pub grouped_at: Option<i64>,
}

/// A lesson as a writer gives it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewLesson {
    pub node_id: Uuid,
    pub source: String,
    #[serde(default)]
    pub decision_id: Option<Uuid>,
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
    #[serde(default)]
    pub send_back_id: Option<Uuid>,
    #[serde(default)]
    pub incident_id: Option<Uuid>,
    pub category: String,
    #[serde(default)]
    pub target: String,
    pub summary: String,
    #[serde(default)]
    pub user_quote: String,
}

const COLUMNS: &str = "id, node_id, source, decision_id, conversation_id, send_back_id, incident_id, category, \
     target, summary, user_quote, status, applied_ref, created_at, updated_at, event_key, run_node_id, grouped_at";

fn blob_opt(row: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<Option<Uuid>> {
    row.get::<_, Option<Vec<u8>>>(i)?.map(|b| blob_to_uuid_sql(&b)).transpose()
}

fn lesson_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lesson> {
    Ok(Lesson {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        source: row.get(2)?,
        decision_id: blob_opt(row, 3)?,
        conversation_id: blob_opt(row, 4)?,
        send_back_id: blob_opt(row, 5)?,
        incident_id: blob_opt(row, 6)?,
        category: row.get(7)?,
        target: row.get(8)?,
        summary: row.get(9)?,
        user_quote: row.get(10)?,
        status: row.get(11)?,
        applied_ref: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        event_key: row.get(15)?,
        run_node_id: blob_opt(row, 16)?,
        grouped_at: row.get(17)?,
    })
}

/// Record a lesson. Every writer goes through here.
pub fn record(conn: &Connection, new: &NewLesson) -> Result<Lesson> {
    insert(conn, new, "")
}

/// Record a lesson for the source event `event_key` (`send_back:<id>`,
/// `ladder:quick_fix:<id>`, ...), once: `None` when one was already written
/// for it, so a writer that runs twice for the same event adds nothing.
pub fn record_once(conn: &Connection, new: &NewLesson, event_key: &str) -> Result<Option<Lesson>> {
    if event_key.trim().is_empty() {
        bail!("an event key is blank");
    }
    if by_event(conn, event_key)?.is_some() {
        return Ok(None);
    }
    Ok(Some(insert(conn, new, event_key)?))
}

/// The lesson written for `event_key`.
pub fn by_event(conn: &Connection, event_key: &str) -> Result<Option<Lesson>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM improvement_lessons WHERE event_key = ?1"),
            params![event_key],
            lesson_of,
        )
        .optional()?)
}

fn insert(conn: &Connection, new: &NewLesson, event_key: &str) -> Result<Lesson> {
    if !SOURCES.contains(&new.source.as_str()) {
        bail!("unknown lesson source `{}` (expected {})", new.source, SOURCES.join("|"));
    }
    if !CATEGORIES.contains(&new.category.as_str()) {
        bail!("unknown lesson category `{}` (expected {})", new.category, CATEGORIES.join("|"));
    }
    let summary = new.summary.trim();
    if summary.is_empty() {
        bail!("a lesson needs a summary: what went wrong, and what would have prevented it");
    }
    let id = Uuid::new_v4();
    let now = now_ms();
    conn.execute(
        "INSERT INTO improvement_lessons
             (id, node_id, source, decision_id, conversation_id, send_back_id, incident_id, category,
              target, summary, user_quote, status, applied_ref, created_at, updated_at, event_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'new', '', ?12, ?12, ?13)",
        params![
            uuid_to_blob(id),
            uuid_to_blob(new.node_id),
            new.source,
            new.decision_id.map(uuid_to_blob),
            new.conversation_id.map(uuid_to_blob),
            new.send_back_id.map(uuid_to_blob),
            new.incident_id.map(uuid_to_blob),
            new.category,
            new.target.trim(),
            summary,
            new.user_quote.trim(),
            now,
            event_key,
        ],
    )?;
    get(conn, id)?.ok_or_else(|| anyhow::anyhow!("lesson vanished after insert"))
}

pub fn get(conn: &Connection, id: Uuid) -> Result<Option<Lesson>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM improvement_lessons WHERE id = ?1"),
            params![uuid_to_blob(id)],
            lesson_of,
        )
        .optional()?)
}

/// The lessons recorded about one request, oldest first.
pub fn for_decision(conn: &Connection, decision: Uuid) -> Result<Vec<Lesson>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM improvement_lessons WHERE decision_id = ?1 ORDER BY created_at, rowid"
    ))?;
    let rows = stmt.query_map(params![uuid_to_blob(decision)], lesson_of)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}


mod writers;
pub use writers::*;

#[cfg(test)]
mod tests;
