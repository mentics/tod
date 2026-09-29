//! Phase certifications: the lifecycle's record of "phase *S* of node *N* was
//! judged complete, over inputs with digest *D*" (`doc/lifecycle/phase-agents.md`).
//!
//! `phase_events` is append-only. A phase agent marks its phase `ready` for
//! an evaluator; the evaluator (or the phase agent itself, or the user)
//! `certify`s it, or `reject`s it with the fixes it needs. Every event records
//! the digest of the state's inputs at the time, computed here and nowhere
//! else ([`phase_inputs`], [`digest`]), with the canonical JSON it was taken
//! over so a stale certificate can say what changed since.
//!
//! A certificate is **current** while it was recorded in the node's current
//! stay in the state (at or after [`PhaseRepo::stay_started_at`]) and the
//! digest recomputed now still equals it. Because the digest is recomputed
//! from the rows, a change that is later reversed stops counting.

use crate::conversation::ProtocolKind;
use crate::interview::short_id;
use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The phase's work is done; have it evaluated.
pub const PHASE_READY: &str = "ready";
/// The phase is judged done over the inputs with this event's digest.
pub const PHASE_CERTIFY: &str = "certify";
/// The phase is sent back to its agent with the fixes in the body.
pub const PHASE_REJECT: &str = "reject";

/// The phase agent judged its own phase (independent evaluation off).
pub const CERTIFIER_SELF: &str = "self";
/// A separate Evaluate session judged it.
pub const CERTIFIER_INDEPENDENT: &str = "independent";
/// The user did ("Mark phase done").
pub const CERTIFIER_USER: &str = "user";

/// The states whose leaving needs a certificate.
pub const CERTIFIABLE_STATES: [&str; 5] = ["proposed", "design", "planning", "merged", "released"];

/// Whether `state` is one a certificate can be recorded for.
pub fn is_certifiable(state: &str) -> bool {
    CERTIFIABLE_STATES.contains(&state)
}

///
/// `id` is a UUIDv7: the table is synced, so two copies (the app and a cloud
/// runner) insert on their own between syncs, and an autoincrement key would
/// collide and let one side's event overwrite the other's. v7 ids also sort
/// by time, which the log is read in.
pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS phase_events (
        id              BLOB PRIMARY KEY,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        state           TEXT NOT NULL,
        kind            TEXT NOT NULL CHECK (kind IN ('ready','certify','reject')),
        digest          TEXT NOT NULL,
        snapshot        TEXT NOT NULL,
        conversation_id BLOB,
        certifier       TEXT NOT NULL CHECK (certifier IN ('self','independent','user')),
        body            TEXT NOT NULL DEFAULT '',
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_phase_events_node ON phase_events(node_id, state, created_at);
";

/// Create `phase_events`, or rebuild one whose `id` is still the integer it
/// first was (a store opened by the branch that introduced the table),
/// keeping its rows in order under new UUIDv7 ids.
pub fn ensure_table(conn: &Connection) -> Result<()> {
    let id_type: Option<String> = conn
        .query_row(
            "SELECT type FROM pragma_table_info('phase_events') WHERE name = 'id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if !id_type.is_some_and(|t| t.eq_ignore_ascii_case("INTEGER")) {
        conn.execute_batch(CREATE_TABLE)?;
        return Ok(());
    }
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS trg_journey_phase_events_insert;
         DROP INDEX IF EXISTS idx_phase_events_node;
         ALTER TABLE phase_events RENAME TO phase_events_old;",
    )?;
    conn.execute_batch(CREATE_TABLE)?;
    let old_ids: Vec<i64> = conn
        .prepare("SELECT id FROM phase_events_old ORDER BY id")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for old in old_ids {
        conn.execute(
            "INSERT INTO phase_events
             (id, node_id, state, kind, digest, snapshot, conversation_id, certifier, body, created_at)
             SELECT ?1, node_id, state, kind, digest, snapshot, conversation_id, certifier, body, created_at
             FROM phase_events_old WHERE id = ?2",
            params![uuid_to_blob(Uuid::now_v7()), old],
        )?;
    }
    conn.execute_batch("DROP TABLE phase_events_old;")?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseEvent {
    pub id: Uuid,
    pub node_id: Uuid,
    /// The lifecycle state the event is about.
    pub state: String,
    /// [`PHASE_READY`], [`PHASE_CERTIFY`], or [`PHASE_REJECT`].
    pub kind: String,
    /// [`digest`] of `snapshot`.
    pub digest: String,
    /// The state's inputs ([`phase_inputs`]) when the event was recorded.
    pub snapshot: Value,
    /// The conversation whose agent recorded it; `None` for the user.
    pub conversation_id: Option<Uuid>,
    /// [`CERTIFIER_SELF`], [`CERTIFIER_INDEPENDENT`], or [`CERTIFIER_USER`].
    pub certifier: String,
    /// `certify`: the note. `reject`: a JSON array of fixes. `ready`: empty.
    pub body: String,
    pub created_at: i64,
}

impl PhaseEvent {
    /// A rejection's fixes, in the order given; empty for any other kind.
    pub fn fixes(&self) -> Vec<String> {
        if self.kind != PHASE_REJECT {
            return Vec::new();
        }
        serde_json::from_str(&self.body).unwrap_or_default()
    }
}

/// Whether a state's phase has a certificate that still holds.
#[derive(Debug, Clone, PartialEq)]
pub enum CertificateStatus {
    /// Nothing certified in this stay.
    None,
    /// Certified, and nothing it covers has changed since.
    Current(PhaseEvent),
    /// The latest certificate in this stay no longer matches the inputs;
    /// `changed` says how, one short line per difference.
    Stale { event: PhaseEvent, changed: Vec<String> },
}

// ── Inputs and digest ───────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct InObligation {
    id: Uuid,
    kind: String,
    body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct InContent {
    id: Uuid,
    #[serde(rename = "type")]
    content_type: String,
    body: String,
}

/// An obligation's visual-design mockup: the file's path and a hash of its
/// contents (`None` when the file is missing), since saving again may
/// overwrite the same path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct InMockup {
    obligation_id: Uuid,
    path: String,
    sha256: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct InMedia {
    media_id: Uuid,
    role: String,
    sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct InStep {
    id: Uuid,
    body: String,
    /// Left out for `active`, so certificates taken before steps had phases
    /// keep their digest.
    #[serde(default = "active_phase", skip_serializing_if = "is_active_phase")]
    phase: String,
}

fn active_phase() -> String {
    crate::outline::repos::plan_steps::PHASE_ACTIVE.to_string()
}

fn is_active_phase(phase: &str) -> bool {
    phase == crate::outline::repos::plan_steps::PHASE_ACTIVE
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct InLink {
    step_id: Uuid,
    obligation_id: Uuid,
}

/// Everything a state's certificate covers. Only the fields the state lists
/// are present; the rest are left out of the JSON entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Inputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    obligations: Option<Vec<InObligation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<Vec<InContent>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mockups: Option<Vec<InMockup>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    media: Option<Vec<InMedia>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_steps: Option<Vec<InStep>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    step_obligations: Option<Vec<InLink>>,
}

/// The canonical inputs of `state`'s phase on `node_id`, or `None` when the
/// state is not certifiable. Arrays are sorted by id, so neither reordering
/// nor a status change moves the digest; a row deleted and recreated with
/// the same text does, since ids are included.
///
/// - `proposed`: the node's own obligations (id, kind, body).
/// - `design`: those, plus its content except the regenerated `summary`
///   and the data source's `metadata`, each obligation's mockup (path and
///   file hash), and any linked media (id, role, sha256). `metadata` is the
///   generator's own record of the source item (it links the node to its
///   Linear issue, and the task editor shows it); no agent is given it, so
///   no design turns on it, and a refresh that only touches it must not
///   send the phase back.
/// - `planning`: own obligations, plan steps (id, body, phase), and which step
///   satisfies which obligation.
/// - `merged`, `released`: `{}` — the certificate is a check mark for the stay.
pub fn phase_inputs(conn: &Connection, node_id: Uuid, state: &str) -> Result<Option<Value>> {
    let inputs = match state {
        "proposed" => Inputs {
            obligations: Some(obligations(conn, node_id)?),
            ..Default::default()
        },
        "design" => Inputs {
            obligations: Some(obligations(conn, node_id)?),
            content: Some(content(conn, node_id)?),
            mockups: Some(mockups(conn, node_id)?),
            media: Some(media(conn, node_id)?),
            ..Default::default()
        },
        "planning" => Inputs {
            obligations: Some(obligations(conn, node_id)?),
            plan_steps: Some(plan_steps(conn, node_id)?),
            step_obligations: Some(step_links(conn, node_id)?),
            ..Default::default()
        },
        "merged" | "released" => Inputs::default(),
        _ => return Ok(None),
    };
    Ok(Some(serde_json::to_value(inputs)?))
}

/// Lowercase hex SHA-256 of the canonical JSON of `inputs`.
pub fn digest(inputs: &Value) -> String {
    let json = serde_json::to_string(inputs).expect("a JSON value always serializes");
    format!("{:x}", Sha256::digest(json.as_bytes()))
}

fn obligations(conn: &Connection, node_id: Uuid) -> Result<Vec<InObligation>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, body FROM node_obligations WHERE node_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![uuid_to_blob(node_id)], |row| {
            Ok(InObligation {
                id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
                kind: row.get(1)?,
                body: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(sorted(rows, |o| o.id))
}

fn content(conn: &Connection, node_id: Uuid) -> Result<Vec<InContent>> {
    let mut stmt = conn.prepare(
        "SELECT id, content_type, body FROM node_extra_content
         WHERE node_id = ?1 AND content_type NOT IN ('summary', 'metadata') AND trim(body) != ''",
    )?;
    let rows = stmt
        .query_map(params![uuid_to_blob(node_id)], |row| {
            Ok(InContent {
                id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
                content_type: row.get(1)?,
                body: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(sorted(rows, |c| c.id))
}

fn mockups(conn: &Connection, node_id: Uuid) -> Result<Vec<InMockup>> {
    let mut stmt = conn.prepare(
        "SELECT id, visual_design_path FROM node_obligations
         WHERE node_id = ?1 AND visual_design_path IS NOT NULL AND visual_design_path != ''",
    )?;
    let rows = stmt
        .query_map(params![uuid_to_blob(node_id)], |row| {
            Ok((
                blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
                row.get::<_, String>(1)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let rows = rows
        .into_iter()
        .map(|(obligation_id, path)| InMockup {
            obligation_id,
            sha256: std::fs::read(&path)
                .ok()
                .map(|bytes| format!("{:x}", Sha256::digest(&bytes))),
            path,
        })
        .collect();
    Ok(sorted(rows, |m| m.obligation_id))
}

fn media(conn: &Connection, node_id: Uuid) -> Result<Vec<InMedia>> {
    let mut stmt = conn.prepare(
        "SELECT l.media_id, l.role, a.sha256 FROM node_media_links l
         JOIN media_assets a ON a.id = l.media_id WHERE l.node_id = ?1",
    )?;
    let mut rows = stmt
        .query_map(params![uuid_to_blob(node_id)], |row| {
            Ok(InMedia {
                media_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
                role: row.get(1)?,
                sha256: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.sort_by(|a, b| (a.media_id, &a.role).cmp(&(b.media_id, &b.role)));
    Ok(rows)
}

fn plan_steps(conn: &Connection, node_id: Uuid) -> Result<Vec<InStep>> {
    let mut stmt =
        conn.prepare("SELECT id, body, phase FROM node_plan_steps WHERE node_id = ?1")?;
    let rows = stmt
        .query_map(params![uuid_to_blob(node_id)], |row| {
            Ok(InStep {
                id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
                body: row.get(1)?,
                phase: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(sorted(rows, |s| s.id))
}

fn step_links(conn: &Connection, node_id: Uuid) -> Result<Vec<InLink>> {
    let mut stmt = conn.prepare(
        "SELECT l.step_id, l.obligation_id FROM node_plan_step_obligations l
         JOIN node_plan_steps s ON s.id = l.step_id WHERE s.node_id = ?1",
    )?;
    let mut rows = stmt
        .query_map(params![uuid_to_blob(node_id)], |row| {
            Ok(InLink {
                step_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
                obligation_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.sort_by_key(|l| (l.step_id, l.obligation_id));
    Ok(rows)
}

fn sorted<T>(mut rows: Vec<T>, key: impl Fn(&T) -> Uuid) -> Vec<T> {
    rows.sort_by_key(|row| key(row));
    rows
}

// ── What changed ────────────────────────────────────────────────────────

/// One short line per difference between the inputs a certificate was taken
/// over (`before`) and the inputs now (`after`), for telling a phase agent
/// why its certificate went stale.
pub fn describe_changes(before: &Value, after: &Value) -> Vec<String> {
    let before: Inputs = serde_json::from_value(before.clone()).unwrap_or_default();
    let after: Inputs = serde_json::from_value(after.clone()).unwrap_or_default();
    let mut lines = Vec::new();
    diff_keyed(
        &mut lines,
        before.obligations.as_deref().unwrap_or_default(),
        after.obligations.as_deref().unwrap_or_default(),
        |o| (o.id, short_id(o.id)),
        |id| format!("obligation {id}"),
        |a, b| {
            if a.kind != b.kind {
                "changed kind"
            } else {
                "reworded"
            }
        },
    );
    diff_keyed(
        &mut lines,
        before.content.as_deref().unwrap_or_default(),
        after.content.as_deref().unwrap_or_default(),
        |c| (c.id, c.content_type.clone()),
        |ty| format!("{ty} content"),
        |_, _| "edited",
    );
    diff_keyed(
        &mut lines,
        before.mockups.as_deref().unwrap_or_default(),
        after.mockups.as_deref().unwrap_or_default(),
        |m| (m.obligation_id, short_id(m.obligation_id)),
        |id| format!("mockup of obligation {id}"),
        |_, _| "changed",
    );
    diff_keyed(
        &mut lines,
        before.media.as_deref().unwrap_or_default(),
        after.media.as_deref().unwrap_or_default(),
        |m| (m.media_id, format!("{} ({})", short_id(m.media_id), m.role)),
        |id| format!("attachment {id}"),
        |_, _| "changed",
    );
    diff_keyed(
        &mut lines,
        before.plan_steps.as_deref().unwrap_or_default(),
        after.plan_steps.as_deref().unwrap_or_default(),
        |s| (s.id, short_id(s.id)),
        |id| format!("plan step {id}"),
        |a, b| {
            if a.phase != b.phase {
                "moved to another phase"
            } else {
                "reworded"
            }
        },
    );
    let before_links = before.step_obligations.unwrap_or_default();
    let after_links = after.step_obligations.unwrap_or_default();
    for link in &after_links {
        if !before_links.contains(link) {
            lines.push(format!(
                "plan step {} now satisfies obligation {}",
                short_id(link.step_id),
                short_id(link.obligation_id)
            ));
        }
    }
    for link in &before_links {
        if !after_links.contains(link) {
            lines.push(format!(
                "plan step {} no longer satisfies obligation {}",
                short_id(link.step_id),
                short_id(link.obligation_id)
            ));
        }
    }
    lines
}

/// Added / removed / changed lines for one kind of row, matched by id.
fn diff_keyed<T: PartialEq>(
    lines: &mut Vec<String>,
    before: &[T],
    after: &[T],
    key: impl Fn(&T) -> (Uuid, String),
    noun: impl Fn(&str) -> String,
    changed: impl Fn(&T, &T) -> &'static str,
) {
    let before: BTreeMap<Uuid, (&T, String)> =
        before.iter().map(|r| (key(r).0, (r, key(r).1))).collect();
    let after: BTreeMap<Uuid, (&T, String)> =
        after.iter().map(|r| (key(r).0, (r, key(r).1))).collect();
    for (id, (row, label)) in &after {
        match before.get(id) {
            None => lines.push(format!("{} added", noun(label))),
            Some((old, _)) if *old != *row => {
                lines.push(format!("{} {}", noun(label), changed(old, row)))
            }
            Some(_) => {}
        }
    }
    for (id, (_, label)) in &before {
        if !after.contains_key(id) {
            lines.push(format!("{} removed", noun(label)));
        }
    }
}

// ── Repo ────────────────────────────────────────────────────────────────

const COLUMNS: &str =
    "id, node_id, state, kind, digest, snapshot, conversation_id, certifier, body, created_at";

pub struct PhaseRepo<'a> {
    conn: &'a Connection,
}

impl<'a> PhaseRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Append an event for `state` on `node_id`, over the inputs as they
    /// stand now. Refused when the state is not certifiable or is not the
    /// node's current state. Callers never supply the digest.
    pub fn record(
        &self,
        node_id: Uuid,
        state: &str,
        kind: &str,
        conversation_id: Option<Uuid>,
        certifier: &str,
        body: &str,
    ) -> Result<PhaseEvent> {
        if ![PHASE_READY, PHASE_CERTIFY, PHASE_REJECT].contains(&kind) {
            bail!("unknown phase event `{kind}`");
        }
        if ![CERTIFIER_SELF, CERTIFIER_INDEPENDENT, CERTIFIER_USER].contains(&certifier) {
            bail!("unknown certifier `{certifier}`");
        }
        let current = self.current_state(node_id)?;
        let Some(snapshot) = phase_inputs(self.conn, node_id, state)? else {
            bail!("`{state}` has no phase to certify");
        };
        if current.as_deref() != Some(state) {
            bail!(
                "the node is in `{}`, not `{state}`",
                current.as_deref().unwrap_or("no lifecycle state")
            );
        }
        let digest = digest(&snapshot);
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO phase_events
             (id, node_id, state, kind, digest, snapshot, conversation_id, certifier, body, created_at)
             VALUES (?10, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                uuid_to_blob(node_id),
                state,
                kind,
                digest,
                serde_json::to_string(&snapshot)?,
                conversation_id.map(uuid_to_blob),
                certifier,
                body,
                now_ms(),
                uuid_to_blob(id),
            ],
        )?;
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM phase_events WHERE id = ?1"),
                params![uuid_to_blob(id)],
                map_event,
            )
            .context("phase event vanished after insert")
    }

    fn current_state(&self, node_id: Uuid) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT state FROM node_lifecycle WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// When the node entered its current lifecycle state (ms since the
    /// epoch): `node_lifecycle.updated_at`, which only a state *change*
    /// moves (`NodeRepo::set_lifecycle`). `None` without a lifecycle.
    pub fn stay_started_at(&self, node_id: Uuid) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT updated_at FROM node_lifecycle WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// The events about `state` in the node's current stay in it, oldest
    /// first. Empty when the node is not in `state`.
    pub fn events_in_stay(&self, node_id: Uuid, state: &str) -> Result<Vec<PhaseEvent>> {
        if self.current_state(node_id)?.as_deref() != Some(state) {
            return Ok(Vec::new());
        }
        let Some(since) = self.stay_started_at(node_id)? else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM phase_events
             WHERE node_id = ?1 AND state = ?2 AND created_at >= ?3
             ORDER BY created_at, id"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id), state, since], map_event)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every event on the node, oldest first, across every stay.
    pub fn list_for_node(&self, node_id: Uuid) -> Result<Vec<PhaseEvent>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM phase_events WHERE node_id = ?1 ORDER BY created_at, id"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_event)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The digest of `state`'s inputs as they stand now; `None` when the
    /// state is not certifiable.
    pub fn current_digest(&self, node_id: Uuid, state: &str) -> Result<Option<String>> {
        Ok(phase_inputs(self.conn, node_id, state)?.map(|inputs| digest(&inputs)))
    }

    /// The latest `certify` in the current stay whose digest equals the
    /// digest recomputed now.
    pub fn current_certificate(&self, node_id: Uuid, state: &str) -> Result<Option<PhaseEvent>> {
        let Some(now) = self.current_digest(node_id, state)? else {
            return Ok(None);
        };
        Ok(self
            .events_in_stay(node_id, state)?
            .into_iter()
            .rev()
            .find(|e| e.kind == PHASE_CERTIFY && e.digest == now))
    }

    /// Whether `state`'s phase is certified now, and if the latest
    /// certificate went stale, what changed since it was recorded.
    pub fn certificate_status(&self, node_id: Uuid, state: &str) -> Result<CertificateStatus> {
        let Some(inputs) = phase_inputs(self.conn, node_id, state)? else {
            return Ok(CertificateStatus::None);
        };
        let now = digest(&inputs);
        let certificates: Vec<PhaseEvent> = self
            .events_in_stay(node_id, state)?
            .into_iter()
            .filter(|e| e.kind == PHASE_CERTIFY)
            .collect();
        if let Some(current) = certificates.iter().rev().find(|e| e.digest == now) {
            return Ok(CertificateStatus::Current(current.clone()));
        }
        match certificates.into_iter().last() {
            None => Ok(CertificateStatus::None),
            Some(event) => {
                let changed = describe_changes(&event.snapshot, &inputs);
                Ok(CertificateStatus::Stale { event, changed })
            }
        }
    }

    /// The latest `ready` in the current stay.
    pub fn latest_ready_in_stay(&self, node_id: Uuid, state: &str) -> Result<Option<PhaseEvent>> {
        Ok(self
            .events_in_stay(node_id, state)?
            .into_iter()
            .rev()
            .find(|e| e.kind == PHASE_READY))
    }

    /// The rejections in the current stay, oldest first.
    pub fn rejections_in_stay(&self, node_id: Uuid, state: &str) -> Result<Vec<PhaseEvent>> {
        Ok(self
            .events_in_stay(node_id, state)?
            .into_iter()
            .filter(|e| e.kind == PHASE_REJECT)
            .collect())
    }

    /// The evaluation loop guard: the latest rejection in this stay has the
    /// same digest as an earlier one, so the phase agent changed nothing
    /// between them (or changed it back).
    pub fn is_stuck(&self, node_id: Uuid, state: &str) -> Result<bool> {
        let rejections = self.rejections_in_stay(node_id, state)?;
        let Some((latest, earlier)) = rejections.split_last() else {
            return Ok(false);
        };
        Ok(earlier.iter().any(|e| e.digest == latest.digest))
    }
}

/// Who a phase event is from, given the writer's actor: the user; an
/// Evaluate conversation (independent); or any other agent (self). Returns
/// `(conversation_id, certifier)`.
pub fn certifier_for_actor(conn: &Connection, actor: &str) -> Result<(Option<Uuid>, &'static str)> {
    if actor == crate::interview::ACTOR_USER {
        return Ok((None, CERTIFIER_USER));
    }
    let Some(conversation_id) = crate::conversation::actor_conversation(actor) else {
        return Ok((None, CERTIFIER_SELF));
    };
    let protocol: Option<String> = conn
        .query_row(
            "SELECT protocol FROM conversations WHERE id = ?1",
            params![uuid_to_blob(conversation_id)],
            |row| row.get(0),
        )
        .optional()?;
    let certifier = if protocol.as_deref() == Some(ProtocolKind::Evaluate.as_str()) {
        CERTIFIER_INDEPENDENT
    } else {
        CERTIFIER_SELF
    };
    Ok((Some(conversation_id), certifier))
}

fn map_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<PhaseEvent> {
    let conversation_id: Option<Vec<u8>> = row.get(6)?;
    let snapshot: String = row.get(5)?;
    Ok(PhaseEvent {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        state: row.get(2)?,
        kind: row.get(3)?,
        digest: row.get(4)?,
        snapshot: serde_json::from_str(&snapshot).unwrap_or(Value::Null),
        conversation_id: conversation_id.as_deref().map(blob_to_uuid_sql).transpose()?,
        certifier: row.get(7)?,
        body: row.get(8)?,
        created_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{ConversationRepo, Focus};
    use crate::fleet::schema;
    use crate::interview::{InterviewCommand, execute};
    use crate::outline::repos::{NodeRepo, ObligationRepo, PlanStepRepo};
    use std::path::{Path, PathBuf};

    struct Fx {
        dir: PathBuf,
        conn: Connection,
        node: Uuid,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn setup(state: &str) -> Fx {
        let dir = std::env::temp_dir().join(format!("tod-phase-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn).create_with_id(node, "n", "N").unwrap();
        NodeRepo::new(&conn).set_lifecycle(node, state).unwrap();
        Fx { dir, conn, node }
    }

    fn obligation(fx: &Fx, id: Uuid, index: usize, body: &str) {
        ObligationRepo::new(&fx.conn)
            .insert_at(id, fx.node, "requirement", index, None, body, "design")
            .unwrap();
    }

    fn certify(fx: &Fx, state: &str) -> PhaseEvent {
        PhaseRepo::new(&fx.conn)
            .record(fx.node, state, PHASE_CERTIFY, None, CERTIFIER_USER, "looks right")
            .unwrap()
    }

    fn set_state(fx: &Fx, state: &str) {
        NodeRepo::new(&fx.conn).set_lifecycle(fx.node, state).unwrap();
    }

    /// Move every event and the stay start one second into the past, so a
    /// state change right after starts a stay strictly later.
    fn age(fx: &Fx) {
        fx.conn
            .execute_batch(
                "UPDATE phase_events SET created_at = created_at - 1000;
                 UPDATE node_lifecycle SET updated_at = updated_at - 1000;",
            )
            .unwrap();
    }

    fn digest_now(fx: &Fx, state: &str) -> String {
        PhaseRepo::new(&fx.conn)
            .current_digest(fx.node, state)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn only_the_listed_states_are_certifiable() {
        let fx = setup("proposed");
        for state in CERTIFIABLE_STATES {
            assert!(is_certifiable(state));
            assert!(phase_inputs(&fx.conn, fx.node, state).unwrap().is_some());
        }
        for state in ["ready", "active", "verifying", "review", "pr", "approved", "learn", "done"] {
            assert!(!is_certifiable(state));
            assert!(phase_inputs(&fx.conn, fx.node, state).unwrap().is_none());
        }
        assert_eq!(
            phase_inputs(&fx.conn, fx.node, "merged").unwrap().unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn the_digest_ignores_order_and_moves_with_the_text() {
        let fx = setup("proposed");
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        obligation(&fx, a, 0, "First");
        obligation(&fx, b, 1, "Second");
        let before = digest_now(&fx, "proposed");
        ObligationRepo::new(&fx.conn).reorder(b, -1).unwrap();
        assert_eq!(digest_now(&fx, "proposed"), before, "reordering changes nothing");

        ObligationRepo::new(&fx.conn).update_body(a, "First, reworded").unwrap();
        assert_ne!(digest_now(&fx, "proposed"), before);
        ObligationRepo::new(&fx.conn).update_body(a, "First").unwrap();
        assert_eq!(digest_now(&fx, "proposed"), before, "a reversed change stops counting");
        assert_eq!(before.len(), 64);
        assert!(before.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn design_covers_content_but_not_the_summary() {
        let fx = setup("design");
        let nodes = NodeRepo::new(&fx.conn);
        nodes.set_extra_content(fx.node, "design", "Use a table").unwrap();
        let before = digest_now(&fx, "design");
        nodes.set_extra_content(fx.node, "summary", "Regenerated").unwrap();
        assert_eq!(digest_now(&fx, "design"), before);
        nodes.set_extra_content(fx.node, "design", "Use a list").unwrap();
        assert_ne!(digest_now(&fx, "design"), before);
    }

    #[test]
    fn design_covers_the_mockup_file_contents() {
        let fx = setup("design");
        let a = Uuid::new_v4();
        obligation(&fx, a, 0, "Looks like the mockup");
        let mockup = fx.dir.join("mockup.html");
        std::fs::write(&mockup, "<p>one</p>").unwrap();
        ObligationRepo::new(&fx.conn)
            .update_visual_design_path(a, Some(&mockup.display().to_string()))
            .unwrap();
        let before = digest_now(&fx, "design");
        std::fs::write(&mockup, "<p>two</p>").unwrap();
        assert_ne!(digest_now(&fx, "design"), before);
    }

    #[test]
    fn planning_covers_steps_and_links_but_not_status() {
        let fx = setup("planning");
        let requirement = Uuid::new_v4();
        obligation(&fx, requirement, 0, "Do it");
        let step = Uuid::new_v4();
        let steps = PlanStepRepo::new(&fx.conn);
        steps.insert_at(step, fx.node, 0, "Build it").unwrap();
        let unlinked = digest_now(&fx, "planning");
        steps.link_obligation(step, requirement).unwrap();
        let linked = digest_now(&fx, "planning");
        assert_ne!(linked, unlinked);
        steps
            .update_status(step, crate::outline::repos::plan_steps::STATUS_IMPLEMENTED, None, None)
            .unwrap();
        assert_eq!(digest_now(&fx, "planning"), linked);
    }

    #[test]
    fn a_certificate_goes_stale_and_says_what_changed() {
        let fx = setup("planning");
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        obligation(&fx, a, 0, "Keep");
        obligation(&fx, b, 1, "Reword me");
        let step = Uuid::new_v4();
        PlanStepRepo::new(&fx.conn)
            .insert_at(step, fx.node, 0, "Build it")
            .unwrap();
        let repo = PhaseRepo::new(&fx.conn);
        assert_eq!(repo.certificate_status(fx.node, "planning").unwrap(), CertificateStatus::None);
        let event = certify(&fx, "planning");
        assert_eq!(event.digest, digest_now(&fx, "planning"));
        assert!(matches!(
            repo.certificate_status(fx.node, "planning").unwrap(),
            CertificateStatus::Current(e) if e.id == event.id
        ));

        ObligationRepo::new(&fx.conn).update_body(b, "Reworded").unwrap();
        let added = Uuid::new_v4();
        PlanStepRepo::new(&fx.conn)
            .insert_at(added, fx.node, 1, "Test it")
            .unwrap();
        PlanStepRepo::new(&fx.conn).link_obligation(step, a).unwrap();
        assert!(repo.current_certificate(fx.node, "planning").unwrap().is_none());
        let CertificateStatus::Stale { event: stale, changed } =
            repo.certificate_status(fx.node, "planning").unwrap()
        else {
            panic!("expected a stale certificate");
        };
        assert_eq!(stale.id, event.id);
        assert!(changed.contains(&format!("obligation {} reworded", short_id(b))), "{changed:?}");
        assert!(changed.contains(&format!("plan step {} added", short_id(added))), "{changed:?}");
        assert!(
            changed.contains(&format!(
                "plan step {} now satisfies obligation {}",
                short_id(step),
                short_id(a)
            )),
            "{changed:?}"
        );
        assert_eq!(changed.len(), 3, "{changed:?}");

        // Reversing every change makes it current again.
        ObligationRepo::new(&fx.conn).update_body(b, "Reword me").unwrap();
        PlanStepRepo::new(&fx.conn).delete(added).unwrap();
        PlanStepRepo::new(&fx.conn).unlink_obligation(step, a).unwrap();
        assert!(repo.current_certificate(fx.node, "planning").unwrap().is_some());
    }

    #[test]
    fn a_certificate_counts_only_in_the_stay_it_was_recorded_in() {
        let fx = setup("design");
        certify(&fx, "design");
        let repo = PhaseRepo::new(&fx.conn);
        assert!(repo.current_certificate(fx.node, "design").unwrap().is_some());

        // Re-setting the same state does not start a new stay.
        age(&fx);
        set_state(&fx, "design");
        assert!(repo.current_certificate(fx.node, "design").unwrap().is_some());

        // Leaving and coming back does.
        set_state(&fx, "planning");
        assert!(repo.current_certificate(fx.node, "design").unwrap().is_none());
        assert!(repo.events_in_stay(fx.node, "design").unwrap().is_empty());
        set_state(&fx, "design");
        assert!(repo.current_certificate(fx.node, "design").unwrap().is_none());
        assert_eq!(repo.certificate_status(fx.node, "design").unwrap(), CertificateStatus::None);
        assert_eq!(repo.list_for_node(fx.node).unwrap().len(), 1);
    }

    #[test]
    fn recording_needs_the_node_in_that_certifiable_state() {
        let fx = setup("design");
        let repo = PhaseRepo::new(&fx.conn);
        assert!(repo.record(fx.node, "planning", PHASE_READY, None, CERTIFIER_SELF, "").is_err());
        set_state(&fx, "active");
        assert!(repo.record(fx.node, "active", PHASE_READY, None, CERTIFIER_SELF, "").is_err());
    }

    #[test]
    fn stuck_when_a_rejection_repeats_an_earlier_digest() {
        let fx = setup("proposed");
        let repo = PhaseRepo::new(&fx.conn);
        let fixes = serde_json::to_string(&["x"]).unwrap();
        let reject = || {
            repo.record(fx.node, "proposed", PHASE_REJECT, None, CERTIFIER_INDEPENDENT, &fixes)
                .unwrap()
        };
        let a = Uuid::new_v4();
        obligation(&fx, a, 0, "Vague");
        assert!(!repo.is_stuck(fx.node, "proposed").unwrap());
        reject();
        assert!(!repo.is_stuck(fx.node, "proposed").unwrap());
        ObligationRepo::new(&fx.conn).update_body(a, "Clear").unwrap();
        let second = reject();
        assert_eq!(second.fixes(), ["x"]);
        assert!(!repo.is_stuck(fx.node, "proposed").unwrap(), "the agent changed something");
        reject();
        assert!(repo.is_stuck(fx.node, "proposed").unwrap(), "nothing changed in between");

        // A new stay starts the count again.
        age(&fx);
        set_state(&fx, "design");
        set_state(&fx, "proposed");
        assert!(!repo.is_stuck(fx.node, "proposed").unwrap());
        assert!(repo.rejections_in_stay(fx.node, "proposed").unwrap().is_empty());
    }

    fn run(fx: &Fx, actor: &str, command: InterviewCommand) -> anyhow::Result<Value> {
        execute(&fx.conn, Path::new("."), actor, &command)
    }

    #[test]
    fn the_certifier_comes_from_the_actor() {
        let fx = setup("proposed");
        let conversations = ConversationRepo::new(&fx.conn);
        let phase = conversations
            .create(Focus::Node(fx.node), ProtocolKind::Phase, None, None, None)
            .unwrap();
        let evaluate = conversations
            .create(Focus::Node(fx.node), ProtocolKind::Evaluate, None, None, None)
            .unwrap();
        let state = || "proposed".to_string();

        run(
            &fx,
            "user",
            InterviewCommand::PhaseCertify {
                node_id: fx.node,
                state: state(),
                note: "fine".into(),
            },
        )
        .unwrap();
        run(
            &fx,
            &crate::conversation::actor_for(phase.id),
            InterviewCommand::PhaseReady {
                node_id: fx.node,
                state: state(),
            },
        )
        .unwrap();
        run(
            &fx,
            &crate::conversation::actor_for(evaluate.id),
            InterviewCommand::PhaseReject {
                node_id: fx.node,
                state: state(),
                fixes: vec!["Split the second obligation".into(), " ".into()],
            },
        )
        .unwrap();

        let events = PhaseRepo::new(&fx.conn)
            .events_in_stay(fx.node, "proposed")
            .unwrap();
        let who: Vec<_> = events
            .iter()
            .map(|e| (e.kind.as_str(), e.certifier.as_str(), e.conversation_id))
            .collect();
        assert_eq!(
            who,
            [
                (PHASE_CERTIFY, CERTIFIER_USER, None),
                (PHASE_READY, CERTIFIER_SELF, Some(phase.id)),
                (PHASE_REJECT, CERTIFIER_INDEPENDENT, Some(evaluate.id)),
            ]
        );
        assert_eq!(events[0].body, "fine");
        assert_eq!(events[2].fixes(), ["Split the second obligation"]);
        assert!(events[1].fixes().is_empty());

        // A certificate needs a note, and a rejection a fix.
        let blank_note = InterviewCommand::PhaseCertify {
            node_id: fx.node,
            state: state(),
            note: "  ".into(),
        };
        assert!(run(&fx, "user", blank_note).is_err());
        let blank_fix = InterviewCommand::PhaseReject {
            node_id: fx.node,
            state: state(),
            fixes: vec![" ".into()],
        };
        assert!(run(&fx, "user", blank_fix).is_err());
    }

    /// An evaluator judges what it is given: it may certify, reject, or ask
    /// the user, and nothing else.
    #[test]
    fn an_evaluator_cannot_change_the_node() {
        let fx = setup("proposed");
        let evaluate = ConversationRepo::new(&fx.conn)
            .create(Focus::Node(fx.node), ProtocolKind::Evaluate, None, None, None)
            .unwrap();
        let actor = crate::conversation::actor_for(evaluate.id);
        let edit = InterviewCommand::Outline {
            mutation: crate::outline::OutlineMutation::CreateObligation {
                obligation_id: None,
                node_id: fx.node,
                kind: crate::outline::KIND_REQUIREMENT.into(),
                after_id: None,
                before: false,
                section: None,
                body: "Added by the evaluator".into(),
                phase: crate::interview::PHASE_REQUIREMENTS.into(),
            },
            target: None,
        };
        let err = run(&fx, &actor, edit).unwrap_err();
        assert!(format!("{err:#}").contains("evaluator"), "{err:#}");
        let ready = InterviewCommand::PhaseReady {
            node_id: fx.node,
            state: "proposed".into(),
        };
        assert!(run(&fx, &actor, ready).is_err());
        run(
            &fx,
            &actor,
            InterviewCommand::AskDecision {
                node_id: fx.node,
                conversation_id: Some(evaluate.id),
                protocol: None,
                decision: crate::decisions::NewDecision {
                    question: "Is the second requirement meant literally?".into(),
                    ..Default::default()
                },
            },
        )
        .unwrap();
        run(
            &fx,
            &actor,
            InterviewCommand::PhaseCertify {
                node_id: fx.node,
                state: "proposed".into(),
                note: "The task is concrete.".into(),
            },
        )
        .unwrap();
    }

    #[test]
    fn deleting_the_node_cascades_its_events() {
        let fx = setup("merged");
        certify(&fx, "merged");
        fx.conn
            .execute("DELETE FROM nodes WHERE id = ?1", params![uuid_to_blob(fx.node)])
            .unwrap();
        assert!(PhaseRepo::new(&fx.conn).list_for_node(fx.node).unwrap().is_empty());
    }

    /// A store opened by the branch that introduced the table has integer
    /// ids; opening it again rebuilds the table with UUIDs, keeping every
    /// row and their order.
    #[test]
    fn an_integer_keyed_table_is_rebuilt_with_its_rows_in_order() {
        let fx = setup("merged");
        fx.conn
            .execute_batch(
                "DROP TABLE phase_events;
                 CREATE TABLE phase_events (
                     id              INTEGER PRIMARY KEY AUTOINCREMENT,
                     node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                     state           TEXT NOT NULL,
                     kind            TEXT NOT NULL,
                     digest          TEXT NOT NULL,
                     snapshot        TEXT NOT NULL,
                     conversation_id BLOB,
                     certifier       TEXT NOT NULL,
                     body            TEXT NOT NULL DEFAULT '',
                     created_at      INTEGER NOT NULL
                 );
                 CREATE INDEX idx_phase_events_node ON phase_events(node_id, state, id);",
            )
            .unwrap();
        for body in ["first", "second"] {
            fx.conn
                .execute(
                    "INSERT INTO phase_events
                     (node_id, state, kind, digest, snapshot, certifier, body, created_at)
                     VALUES (?1, 'merged', 'certify', 'd', '{}', 'user', ?2, 5)",
                    params![uuid_to_blob(fx.node), body],
                )
                .unwrap();
        }

        ensure_table(&fx.conn).unwrap();
        ensure_table(&fx.conn).unwrap();

        let events = PhaseRepo::new(&fx.conn).list_for_node(fx.node).unwrap();
        let bodies: Vec<&str> = events.iter().map(|e| e.body.as_str()).collect();
        assert_eq!(bodies, ["first", "second"]);
        assert!(events.iter().all(|e| e.id.get_version_num() == 7));
        let old_left: i64 = fx
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'phase_events_old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_left, 0);
        certify(&fx, "merged");
        assert_eq!(PhaseRepo::new(&fx.conn).list_for_node(fx.node).unwrap().len(), 3);
    }
}
