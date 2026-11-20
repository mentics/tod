//! Improvement lessons (schema v112; `doc/requests/design.md` 11.1): what went
//! wrong or took longer than it should have, and what would have prevented
//! it, recorded where it was learned so a later process review can group them
//! by target and cause. Synced and journaled. Step 7 writes the triage ones
//! ([`record_triage_lesson`]); the send-back, incident and ladder writers, the
//! user's highlights, the `tod-cli lessons` noun and the process review are
//! step 8's.
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
        updated_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_improvement_lessons_node ON improvement_lessons(node_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_improvement_lessons_decision ON improvement_lessons(decision_id);
    CREATE INDEX IF NOT EXISTS idx_improvement_lessons_status ON improvement_lessons(status, created_at);
";

/// One recorded lesson.
#[derive(Debug, Clone, PartialEq, Eq)]
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
     target, summary, user_quote, status, applied_ref, created_at, updated_at";

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
    })
}

/// Record a lesson. Every writer goes through here.
pub fn record(conn: &Connection, new: &NewLesson) -> Result<Lesson> {
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
    let id = Uuid::now_v7();
    let now = now_ms();
    conn.execute(
        "INSERT INTO improvement_lessons
             (id, node_id, source, decision_id, conversation_id, send_back_id, incident_id, category,
              target, summary, user_quote, status, applied_ref, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'new', '', ?12, ?12)",
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

/// A triage reflection: the one lesson triage records when a request it read
/// closes (`doc/requests/design.md` 8.7). The only writer of a triage lesson,
/// so step 8 (which routes lessons to targets and the process review) has one
/// place to extend. The request's closing is what it learns from: its
/// `category`, what `summary` says went wrong or right, and the user's own
/// `user_quote` when they said something about it.
pub fn record_triage_lesson(
    conn: &Connection,
    decision: Uuid,
    conversation: Uuid,
    category: &str,
    summary: &str,
    user_quote: &str,
) -> Result<Lesson> {
    // step 8: set `target` from the category and the request's reason (a
    // context fragment, the brief guidance, a role doc), and group.
    if !TRIAGE_CATEGORIES.contains(&category) {
        bail!("unknown category `{category}` (expected {})", TRIAGE_CATEGORIES.join("|"));
    }
    let Some(found) = crate::decisions::DecisionRepo::new(conn).get(decision)? else {
        bail!("decision {decision} not found");
    };
    if found.triage_conversation_id != Some(conversation) {
        bail!("decision {decision} is not the request this triage session read");
    }
    if crate::decisions::is_open(&found.status) {
        bail!("the request is still open: reflect once it is answered");
    }
    if !for_decision(conn, decision)?.is_empty() {
        bail!("a lesson was already recorded for this request");
    }
    record(
        conn,
        &NewLesson {
            node_id: found.node_id,
            source: SOURCE_TRIAGE.to_string(),
            decision_id: Some(decision),
            conversation_id: Some(conversation),
            category: category.to_string(),
            summary: summary.to_string(),
            user_quote: user_quote.to_string(),
            ..Default::default()
        },
    )
}
