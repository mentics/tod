//! The project guidance selected for a node (schema v104;
//! `doc/lifecycle/find-it-first.md` section 12): which of the project's
//! skills, always-read files, per-phase checks and tod's review lenses apply
//! to the node's change, each with a one-line reason.
//!
//! The selection is made in **runs** (`node_guidance_runs`), each against
//! what is known of the change at that point ([`STAGES`]):
//!
//! - `predicted`, once design is certified, against the areas the design
//!   names;
//! - `actual`, on entering review, against the real change: anything the
//!   design did not predict is added, anything no longer selected is dropped,
//!   and the difference is recorded on the run;
//! - `grown`, after a fix, only when the change's areas grew beyond what was
//!   selected for (adds only).
//!
//! A run is started by the app ([`start_run`]), which records the matches of
//! every skill's machine-readable triggers (`selected_by` [`BY_APP`]) and the
//! candidates the selector may choose from; the guidance selector session
//! (protocol `guidance`) then records each further match and each lens that
//! does not apply ([`select`]); the app finishes the run ([`finish_run`]) once
//! every lens is accounted for. The user may add and remove items at any time
//! ([`select`] as [`crate::interview::ACTOR_USER`], [`remove`]); a removal is
//! the user's to undo, so a later run does not select it again.
//!
//! Every later phase's opening inlines exactly the live selection
//! (`tod_core::skills_context`), under one heading per item
//! ([`inline_heading`]), so the review's `review done` check can tell an
//! inlined item from one the reviewer had to read itself
//! ([`unread_for_review`]).

use crate::conversation::{ConversationRepo, ProtocolKind};
use crate::interview::ACTOR_USER;
use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use crate::review::coverage::lens as find_lens;
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub const TABLE: &str = "node_guidance";
pub const RUNS_TABLE: &str = "node_guidance_runs";

/// A skill of the project's (`.claude/skills/.../SKILL.md`), by name.
pub const KIND_SKILL: &str = "skill";
/// A file the agent must read (`REVIEW.md`, a submodule's `CLAUDE.md`), by
/// its path relative to the node's Files directory.
pub const KIND_FILE: &str = "file";
/// One of tod's review lenses (`crate::review::LENSES`), by slug.
pub const KIND_LENS: &str = "lens";
/// A per-phase check (`crate::lifecycle_config::Check`), as `<phase>/<label>`.
pub const KIND_CHECK: &str = "check";
pub const KINDS: [&str; 4] = [KIND_SKILL, KIND_FILE, KIND_LENS, KIND_CHECK];

pub const STAGE_PREDICTED: &str = "predicted";
pub const STAGE_ACTUAL: &str = "actual";
pub const STAGE_GROWN: &str = "grown";
pub const STAGES: [&str; 3] = [STAGE_PREDICTED, STAGE_ACTUAL, STAGE_GROWN];

/// `selected_by` for a match of a skill's own triggers, made by the app.
pub const BY_APP: &str = "app";
/// `selected_by` for the user's own picks.
pub const BY_USER: &str = "user";

/// The `change_ref` of a predicted run: made against the design, not code.
pub const CHANGE_DESIGN: &str = "design";

const OUTCOME_OPEN: &str = "open";
const OUTCOME_FINISHED: &str = "finished";
const OUTCOME_ABANDONED: &str = "abandoned";

/// The error for anyone who may not select guidance.
pub const WHO_MAY: &str = "only the guidance selector session and the user may select a task's guidance \
    (`tod-cli guidance select`); every other session works from the selection it was given";

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS node_guidance (
        id               BLOB PRIMARY KEY NOT NULL,
        node_id          BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        item             TEXT NOT NULL,
        kind             TEXT NOT NULL CHECK (kind IN ('skill', 'file', 'lens', 'check')),
        applies          INTEGER NOT NULL DEFAULT 1,
        reason           TEXT NOT NULL DEFAULT '',
        stage            TEXT NOT NULL CHECK (stage IN ('predicted', 'actual', 'grown')),
        change_ref       TEXT NOT NULL DEFAULT '',
        selected_by      TEXT NOT NULL,
        run_id           BLOB,
        created_at       INTEGER NOT NULL,
        last_selected_at INTEGER NOT NULL,
        removed_at       INTEGER,
        removed_by       TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_node_guidance_node ON node_guidance(node_id, kind, item);
    CREATE TABLE IF NOT EXISTS node_guidance_runs (
        id              BLOB PRIMARY KEY NOT NULL,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        stage           TEXT NOT NULL CHECK (stage IN ('predicted', 'actual', 'grown')),
        change_ref      TEXT NOT NULL DEFAULT '',
        areas           TEXT NOT NULL DEFAULT '[]',
        candidates      TEXT NOT NULL DEFAULT '{}',
        material        TEXT NOT NULL DEFAULT '',
        conversation_id BLOB,
        outcome         TEXT NOT NULL DEFAULT 'open' CHECK (outcome IN ('open', 'finished', 'abandoned')),
        started_at      INTEGER NOT NULL,
        finished_at     INTEGER,
        added           TEXT NOT NULL DEFAULT '[]',
        dropped         TEXT NOT NULL DEFAULT '[]'
    );
    CREATE INDEX IF NOT EXISTS idx_node_guidance_runs_node ON node_guidance_runs(node_id, started_at);
";

/// One selected (or once-selected) item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guidance {
    pub id: Uuid,
    pub node_id: Uuid,
    pub item: String,
    pub kind: String,
    /// False only for a lens recorded as not applying (its reason says why).
    pub applies: bool,
    pub reason: String,
    pub stage: String,
    /// The change it was selected against: a head sha, or `design`.
    pub change_ref: String,
    /// `app`, `user`, or the selector's actor (`conversation:<id>`).
    pub selected_by: String,
    pub run_id: Option<Uuid>,
    pub created_at: i64,
    /// When a run last selected it again (equal to `created_at` at first).
    pub last_selected_at: i64,
    pub removed_at: Option<i64>,
    pub removed_by: Option<String>,
}

impl Guidance {
    pub fn is_live(&self) -> bool {
        self.removed_at.is_none()
    }

    /// Whether it is a reading item (a skill or a file) the agent applies.
    pub fn is_reading(&self) -> bool {
        self.applies && (self.kind == KIND_SKILL || self.kind == KIND_FILE)
    }

    /// Who selected it, in words.
    pub fn selected_by_label(&self) -> &'static str {
        if self.selected_by == BY_APP {
            "its triggers"
        } else if self.selected_by == BY_USER {
            "you"
        } else {
            "the selector"
        }
    }
}

/// What a run's selector may pick from, besides the lenses: built by the app
/// from the node's Files directory (`tod_core::guidance`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidates {
    /// Skill names, from the project's `.claude/skills`.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Always-read files, relative to the Files directory.
    #[serde(default)]
    pub files: Vec<String>,
    /// Per-phase checks, as `<phase>/<label>`.
    #[serde(default)]
    pub checks: Vec<String>,
}

impl Candidates {
    fn offers(&self, kind: &str, item: &str) -> bool {
        let list = match kind {
            KIND_SKILL => &self.skills,
            KIND_FILE => &self.files,
            KIND_CHECK => &self.checks,
            _ => return false,
        };
        list.iter().any(|c| c == item)
    }

    fn list(&self, kind: &str) -> &[String] {
        match kind {
            KIND_SKILL => &self.skills,
            KIND_FILE => &self.files,
            KIND_CHECK => &self.checks,
            _ => &[],
        }
    }
}

/// A match the app made by a skill's own triggers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppMatch {
    pub item: String,
    pub kind: String,
    pub reason: String,
}

/// A run as the app starts it (`InterviewCommand::StartGuidanceRun`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewRun {
    pub node_id: Uuid,
    pub stage: String,
    pub change_ref: String,
    /// The areas of the change it is made against (directories of the
    /// changed files; for a predicted run, empty).
    #[serde(default)]
    pub areas: Vec<String>,
    #[serde(default)]
    pub candidates: Candidates,
    /// What the selector's opening shows of the change and the project: the
    /// change summary, the routing table, the candidates' descriptions.
    #[serde(default)]
    pub material: String,
    /// The app's own trigger matches, recorded as the run starts.
    #[serde(default)]
    pub app_matches: Vec<AppMatch>,
}

/// One selection run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: Uuid,
    pub node_id: Uuid,
    pub stage: String,
    pub change_ref: String,
    pub areas: Vec<String>,
    pub candidates: Candidates,
    pub material: String,
    pub conversation_id: Option<Uuid>,
    pub outcome: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// `kind:item` of what this run selected that was not selected before.
    pub added: Vec<String>,
    /// `kind:item` of what this run dropped.
    pub dropped: Vec<String>,
}

impl Run {
    pub fn is_open(&self) -> bool {
        self.outcome == OUTCOME_OPEN
    }

    pub fn is_finished(&self) -> bool {
        self.outcome == OUTCOME_FINISHED
    }
}

/// A selection as asked for (`InterviewCommand::SelectGuidance`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub node_id: Uuid,
    /// The selector conversation choosing it; `None` for the user.
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
    pub kind: String,
    pub item: String,
    /// False to record a lens as not applying.
    pub applies: bool,
    pub reason: String,
}

const COLUMNS: &str = "id, node_id, item, kind, applies, reason, stage, change_ref, selected_by, run_id, \
    created_at, last_selected_at, removed_at, removed_by";

fn map_row(row: &Row<'_>) -> rusqlite::Result<Guidance> {
    let run: Option<Vec<u8>> = row.get(9)?;
    Ok(Guidance {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        item: row.get(2)?,
        kind: row.get(3)?,
        applies: row.get::<_, i64>(4)? != 0,
        reason: row.get(5)?,
        stage: row.get(6)?,
        change_ref: row.get(7)?,
        selected_by: row.get(8)?,
        run_id: run.as_deref().map(blob_to_uuid_sql).transpose()?,
        created_at: row.get(10)?,
        last_selected_at: row.get(11)?,
        removed_at: row.get(12)?,
        removed_by: row.get(13)?,
    })
}

const RUN_COLUMNS: &str = "id, node_id, stage, change_ref, areas, candidates, material, conversation_id, \
    outcome, started_at, finished_at, added, dropped";

fn map_run(row: &Row<'_>) -> rusqlite::Result<Run> {
    let json_err = |i: usize, e: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    };
    let areas: String = row.get(4)?;
    let candidates: String = row.get(5)?;
    let conversation: Option<Vec<u8>> = row.get(7)?;
    let added: String = row.get(11)?;
    let dropped: String = row.get(12)?;
    Ok(Run {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        stage: row.get(2)?,
        change_ref: row.get(3)?,
        areas: serde_json::from_str(&areas).map_err(|e| json_err(4, e))?,
        candidates: serde_json::from_str(&candidates).map_err(|e| json_err(5, e))?,
        material: row.get(6)?,
        conversation_id: conversation.as_deref().map(blob_to_uuid_sql).transpose()?,
        outcome: row.get(8)?,
        started_at: row.get(9)?,
        finished_at: row.get(10)?,
        added: serde_json::from_str(&added).map_err(|e| json_err(11, e))?,
        dropped: serde_json::from_str(&dropped).map_err(|e| json_err(12, e))?,
    })
}

/// Every item ever selected for `node`, live and removed, oldest first.
pub fn list_all(conn: &Connection, node: Uuid) -> Result<Vec<Guidance>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM node_guidance WHERE node_id = ?1 ORDER BY created_at, rowid"
    ))?;
    let rows = stmt.query_map(params![uuid_to_blob(node)], map_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The node's current selection: its live items, in a stable order (kind,
/// then item), so every opening built from it is byte-identical.
pub fn live(conn: &Connection, node: Uuid) -> Result<Vec<Guidance>> {
    let mut items: Vec<Guidance> = list_all(conn, node)?.into_iter().filter(Guidance::is_live).collect();
    items.sort_by(|a, b| kind_rank(&a.kind).cmp(&kind_rank(&b.kind)).then_with(|| a.item.cmp(&b.item)));
    Ok(items)
}

fn kind_rank(kind: &str) -> usize {
    KINDS.iter().position(|k| *k == kind).unwrap_or(KINDS.len())
}

fn live_row(conn: &Connection, node: Uuid, kind: &str, item: &str) -> Result<Option<Guidance>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM node_guidance
                 WHERE node_id = ?1 AND kind = ?2 AND item = ?3 AND removed_at IS NULL
                 ORDER BY created_at DESC, rowid DESC LIMIT 1"
            ),
            params![uuid_to_blob(node), kind, item],
            map_row,
        )
        .optional()?)
}

fn removed_by_user(conn: &Connection, node: Uuid, kind: &str, item: &str) -> Result<bool> {
    // The latest row decides: a user who removed it and later added it back
    // has a live row again.
    let latest: Option<Option<String>> = conn
        .query_row(
            "SELECT removed_by FROM node_guidance WHERE node_id = ?1 AND kind = ?2 AND item = ?3
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
            params![uuid_to_blob(node), kind, item],
            |row| row.get(0),
        )
        .optional()?;
    Ok(matches!(latest, Some(Some(by)) if by == BY_USER))
}

/// Every run of `node`, oldest first.
pub fn runs(conn: &Connection, node: Uuid) -> Result<Vec<Run>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM node_guidance_runs WHERE node_id = ?1 ORDER BY started_at, rowid"
    ))?;
    let rows = stmt.query_map(params![uuid_to_blob(node)], map_run)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_run(conn: &Connection, id: Uuid) -> Result<Option<Run>> {
    Ok(conn
        .query_row(
            &format!("SELECT {RUN_COLUMNS} FROM node_guidance_runs WHERE id = ?1"),
            params![uuid_to_blob(id)],
            map_run,
        )
        .optional()?)
}

/// The node's run still open, if any.
pub fn open_run(conn: &Connection, node: Uuid) -> Result<Option<Run>> {
    Ok(runs(conn, node)?.into_iter().rev().find(Run::is_open))
}

/// The node's finished runs started at or after `since`, oldest first.
pub fn finished_since(conn: &Connection, node: Uuid, since: i64) -> Result<Vec<Run>> {
    Ok(runs(conn, node)?
        .into_iter()
        .filter(|r| r.is_finished() && r.started_at >= since)
        .collect())
}

fn check_kind(kind: &str) -> Result<()> {
    if !KINDS.contains(&kind) {
        bail!("`{kind}` is not a kind of guidance (expected one of {})", KINDS.join(", "));
    }
    Ok(())
}

fn check_stage(stage: &str) -> Result<()> {
    if !STAGES.contains(&stage) {
        bail!("`{stage}` is not a selection stage (expected one of {})", STAGES.join(", "));
    }
    Ok(())
}

/// Normalize an item of `kind`: a lens's slug, a trimmed name or path.
fn normalize_item(kind: &str, item: &str) -> Result<String> {
    let item = item.trim();
    if item.is_empty() {
        bail!("a {kind} needs a name");
    }
    if kind == KIND_LENS {
        let Some(lens) = find_lens(item) else {
            bail!(
                "`{item}` is not one of tod's review lenses (expected one of {})",
                crate::review::LENSES.iter().map(|l| l.slug).collect::<Vec<_>>().join(", ")
            );
        };
        return Ok(lens.slug.to_string());
    }
    Ok(item.trim_start_matches("./").to_string())
}

/// The app and the autopilot only: no agent session starts or finishes a run.
fn check_app_actor(actor: &str) -> Result<()> {
    if crate::conversation::actor_conversation(actor).is_some()
        || crate::conversation::agent_actor_conversation(actor).is_some()
        || actor == crate::interview::ACTOR_AGENT
    {
        bail!("only the app starts and finishes a guidance selection");
    }
    Ok(())
}

/// Start a selection run for `new.node_id`: abandon any run left open,
/// record the app's trigger matches, and return the run. As `actor` (the
/// autopilot).
pub fn start_run(conn: &Connection, actor: &str, new: &NewRun) -> Result<Run> {
    check_app_actor(actor)?;
    check_stage(&new.stage)?;
    let node = new.node_id;
    let now = now_ms();
    conn.execute(
        "UPDATE node_guidance_runs SET outcome = ?3, finished_at = ?2 WHERE node_id = ?1 AND outcome = ?4",
        params![uuid_to_blob(node), now, OUTCOME_ABANDONED, OUTCOME_OPEN],
    )?;
    let id = Uuid::now_v7();
    conn.execute(
        &format!("INSERT INTO node_guidance_runs ({RUN_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, 'open', ?8, NULL, '[]', '[]')"),
        params![
            uuid_to_blob(id),
            uuid_to_blob(node),
            new.stage,
            new.change_ref,
            serde_json::to_string(&new.areas)?,
            serde_json::to_string(&new.candidates)?,
            new.material,
            now,
        ],
    )?;
    let run = get_run(conn, id)?.expect("the run just inserted");
    for matched in &new.app_matches {
        check_kind(&matched.kind)?;
        let item = normalize_item(&matched.kind, &matched.item)?;
        if removed_by_user(conn, node, &matched.kind, &item)? {
            continue;
        }
        put(conn, &run, &matched.kind, &item, true, &matched.reason, BY_APP, now)?;
    }
    Ok(run)
}

/// Record one item selected by `by` in `run` (or by the user, with no run):
/// a live row with the same answer is selected again (its reason refreshed,
/// its stage kept); a different answer replaces it.
#[allow(clippy::too_many_arguments)]
fn put(
    conn: &Connection,
    run: &Run,
    kind: &str,
    item: &str,
    applies: bool,
    reason: &str,
    by: &str,
    now: i64,
) -> Result<Guidance> {
    let node = run.node_id;
    if let Some(existing) = live_row(conn, node, kind, item)? {
        if existing.applies == applies {
            conn.execute(
                "UPDATE node_guidance SET reason = ?2, last_selected_at = ?3, run_id = ?4,
                     selected_by = CASE WHEN selected_by = 'user' THEN selected_by ELSE ?5 END
                 WHERE id = ?1",
                params![uuid_to_blob(existing.id), reason, now, uuid_to_blob(run.id), by],
            )?;
            return Ok(live_row(conn, node, kind, item)?.expect("the row just updated"));
        }
        conn.execute(
            "UPDATE node_guidance SET removed_at = ?2, removed_by = ?3 WHERE id = ?1",
            params![uuid_to_blob(existing.id), now, by],
        )?;
    }
    let id = Uuid::now_v7();
    conn.execute(
        &format!("INSERT INTO node_guidance ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, NULL, NULL)"),
        params![
            uuid_to_blob(id),
            uuid_to_blob(node),
            item,
            kind,
            applies as i64,
            reason,
            run.stage,
            run.change_ref,
            by,
            uuid_to_blob(run.id),
            now,
        ],
    )?;
    Ok(live_row(conn, node, kind, item)?.expect("the row just inserted"))
}

/// Record a selection as `actor`: the guidance selector session of the
/// node's open run, or the user (see [`WHO_MAY`]).
pub fn select(conn: &Connection, actor: &str, selection: &Selection) -> Result<Value> {
    check_kind(&selection.kind)?;
    let kind = selection.kind.as_str();
    let item = normalize_item(kind, &selection.item)?;
    let reason = selection.reason.trim();
    if reason.is_empty() {
        bail!("a selection needs a one-line reason: what in the change it matches, or why a lens does not apply");
    }
    if !selection.applies && kind != KIND_LENS {
        bail!("only a lens is recorded as not applying; a {kind} that does not apply is simply not selected");
    }
    let node = selection.node_id;
    let now = now_ms();
    let conversation = selection
        .conversation_id
        .or_else(|| crate::conversation::actor_conversation(actor))
        .or_else(|| crate::conversation::agent_actor_conversation(actor));
    let Some(conversation_id) = conversation else {
        if actor != ACTOR_USER {
            bail!("{WHO_MAY}");
        }
        // The user's own pick: recorded in the stage the node is at.
        let stage = runs(conn, node)?
            .into_iter()
            .rev()
            .find(Run::is_finished)
            .map_or(STAGE_PREDICTED.to_string(), |r| r.stage);
        let user_run = Run {
            id: Uuid::nil(),
            node_id: node,
            stage,
            change_ref: String::new(),
            areas: Vec::new(),
            candidates: Candidates::default(),
            material: String::new(),
            conversation_id: None,
            outcome: OUTCOME_FINISHED.to_string(),
            started_at: now,
            finished_at: Some(now),
            added: Vec::new(),
            dropped: Vec::new(),
        };
        let row = put(conn, &user_run, kind, &item, selection.applies, reason, BY_USER, now)?;
        // A user's pick belongs to no run.
        conn.execute("UPDATE node_guidance SET run_id = NULL WHERE id = ?1", params![uuid_to_blob(row.id)])?;
        return Ok(json!({ "id": row.id, "item": item, "kind": kind }));
    };
    let Some(convo) = ConversationRepo::new(conn).get(conversation_id)? else {
        bail!("conversation {conversation_id} not found: {WHO_MAY}");
    };
    if convo.protocol != ProtocolKind::Guidance {
        bail!("a {} session may not select guidance: {WHO_MAY}", convo.protocol.as_str());
    }
    if convo.focus.node_id() != Some(node) {
        bail!("the guidance selector of conversation {conversation_id} selects for its own task only");
    }
    let Some(run) = open_run(conn, node)? else {
        bail!("there is no guidance selection running for this task");
    };
    if run.conversation_id.is_some_and(|c| c != conversation_id) {
        bail!("the selection running for this task belongs to another selector session");
    }
    if kind != KIND_LENS && !run.candidates.offers(kind, &item) {
        let offered = run.candidates.list(kind);
        bail!(
            "`{item}` is not a {kind} this selection offers{}",
            if offered.is_empty() {
                format!(": it offers no {kind}")
            } else {
                format!(" (expected one of {})", offered.join(", "))
            }
        );
    }
    if removed_by_user(conn, node, kind, &item)? {
        bail!("the user removed `{item}` from this task's guidance; leave it out");
    }
    conn.execute(
        "UPDATE node_guidance_runs SET conversation_id = ?2 WHERE id = ?1 AND conversation_id IS NULL",
        params![uuid_to_blob(run.id), uuid_to_blob(conversation_id)],
    )?;
    let by = crate::conversation::actor_for(conversation_id);
    let row = put(conn, &run, kind, &item, selection.applies, reason, &by, now)?;
    Ok(json!({ "id": row.id, "item": item, "kind": kind }))
}

/// Remove an item from the node's selection: the user's alone. A removed
/// item is not selected again by a later run until the user adds it back.
pub fn remove(conn: &Connection, actor: &str, node: Uuid, kind: &str, item: &str) -> Result<Value> {
    if actor != ACTOR_USER {
        bail!("only the user removes an item from a task's guidance");
    }
    check_kind(kind)?;
    let item = normalize_item(kind, item)?;
    let Some(row) = live_row(conn, node, kind, &item)? else {
        bail!("`{item}` is not a selected {kind} on this task (`guidance list` shows the selection)");
    };
    conn.execute(
        "UPDATE node_guidance SET removed_at = ?2, removed_by = 'user' WHERE id = ?1",
        params![uuid_to_blob(row.id), now_ms()],
    )?;
    Ok(json!({ "id": row.id, "item": item, "kind": kind }))
}

/// The live items an open run would drop on finishing: in a predicted or
/// actual run, everything not selected again in this run, except the user's
/// picks. A grown run only adds.
fn would_drop(conn: &Connection, run: &Run) -> Result<Vec<Guidance>> {
    if run.stage == STAGE_GROWN {
        return Ok(Vec::new());
    }
    Ok(live(conn, run.node_id)?
        .into_iter()
        .filter(|g| g.selected_by != BY_USER && g.last_selected_at < run.started_at)
        .collect())
}

/// The lenses `run` has not accounted for: neither selected in it (as
/// applying or not), nor kept from earlier (in a grown run), nor removed by
/// the user.
pub fn missing_lenses(conn: &Connection, run: &Run) -> Result<Vec<&'static str>> {
    let dropping: Vec<Uuid> = would_drop(conn, run)?.into_iter().map(|g| g.id).collect();
    let current = live(conn, run.node_id)?;
    let mut missing = Vec::new();
    for lens in crate::review::LENSES.iter() {
        let accounted = current
            .iter()
            .any(|g| g.kind == KIND_LENS && g.item == lens.slug && !dropping.contains(&g.id))
            || removed_by_user(conn, run.node_id, KIND_LENS, lens.slug)?;
        if !accounted {
            missing.push(lens.slug);
        }
    }
    Ok(missing)
}

/// Finish the node's open run `run_id`: refused while a lens is not
/// accounted for; drops what a predicted or actual run did not select again
/// and records what it added and dropped.
pub fn finish_run(conn: &Connection, actor: &str, run_id: Uuid) -> Result<Run> {
    check_app_actor(actor)?;
    let Some(run) = get_run(conn, run_id)? else {
        bail!("guidance selection {run_id} not found");
    };
    if !run.is_open() {
        bail!("guidance selection {run_id} is already {}", run.outcome);
    }
    let missing = missing_lenses(conn, &run)?;
    if !missing.is_empty() {
        bail!(
            "the guidance selection has not recorded {} of tod's lenses: {}. Record each with \
             `guidance select --kind lens`, as applying or not, with the reason",
            missing.len(),
            missing.join(", ")
        );
    }
    let now = now_ms();
    let dropped = would_drop(conn, &run)?;
    for row in &dropped {
        conn.execute(
            "UPDATE node_guidance SET removed_at = ?2, removed_by = 'app' WHERE id = ?1",
            params![uuid_to_blob(row.id), now],
        )?;
    }
    let added: Vec<String> = list_all(conn, run.node_id)?
        .into_iter()
        .filter(|g| g.is_live() && g.run_id == Some(run.id) && g.created_at >= run.started_at)
        .filter(|g| !was_live_before(conn, g, run.started_at).unwrap_or(false))
        .map(|g| format!("{}:{}", g.kind, g.item))
        .collect();
    let dropped: Vec<String> = dropped.iter().map(|g| format!("{}:{}", g.kind, g.item)).collect();
    conn.execute(
        "UPDATE node_guidance_runs SET outcome = 'finished', finished_at = ?2, added = ?3, dropped = ?4
         WHERE id = ?1",
        params![uuid_to_blob(run.id), now, serde_json::to_string(&added)?, serde_json::to_string(&dropped)?],
    )?;
    Ok(get_run(conn, run.id)?.expect("the run just finished"))
}

/// Whether the same item (with any answer) was live just before `at`: a
/// lens whose answer changed is not "added".
fn was_live_before(conn: &Connection, row: &Guidance, at: i64) -> Result<bool> {
    Ok(conn
        .prepare(
            "SELECT 1 FROM node_guidance WHERE node_id = ?1 AND kind = ?2 AND item = ?3 AND id != ?4
             AND created_at < ?5 AND (removed_at IS NULL OR removed_at >= ?5)",
        )?
        .exists(params![uuid_to_blob(row.node_id), row.kind, row.item, uuid_to_blob(row.id), at])?)
}

/// Whether the node's selection says lens `slug` applies: `None` when no run
/// recorded it.
pub fn lens_applies(conn: &Connection, node: Uuid, slug: &str) -> Result<Option<bool>> {
    Ok(live_row(conn, node, KIND_LENS, slug)?.map(|g| g.applies))
}

/// The heading an opening puts over an inlined item, which is also how the
/// review's `review done` check knows the item was inlined.
pub fn inline_heading(kind: &str, item: &str) -> String {
    match kind {
        KIND_SKILL => format!("### Skill `{item}`"),
        _ => format!("### File `{item}`"),
    }
}

/// Whether `read` (a path a coverage entry lists under `--read`) is `item`.
fn read_matches(kind: &str, item: &str, read: &str) -> bool {
    let read = read.trim().trim_start_matches("./");
    if read == item || read.ends_with(&format!("/{item}")) {
        return true;
    }
    kind == KIND_SKILL && (read.ends_with(&format!("/{item}/SKILL.md")) || read.ends_with(&format!("/{item}")))
}

/// The selected files and skills review conversation `conversation_id`
/// neither had inlined in its opening nor lists as read in any coverage
/// entry (find-it-first 6g), as `kind:item`.
pub fn unread_for_review(conn: &Connection, conversation_id: Uuid, node: Uuid) -> Result<Vec<String>> {
    let opening = ConversationRepo::new(conn)
        .opening_context(conversation_id)?
        .unwrap_or_default();
    let reads: Vec<String> = crate::review::coverage::for_conversation(conn, conversation_id)?
        .into_iter()
        .flat_map(|e| e.read)
        .collect();
    Ok(live(conn, node)?
        .into_iter()
        .filter(Guidance::is_reading)
        .filter(|g| !opening.contains(&inline_heading(&g.kind, &g.item)))
        .filter(|g| !reads.iter().any(|r| read_matches(&g.kind, &g.item, r)))
        .map(|g| format!("{}:{}", g.kind, g.item))
        .collect())
}

#[cfg(test)]
mod tests;
