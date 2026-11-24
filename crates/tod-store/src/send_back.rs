//! Sending a node back to an earlier lifecycle state: the one store move
//! behind every way it happens, and the record of each send-back an agent
//! made (schema v103).
//!
//! [`apply_move`] is the move itself, shared by the user's
//! (`OutlineMutation::SetLifecycleByUser`: the phase chip, `tod-cli lifecycle
//! set`, a request's answer, `tod_core::lifecycle::send_back`) and an
//! agent's ([`execute`], `tod-cli lifecycle send-back`): the state changes,
//! what certified the states being redone is cleared
//! (`crate::lifecycle_reset`), requests that carry a move are withdrawn, the
//! questions of the redone states are withdrawn, and at `active` or earlier
//! the implementation steps open again. An agent's send-back to `verifying`
//! reopens only the verdicts of the obligations it names; every other move
//! reopens them all, as before.
//!
//! **Who may send a node back.** The sessions whose work is judging another
//! phase's: a phase agent, an evaluator, a verification, a review (the code
//! review and the review of the pull request) and a fix session ([`SENDERS`]),
//! and an implementation session, for a dispute only (below). The target must
//! be earlier than the node's state and before `pr_review`.
//!
//! **Disputes** (`doc/requests/design.md` 10.3). A send-back that names
//! obligations (`--obligation`) and does not go to `verifying` is a dispute:
//! an agent past the phase that wrote an obligation finds it wrong or unclear
//! and returns the node to that phase ([`dispute_target`]: `design` for a
//! design obligation, `proposed` for a requirement). Obligations are changed
//! only by the phase that wrote them, so this is the only way an agent in a
//! later state gets one changed. The receiving phase agent settles it with
//! [`resolve`]: it **fixed** the obligation, **upheld** it (with a
//! clarification), or **passed it up** (`design` to `proposed`, carrying the
//! chain: a send-back of its own whose `parent_id` is the dispute). The
//! resolution is kept on the row, and the phase that disputed it sees both
//! when the node comes forward ([`render_for_opening`]). A phase with an open
//! dispute cannot be certified by an agent ([`open_dispute_into`]).
//!
//! **Guard rails** ([`guard`]). A send-back is refused, recorded as
//! `stopped`, and the run stops for the user (`NeedsHuman::SendBackLoop` in
//! `tod_core::autopilot`) when:
//!
//! - **Loop:** the same move (from `S` to `T`) was made before for the same
//!   reason (the same cited evidence, or with none the same words), the
//!   node's obligations, plan and design content are unchanged since
//!   ([`work_digest`] equals the one recorded then) and no implementation or
//!   fix session has written since (the code is unchanged). Sending it back
//!   again would hand the same work the same way; a real fix in between, or
//!   a new finding, makes the second one new information. A refusal files a
//!   request on the node ([`LOOP_PROTOCOL`]: "Send it back anyway" / "Keep it
//!   here") so the stop always has a way forward.
//! - **Limit:** it would be the third send-back to the same target since the
//!   user last acted on the node ([`user_acted_at`]: a move of their own, a
//!   request, an answer).
//!
//! Each send-back, moved or stopped, is a row in `node_send_backs` (synced,
//! journaled) and an entry in the node's activity
//! ([`crate::node_timeline::KIND_SEND_BACK`]). The target phase's opening
//! reads the latest one into that state ([`SendBackRepo::latest_into_stay`]).
//! Unlike the task panel's chip, it does not advance the node's Linear ticket
//! (that needs the app's store, as with `tod-cli lifecycle set`).

use crate::conversation::{ConversationRepo, ProtocolKind};
use crate::decisions::{DecisionRepo, EVIDENCE_KINDS, EvidenceRef, NewDecision, OptionMove, REASON_OTHER};
use crate::interview::short_id;
use crate::lifecycle_reset::VerdictScope;
use crate::node_timeline::{KIND_SEND_BACK, TimelineEntry, TimelineRepo};
use crate::outline::repos::obligations::NodeObligation;
use crate::outline::repos::{NodeRepo, ObligationRepo};
use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use crate::settings::VALID_LIFECYCLE_STATES;
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub const TABLE: &str = "node_send_backs";

/// The node was moved.
pub const OUTCOME_MOVED: &str = "moved";
/// The guard refused the move and the run stops for the user.
pub const OUTCOME_STOPPED: &str = "stopped";

/// The first state no send-back may target: marking the pull request ready
/// and choosing a reviewer are the user's step.
pub const FIRST_REFUSED_TARGET: &str = "pr_review";

/// Send-backs to the same target since the user last acted on the node that
/// are allowed; the next one stops the run.
pub const LIMIT_PER_TARGET: usize = 2;

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS node_send_backs (
        id              BLOB PRIMARY KEY NOT NULL,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        conversation_id BLOB,
        protocol        TEXT NOT NULL,
        turn_seq        INTEGER NOT NULL DEFAULT 0,
        from_state      TEXT NOT NULL,
        to_state        TEXT NOT NULL,
        reason          TEXT NOT NULL,
        evidence        TEXT NOT NULL DEFAULT '[]',
        obligations     TEXT NOT NULL DEFAULT '[]',
        digest          TEXT NOT NULL DEFAULT '',
        outcome         TEXT NOT NULL CHECK (outcome IN ('moved', 'stopped')),
        stop_why        TEXT NOT NULL DEFAULT '',
        earlier         TEXT NOT NULL DEFAULT '[]',
        created_at      INTEGER NOT NULL,
        resolution_kind TEXT NOT NULL DEFAULT '',
        resolution      TEXT NOT NULL DEFAULT '',
        resolved_at     INTEGER,
        parent_id       BLOB
    );
    CREATE INDEX IF NOT EXISTS idx_node_send_backs_node ON node_send_backs(node_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_node_send_backs_conversation ON node_send_backs(conversation_id, turn_seq);
";

/// The dispute was fixed: the obligation was changed or removed.
pub const RESOLUTION_FIXED: &str = "fixed";
/// The dispute was upheld: the obligation stands, with a clarification.
pub const RESOLUTION_UPHELD: &str = "upheld";
/// The design phase passed the dispute up to `proposed`.
pub const RESOLUTION_PASSED_UP: &str = "passed_up";
/// What [`resolve`] accepts.
pub const RESOLUTIONS: [&str; 3] = [RESOLUTION_FIXED, RESOLUTION_UPHELD, RESOLUTION_PASSED_UP];

/// `resolution_kind`, `resolution`, `resolved_at` and `parent_id` (schema
/// v108). Idempotent.
pub fn add_dispute_columns(conn: &Connection) -> Result<()> {
    let has = |column: &str| -> Result<bool> {
        Ok(conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('node_send_backs') WHERE name = ?1",
            params![column],
            |row| row.get(0),
        )?)
    };
    for (column, ddl) in [
        ("resolution_kind", "TEXT NOT NULL DEFAULT ''"),
        ("resolution", "TEXT NOT NULL DEFAULT ''"),
        ("resolved_at", "INTEGER"),
        ("parent_id", "BLOB"),
    ] {
        if !has(column)? {
            conn.execute_batch(&format!("ALTER TABLE node_send_backs ADD COLUMN {column} {ddl};"))?;
        }
    }
    Ok(())
}

/// The phase that must settle a dispute of an obligation written in `phase`
/// (`node_obligations.phase`): `design` for a design obligation, `proposed`
/// for a requirement. `None` for one with no recorded phase.
pub fn dispute_target(phase: &str) -> Option<&'static str> {
    match phase {
        "design" => Some("design"),
        "requirements" => Some("proposed"),
        _ => None,
    }
}

/// The protocols whose sessions may send a node back. An implementation
/// session only for a dispute (see the module docs).
pub const SENDERS: [ProtocolKind; 7] = [
    ProtocolKind::Implementation,
    ProtocolKind::Phase,
    ProtocolKind::Evaluate,
    ProtocolKind::Verification,
    ProtocolKind::Review,
    ProtocolKind::Fix,
    ProtocolKind::Triage,
];

/// The error for a session (or none) that may not send a node back.
pub const WHO_MAY: &str = "only a phase agent, an evaluator, or an implementation, verification, review or fix \
    session may send a task back (`tod-cli lifecycle send-back`; an implementation session only to dispute an \
    obligation); anyone else asks the user with `tod-cli decisions ask`";

/// What kind of session a protocol's is, in words.
pub fn sender_noun(protocol: &str) -> &'static str {
    match protocol {
        "phase" => "phase agent",
        "evaluate" => "evaluator",
        "implementation" => "implementation session",
        "verification" => "verification session",
        "review" => "review session",
        "fix" => "fix session",
        "triage" => "triage session",
        _ => "agent session",
    }
}

/// An agent's send-back as asked for (`InterviewCommand::SendBack`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// The conversation whose agent sends it.
    pub conversation_id: Uuid,
    pub to: String,
    pub reason: String,
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
    /// The obligations whose verification falls short (required for a
    /// send-back to `verifying`, whose verdicts alone are reopened).
    #[serde(default)]
    pub obligations: Vec<Uuid>,
}

/// One recorded send-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendBack {
    pub id: Uuid,
    pub node_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub protocol: String,
    pub turn_seq: i64,
    pub from_state: String,
    pub to_state: String,
    pub reason: String,
    pub evidence: Vec<EvidenceRef>,
    pub obligations: Vec<Uuid>,
    pub digest: String,
    pub outcome: String,
    pub stop_why: String,
    /// For a stopped one: the reasons of the earlier send-backs it repeats,
    /// oldest first.
    pub earlier: Vec<String>,
    pub created_at: i64,
    /// How a dispute was settled ([`RESOLUTIONS`]); empty while open (and for
    /// a send-back that is not a dispute).
    pub resolution_kind: String,
    /// The receiving phase agent's note on it.
    pub resolution: String,
    pub resolved_at: Option<i64>,
    /// The dispute this one passes up (`design` to `proposed`).
    pub parent_id: Option<Uuid>,
}

impl SendBack {
    pub fn moved(&self) -> bool {
        self.outcome == OUTCOME_MOVED
    }

    /// A send-back that names obligations and does not go to `verifying`
    /// (whose obligations are the verdicts to reopen).
    pub fn is_dispute(&self) -> bool {
        !self.obligations.is_empty() && self.to_state != "verifying"
    }

    /// A dispute the receiving phase has not settled yet.
    pub fn is_open_dispute(&self) -> bool {
        self.moved() && self.is_dispute() && self.resolution_kind.is_empty()
    }

    /// Who sent it, in words ("the review session").
    pub fn sender(&self) -> &'static str {
        sender_noun(&self.protocol)
    }
}

const COLUMNS: &str = "id, node_id, conversation_id, protocol, turn_seq, from_state, to_state, reason, \
    evidence, obligations, digest, outcome, stop_why, earlier, created_at, resolution_kind, resolution, resolved_at, parent_id";

fn map_row(row: &Row<'_>) -> rusqlite::Result<SendBack> {
    let conversation: Option<Vec<u8>> = row.get(2)?;
    let evidence: String = row.get(8)?;
    let obligations: String = row.get(9)?;
    let earlier: String = row.get(13)?;
    let parent: Option<Vec<u8>> = row.get(18)?;
    let json_err = |i: usize, e: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    };
    Ok(SendBack {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        conversation_id: conversation.as_deref().map(blob_to_uuid_sql).transpose()?,
        protocol: row.get(3)?,
        turn_seq: row.get(4)?,
        from_state: row.get(5)?,
        to_state: row.get(6)?,
        reason: row.get(7)?,
        evidence: serde_json::from_str(&evidence).map_err(|e| json_err(8, e))?,
        obligations: serde_json::from_str(&obligations).map_err(|e| json_err(9, e))?,
        digest: row.get(10)?,
        outcome: row.get(11)?,
        stop_why: row.get(12)?,
        earlier: serde_json::from_str(&earlier).map_err(|e| json_err(13, e))?,
        created_at: row.get(14)?,
        resolution_kind: row.get(15)?,
        resolution: row.get(16)?,
        resolved_at: row.get(17)?,
        parent_id: parent.as_deref().map(blob_to_uuid_sql).transpose()?,
    })
}

pub struct SendBackRepo<'a> {
    conn: &'a Connection,
}

impl<'a> SendBackRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn insert(&self, row: &SendBack) -> Result<()> {
        self.conn.execute(
            &format!("INSERT INTO node_send_backs ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)"),
            params![
                uuid_to_blob(row.id),
                uuid_to_blob(row.node_id),
                row.conversation_id.map(uuid_to_blob),
                row.protocol,
                row.turn_seq,
                row.from_state,
                row.to_state,
                row.reason,
                serde_json::to_string(&row.evidence)?,
                serde_json::to_string(&row.obligations)?,
                row.digest,
                row.outcome,
                row.stop_why,
                serde_json::to_string(&row.earlier)?,
                row.created_at,
                row.resolution_kind,
                row.resolution,
                row.resolved_at,
                row.parent_id.map(uuid_to_blob),
            ],
        )?;
        Ok(())
    }

    /// One send-back by id.
    pub fn get(&self, id: Uuid) -> Result<Option<SendBack>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM node_send_backs WHERE id = ?1"),
                params![uuid_to_blob(id)],
                map_row,
            )
            .optional()?)
    }

    /// Every send-back of `node`, oldest first.
    pub fn list_for_node(&self, node: Uuid) -> Result<Vec<SendBack>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM node_send_backs WHERE node_id = ?1 ORDER BY created_at, rowid"
        ))?;
        let rows = stmt.query_map(params![uuid_to_blob(node)], map_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The send-backs conversation `id` recorded during or after its turn
    /// `turn_seq` (the driver's check that the turn sent the node back).
    pub fn for_conversation_since(&self, id: Uuid, turn_seq: i64) -> Result<Vec<SendBack>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM node_send_backs WHERE conversation_id = ?1 AND turn_seq >= ?2
             ORDER BY created_at, rowid"
        ))?;
        let rows = stmt.query_map(params![uuid_to_blob(id), turn_seq], map_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The send-back that moved `node` into the state it is in now, in this
    /// stay: what that phase's opening starts from. `None` when the node got
    /// here any other way.
    pub fn latest_into_stay(&self, node: Uuid) -> Result<Option<SendBack>> {
        let stay: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT state, updated_at FROM node_lifecycle WHERE node_id = ?1",
                params![uuid_to_blob(node)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, since)) = stay else {
            return Ok(None);
        };
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM node_send_backs
                     WHERE node_id = ?1 AND to_state = ?2 AND outcome = 'moved' AND created_at >= ?3
                     ORDER BY created_at DESC, rowid DESC LIMIT 1"
                ),
                params![uuid_to_blob(node), state, since],
                map_row,
            )
            .optional()?)
    }

    /// The send-backs of `node` the guard refused at or after `since`.
    pub fn stopped_since(&self, node: Uuid, since: i64) -> Result<Vec<SendBack>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM node_send_backs WHERE node_id = ?1 AND outcome = 'stopped' AND created_at >= ?2
             ORDER BY created_at, rowid"
        ))?;
        let rows = stmt.query_map(params![uuid_to_blob(node), since], map_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// When the user last acted on `node` (0 for never): a lifecycle move of
/// their own, a request on it (Run, Pause, a new session, ...), or an answer
/// to one of its requests.
pub fn user_acted_at(conn: &Connection, node: Uuid) -> Result<i64> {
    let node = uuid_to_blob(node);
    let at: Option<i64> = conn.query_row(
        "SELECT MAX(at) FROM (
             SELECT at FROM node_timeline WHERE node_id = ?1 AND kind = 'phase' AND by_user = 1
             UNION ALL
             SELECT requested_at FROM node_requests WHERE node_id = ?1
             UNION ALL
             SELECT a.answered_at FROM decision_answers a JOIN decisions d ON d.id = a.decision_id
             WHERE d.node_id = ?1 AND (a.actor = 'user' OR a.actor LIKE 'shepherd:%')
         )",
        params![node],
        |row| row.get(0),
    )?;
    Ok(at.unwrap_or(0))
}

/// A digest of what a send-back is about: the node's obligations, plan and
/// design content (`crate::phase::phase_inputs` for `design` and `planning`).
pub fn work_digest(conn: &Connection, node: Uuid) -> Result<String> {
    let inputs = json!({
        "design": crate::phase::phase_inputs(conn, node, "design")?,
        "planning": crate::phase::phase_inputs(conn, node, "planning")?,
    });
    Ok(crate::phase::digest(&inputs))
}

/// Whether an implementation or fix session wrote anything on `node` after
/// `since`: the only way the code changes while the node's obligations, plan
/// and design stay as they were. (Repository heads are not in the store; the
/// turns of the sessions that commit are.)
fn code_worked_on_since(conn: &Connection, node: Uuid, since: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM conversation_turns t JOIN conversations c ON c.id = t.conversation_id
             WHERE (c.focus_id = ?1 OR c.focus_node_id = ?1) AND c.protocol IN ('implementation', 'fix')
               AND t.role = 'agent' AND t.created_at > ?2
         )",
        params![uuid_to_blob(node), since],
        |row| row.get(0),
    )?)
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Whether a send-back citing `reason` and `evidence` is the one `earlier`
/// made: the same evidence when either cites any, else the same words.
fn same_cause(earlier: &SendBack, reason: &str, evidence: &[EvidenceRef]) -> bool {
    if earlier.evidence.is_empty() && evidence.is_empty() {
        return normalized(&earlier.reason) == normalized(reason);
    }
    let ids = |refs: &[EvidenceRef]| -> std::collections::BTreeSet<(String, Uuid)> {
        refs.iter().map(|e| (e.kind.clone(), e.id)).collect()
    };
    ids(&earlier.evidence) == ids(evidence)
}

/// Why a send-back from `from` to `to` with `digest` must stop the run for
/// the user, if it must, with the earlier send-backs it repeats (oldest
/// first). See the module docs for the two rules.
pub fn guard(
    conn: &Connection,
    node: Uuid,
    from: &str,
    to: &str,
    digest: &str,
    reason: &str,
    evidence: &[EvidenceRef],
) -> Result<Option<(String, Vec<SendBack>)>> {
    let moved: Vec<SendBack> = SendBackRepo::new(conn)
        .list_for_node(node)?
        .into_iter()
        .filter(SendBack::moved)
        .collect();
    if let Some(previous) = moved
        .iter()
        .rev()
        .find(|s| s.from_state == from && s.to_state == to)
        && previous.digest == digest
        && same_cause(previous, reason, evidence)
        && !code_worked_on_since(conn, node, previous.created_at)?
    {
        return Ok(Some((
            format!(
                "it was sent back from {from} to {to} before for the same reason, and its obligations, plan, design \
                 and code are unchanged since: sending it back again would hand over the same work the same way"
            ),
            vec![previous.clone()],
        )));
    }
    let since = user_acted_at(conn, node)?;
    let recent: Vec<SendBack> = moved
        .into_iter()
        .filter(|s| s.to_state == to && s.created_at >= since)
        .collect();
    if recent.len() >= LIMIT_PER_TARGET {
        return Ok(Some((
            format!(
                "it was sent back to {to} {} times since you last acted on it; a third time needs you",
                recent.len()
            ),
            recent,
        )));
    }
    Ok(None)
}

/// One move of a node's lifecycle state, by the user or an agent.
#[derive(Debug, Clone, Copy)]
pub struct Move<'a> {
    pub node: Uuid,
    pub to: &'a str,
    /// The user chose it (the activity shows it as theirs).
    pub by_user: bool,
    pub reason: Option<&'a str>,
    /// The request whose answer makes this move, which stays pending until
    /// its answer is recorded.
    pub keep_decision: Option<Uuid>,
    pub verdicts: VerdictScope<'a>,
}

/// Move the node (see the module docs). Returns the state it left (`None`
/// when it had none).
pub fn apply_move(conn: &Connection, m: &Move<'_>) -> Result<Option<String>> {
    let nodes = NodeRepo::new(conn);
    let before = nodes.get_lifecycle(m.node)?;
    nodes.set_lifecycle_scoped(m.node, m.to, m.by_user, m.reason, m.verdicts)?;
    if before.as_deref() != Some(m.to) {
        crate::decisions::DecisionRepo::new(conn).withdraw_moving_for_node(m.node, m.keep_decision)?;
        if let Some(from) = before.as_deref() {
            crate::lifecycle_reset::rework_after_user_move(conn, m.node, from, m.to, m.reason, m.keep_decision)?;
        }
    }
    Ok(before)
}

fn rank(state: &str) -> Option<usize> {
    VALID_LIFECYCLE_STATES.iter().position(|s| *s == state)
}

/// The session `actor` writes as: the conversation, which must be the one
/// the request names, and its node.
fn session(conn: &Connection, actor: &str, conversation_id: Uuid) -> Result<(crate::conversation::Conversation, Uuid)> {
    let Some(conversation) = ConversationRepo::new(conn).get(conversation_id)? else {
        bail!("conversation {conversation_id} not found: {WHO_MAY}");
    };
    let named = crate::conversation::actor_conversation(actor)
        .or_else(|| crate::conversation::agent_actor_conversation(actor));
    if named.is_some_and(|id| id != conversation.id) {
        bail!("conversation {} is not the one writing ({actor})", conversation.id);
    }
    let Some(node) = conversation.focus.node_id() else {
        bail!("conversation {} is not about a node", conversation.id);
    };
    Ok((conversation, node))
}

/// Carry out an agent's send-back as `actor`, or record that the guard
/// refused it. Returns `{"outcome": "moved", "from", "to"}` or
/// `{"outcome": "stopped", "why"}`; a refusal is not an error, so its row is
/// kept (the caller tells the agent).
pub fn execute(conn: &Connection, actor: &str, request: &Request) -> Result<Value> {
    let (conversation, node) = session(conn, actor, request.conversation_id)?;
    if !SENDERS.contains(&conversation.protocol) {
        bail!("a {} session may not send a task back: {WHO_MAY}", conversation.protocol.as_str());
    }
    let reason = request.reason.trim();
    if reason.is_empty() {
        bail!("a send-back needs a reason: what is wrong, and why it is that phase's to fix");
    }
    let to = request.to.trim();
    let Some(to_rank) = rank(to) else {
        bail!("`{to}` is not a lifecycle state (expected one of {})", VALID_LIFECYCLE_STATES.join(", "));
    };
    if to_rank >= rank(FIRST_REFUSED_TARGET).unwrap_or(usize::MAX) {
        bail!("a task cannot be sent back to `{to}`: only states before {FIRST_REFUSED_TARGET} can be");
    }
    let Some(from) = NodeRepo::new(conn).get_lifecycle(node)? else {
        bail!("node {node} has no lifecycle");
    };
    if rank(&from).is_none_or(|from_rank| to_rank >= from_rank) {
        bail!("the task is in `{from}`, which is not later than `{to}`: there is nothing to send back");
    }
    for evidence in &request.evidence {
        if !EVIDENCE_KINDS.contains(&evidence.kind.as_str()) {
            bail!("unknown evidence kind `{}` (expected {})", evidence.kind, EVIDENCE_KINDS.join("|"));
        }
    }
    let dispute = !request.obligations.is_empty() && to != "verifying";
    if conversation.protocol == ProtocolKind::Implementation && !dispute {
        bail!(
            "an implementation session sends a task back only to dispute an obligation (--obligation, to the \
             phase that wrote it); for anything else record the step blocked or ask the user"
        );
    }
    if conversation.protocol == ProtocolKind::Triage && !dispute {
        bail!(
            "a triage session sends a task back only to dispute an obligation of the request it reads \
             (`triage convert-dispute`, to the phase that wrote it); for anything else escalate the request"
        );
    }
    let verdicts = crate::verification::VerdictRepo::new(conn).latest_for_node(node)?;
    let obligations = ObligationRepo::new(conn);
    for id in &request.obligations {
        let Some(obligation) = obligations.get(*id)? else {
            bail!("obligation {} not found", short_id(*id));
        };
        if to == "verifying" && !verdicts.get(id).is_some_and(|v| v.is_verified()) {
            bail!(
                "obligation {} is not verified on this task, so there is no verdict to reopen",
                short_id(*id)
            );
        }
        if dispute {
            check_disputable(&obligation, node, to)?;
        }
    }
    if to == "verifying" && request.obligations.is_empty() {
        bail!(
            "a send-back to verifying names the obligations whose evidence falls short (--obligation): \
             only their verdicts are reopened"
        );
    }
    record_move(
        conn,
        &Mover {
            conversation: &conversation,
            node,
            from: &from,
            to,
            reason,
            evidence: &request.evidence,
            obligations: &request.obligations,
            parent_id: None,
        },
    )
}

/// Whether `obligation` may be disputed with a send-back to `to`: it is this
/// node's, an agent's (the user's are changed by the user alone), and `to` is
/// the phase that wrote it ([`dispute_target`]).
fn check_disputable(obligation: &NodeObligation, node: Uuid, to: &str) -> Result<()> {
    let short = short_id(obligation.id);
    if obligation.node_id != node {
        bail!("obligation {short} belongs to another node: a dispute names this node's own obligations");
    }
    if obligation.user_owned() {
        bail!(
            "obligation {short} is the user's ({}): an agent cannot dispute it. If it looks wrong, ask the user \
             with `tod-cli decisions ask`, proposing the change with --edit-obligation",
            obligation.authority
        );
    }
    let Some(target) = dispute_target(&obligation.phase) else {
        bail!(
            "obligation {short} has no recorded phase of origin (`{}`), so there is no phase to send it back to; \
             ask the user with `tod-cli decisions ask`",
            obligation.phase
        );
    };
    if target != to {
        bail!(
            "obligation {short} was written in {} (its phase is `{}`): a dispute of it goes to `{target}`, not `{to}`",
            if target == "proposed" { "the requirements" } else { "design" },
            obligation.phase
        );
    }
    Ok(())
}

/// A move back and everything recorded about it.
struct Mover<'a> {
    conversation: &'a crate::conversation::Conversation,
    node: Uuid,
    from: &'a str,
    to: &'a str,
    reason: &'a str,
    evidence: &'a [EvidenceRef],
    obligations: &'a [Uuid],
    /// The dispute this one passes up.
    parent_id: Option<Uuid>,
}

/// The `protocol` of the request a refused send-back files.
pub const LOOP_PROTOCOL: &str = "send_back_loop";
/// Its options: the first moves the node ([`OptionMove`], answered as the
/// user's own move, `tod_core::lifecycle::send_back`), the second leaves it.
pub const SEND_ANYWAY: &str = "Send it back anyway";
pub const KEEP_HERE: &str = "Keep it here";

/// File the request that gives a refused send-back (`stopped`) a way forward:
/// the user sees every reason given and either sends the node back anyway or
/// keeps it where it is. One per target while one is open.
fn file_loop_request(conn: &Connection, stopped: &SendBack) -> Result<()> {
    let repo = DecisionRepo::new(conn);
    let open = repo.list_open_for_node(stopped.node_id)?;
    if open.iter().any(|d| {
        d.protocol.as_deref() == Some(LOOP_PROTOCOL) && d.moves.iter().any(|m| m.to == stopped.to_state)
    }) {
        return Ok(());
    }
    let mut reasons = stopped.earlier.clone();
    reasons.push(stopped.reason.clone());
    let listed: String = reasons.iter().map(|r| format!("\n- {r}")).collect();
    let question = format!(
        "The {} asked to send this task back from {} to {}, and tod stopped it: {}. Reasons given:{listed}\n\n\
         Send it back anyway, or keep it in {}?",
        stopped.sender(),
        stopped.from_state,
        stopped.to_state,
        stopped.stop_why,
        stopped.from_state,
    );
    repo.create(
        stopped.node_id,
        None,
        Some(LOOP_PROTOCOL),
        &NewDecision {
            question,
            options: vec![SEND_ANYWAY.to_string(), KEEP_HERE.to_string()],
            evidence: stopped.evidence.clone(),
            reason: REASON_OTHER.to_string(),
            moves: vec![OptionMove { option: 1, to: stopped.to_state.clone(), asked_in: None }],
            ..Default::default()
        },
    )?;
    Ok(())
}

/// The guard, the move, the row and the activity entry of a validated
/// send-back: `{"outcome": "moved", ...}` or `{"outcome": "stopped", ...}`.
fn record_move(conn: &Connection, m: &Mover<'_>) -> Result<Value> {
    let (node, from, to) = (m.node, m.from, m.to);
    let digest = work_digest(conn, node)?;
    let turn_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM conversation_turns WHERE conversation_id = ?1",
        params![uuid_to_blob(m.conversation.id)],
        |row| row.get(0),
    )?;
    let stop = guard(conn, node, from, to, &digest, m.reason, m.evidence)?;
    let mut row = SendBack {
        id: Uuid::now_v7(),
        node_id: node,
        conversation_id: Some(m.conversation.id),
        protocol: m.conversation.protocol.as_str().to_string(),
        turn_seq,
        from_state: from.to_string(),
        to_state: to.to_string(),
        reason: m.reason.to_string(),
        evidence: m.evidence.to_vec(),
        obligations: m.obligations.to_vec(),
        digest,
        outcome: OUTCOME_MOVED.to_string(),
        stop_why: String::new(),
        earlier: Vec::new(),
        created_at: 0,
        resolution_kind: String::new(),
        resolution: String::new(),
        resolved_at: None,
        parent_id: m.parent_id,
    };
    let sender = row.sender();
    if let Some((why, earlier)) = stop {
        row.outcome = OUTCOME_STOPPED.to_string();
        row.stop_why = why.clone();
        row.earlier = earlier.into_iter().map(|s| s.reason).collect();
        row.created_at = now_ms();
        SendBackRepo::new(conn).insert(&row)?;
        let title = format!("The {sender} asked to send it back to {to}; stopped for you");
        TimelineRepo::new(conn).record(&TimelineEntry::new(node, KIND_SEND_BACK, from, title, true, detail(&row)))?;
        file_loop_request(conn, &row)?;
        return Ok(json!({ "outcome": OUTCOME_STOPPED, "why": why, "id": row.id }));
    }

    let moved_reason = format!("sent back by the {sender}: {}", m.reason);
    apply_move(
        conn,
        &Move {
            node,
            to,
            by_user: false,
            reason: Some(&moved_reason),
            keep_decision: None,
            verdicts: if to == "verifying" { VerdictScope::Only(m.obligations) } else { VerdictScope::All },
        },
    )?;
    // At or after the phase change, so the activity shows it heading the new stay.
    row.created_at = now_ms();
    SendBackRepo::new(conn).insert(&row)?;
    let title = if row.is_dispute() {
        format!("Disputed from {from} by the {sender}")
    } else {
        format!("Sent back from {from} by the {sender}")
    };
    TimelineRepo::new(conn).record(&TimelineEntry::new(node, KIND_SEND_BACK, to, title, false, detail(&row)))?;
    crate::lessons::record_dispute_lesson(conn, &row)?;
    Ok(json!({ "outcome": OUTCOME_MOVED, "from": from, "to": to, "id": row.id }))
}


/// A request to settle a dispute (`tod-cli lifecycle resolve-dispute`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    /// The conversation whose agent (the receiving phase's) settles it.
    pub conversation_id: Uuid,
    /// The dispute; `None` for the open one sent to the node's current state.
    #[serde(default)]
    pub send_back: Option<Uuid>,
    /// One of [`RESOLUTIONS`].
    pub kind: String,
    /// Why: what was changed, how the obligation is meant, or the note passed
    /// up with it.
    pub note: String,
}

/// The open dispute sent to `node`'s current state, if any: what its phase
/// agent has to settle before it can certify the phase.
pub fn open_dispute_into(conn: &Connection, node: Uuid, state: &str) -> Result<Option<SendBack>> {
    let since: i64 = conn
        .query_row(
            "SELECT updated_at FROM node_lifecycle WHERE node_id = ?1",
            params![uuid_to_blob(node)],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(SendBackRepo::new(conn)
        .list_for_node(node)?
        .into_iter()
        .rev()
        .find(|s| s.is_open_dispute() && s.to_state == state && s.created_at >= since))
}

/// Settle a dispute as `actor`, the receiving phase's agent: fixed (an
/// obligation was changed or removed), upheld (it stands, with the note's
/// clarification), or passed up (`design` to `proposed`, as a send-back of its
/// own that carries the chain). Returns `{"outcome": ...}`: `moved` and
/// `stopped` for a pass-up, as for a send-back, else the resolution kind.
pub fn resolve(conn: &Connection, actor: &str, request: &Resolution) -> Result<Value> {
    let (conversation, node) = session(conn, actor, request.conversation_id)?;
    if conversation.protocol != ProtocolKind::Phase {
        bail!(
            "only the phase agent of the state a dispute was sent to settles it: a {} session may not",
            conversation.protocol.as_str()
        );
    }
    let kind = request.kind.trim();
    if !RESOLUTIONS.contains(&kind) {
        bail!("`{kind}` is not a resolution (expected one of {})", RESOLUTIONS.join(", "));
    }
    let note = request.note.trim();
    if note.is_empty() {
        bail!("a resolution needs a note: what was changed, how the obligation is meant, or why it is passed up");
    }
    let Some(state) = NodeRepo::new(conn).get_lifecycle(node)? else {
        bail!("node {node} has no lifecycle");
    };
    let repo = SendBackRepo::new(conn);
    let dispute = match request.send_back {
        Some(id) => repo.get(id)?.ok_or_else(|| anyhow::anyhow!("send-back {} not found", short_id(id)))?,
        None => open_dispute_into(conn, node, &state)?.ok_or_else(|| {
            anyhow::anyhow!("no dispute is open for `{state}`: nothing was sent back to it with obligations named")
        })?,
    };
    if dispute.node_id != node || !dispute.is_open_dispute() {
        bail!("send-back {} is not an open dispute of this task", short_id(dispute.id));
    }
    if dispute.to_state != state {
        bail!(
            "dispute {} was sent to `{}`, and the task is in `{state}`: its phase agent settles it",
            short_id(dispute.id),
            dispute.to_state
        );
    }

    if kind == RESOLUTION_PASSED_UP {
        if state != "design" {
            bail!("only the design phase passes a dispute up (to `proposed`, which reads the ticket)");
        }
        let chain = format!("{}\n\nPassed up by the design agent: {note}", dispute.reason);
        let moved = record_move(
            conn,
            &Mover {
                conversation: &conversation,
                node,
                from: &state,
                to: "proposed",
                reason: &chain,
                evidence: &dispute.evidence,
                obligations: &dispute.obligations,
                parent_id: Some(dispute.id),
            },
        )?;
        if moved["outcome"] == OUTCOME_MOVED {
            settle(conn, &dispute, RESOLUTION_PASSED_UP, note, &state)?;
        }
        return Ok(moved);
    }

    if kind == RESOLUTION_FIXED {
        let obligations = ObligationRepo::new(conn);
        let mut changed = false;
        for id in &dispute.obligations {
            match obligations.get(*id)? {
                None => changed = true,
                Some(_) => {
                    let updated: i64 = conn.query_row(
                        "SELECT updated_at FROM node_obligations WHERE id = ?1",
                        params![uuid_to_blob(*id)],
                        |row| row.get(0),
                    )?;
                    changed |= updated > dispute.created_at;
                }
            }
        }
        if !changed {
            bail!(
                "none of the disputed obligations was changed or removed since it was disputed, so it is not \
                 `fixed`: change one, or resolve it as `upheld` with a note on how it is meant"
            );
        }
    }
    settle(conn, &dispute, kind, note, &state)?;
    Ok(json!({ "outcome": kind, "id": dispute.id }))
}

/// Record how `dispute` was settled, and settle the dispute it was passed up
/// from with the same outcome.
fn settle(conn: &Connection, dispute: &SendBack, kind: &str, note: &str, state: &str) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "UPDATE node_send_backs SET resolution_kind = ?1, resolution = ?2, resolved_at = ?3 WHERE id = ?4",
        params![kind, note, now, uuid_to_blob(dispute.id)],
    )?;
    let title = match kind {
        RESOLUTION_FIXED => format!("Dispute fixed in {state}"),
        RESOLUTION_UPHELD => format!("Dispute upheld in {state}"),
        _ => format!("Dispute passed up from {state}"),
    };
    TimelineRepo::new(conn).record(&TimelineEntry::new(
        dispute.node_id,
        KIND_SEND_BACK,
        state,
        title,
        false,
        note.to_string(),
    ))?;
    let mut settled = dispute.clone();
    settled.resolution_kind = kind.to_string();
    settled.resolution = note.to_string();
    settled.resolved_at = Some(now);
    crate::lessons::record_dispute_lesson(conn, &settled)?;
    // The dispute this one carried up is settled the way it was: its own
    // phase agent's note, then the phase that settled it.
    if kind != RESOLUTION_PASSED_UP
        && let Some(parent) = dispute.parent_id.map(|id| SendBackRepo::new(conn).get(id)).transpose()?.flatten()
    {
        let combined = format!("{}\n\nSettled in {state} ({kind}): {note}", parent.resolution);
        conn.execute(
            "UPDATE node_send_backs SET resolution_kind = ?1, resolution = ?2, resolved_at = ?3 WHERE id = ?4",
            params![kind, combined.trim(), now, uuid_to_blob(parent.id)],
        )?;
        let mut up = parent.clone();
        up.resolution_kind = kind.to_string();
        up.resolution = combined;
        up.resolved_at = Some(now);
        crate::lessons::record_dispute_lesson(conn, &up)?;
    }
    Ok(())
}

/// The activity's text for a send-back.
fn detail(row: &SendBack) -> String {
    let mut text = format!("{}\n", row.reason);
    if !row.evidence.is_empty() {
        let evidence: Vec<String> = row.evidence.iter().map(|e| format!("{}:{}", e.kind, short_id(e.id))).collect();
        text.push_str(&format!("\nEvidence: {}", evidence.join(", ")));
    }
    if !row.obligations.is_empty() {
        let obligations: Vec<String> = row.obligations.iter().map(|o| short_id(*o)).collect();
        text.push_str(&format!("\nObligations: {}", obligations.join(", ")));
    }
    if !row.stop_why.is_empty() {
        text.push_str(&format!("\nNot moved: {}", row.stop_why));
    }
    text.trim_end().to_string()
}

/// The obligations a send-back names, with their words and who stands
/// behind them, one line each.
fn render_obligations(conn: &Connection, sent: &SendBack, text: &mut String) -> Result<()> {
    for obligation in &sent.obligations {
        match ObligationRepo::new(conn).get(*obligation)? {
            Some(o) => text.push_str(&format!(
                "- [{}] {}{}\n",
                short_id(*obligation),
                o.body,
                if o.authority == crate::outline::repos::obligations::AUTHORITY_AGENT {
                    String::new()
                } else {
                    format!(" ({})", o.authority)
                }
            )),
            None => text.push_str(&format!("- [{}] (deleted since)\n", short_id(*obligation))),
        }
    }
    Ok(())
}

/// A dispute and the chain it came up through, oldest first: who disputed it
/// from where, why, and what each phase on the way said.
fn render_chain(conn: &Connection, sent: &SendBack, text: &mut String) -> Result<()> {
    let mut chain = vec![sent.clone()];
    while let Some(parent) = chain.last().and_then(|s| s.parent_id) {
        match SendBackRepo::new(conn).get(parent)? {
            Some(p) => chain.push(p),
            None => break,
        }
    }
    chain.reverse();
    for link in &chain {
        text.push_str(&format!(
            "\nThe {} disputed it from `{}` to `{}`:\n\n{}\n",
            link.sender(),
            link.from_state,
            link.to_state,
            link.reason
        ));
    }
    Ok(())
}

/// The section a phase's opening starts with when a send-back brought the
/// node there: what was wrong, the evidence, the state it came from, and
/// which kind of session sent it. For a dispute, the chain it came up
/// through and what the phase is to do with it. Otherwise, when the node
/// comes forward into a phase that had disputed an obligation and it was
/// settled meanwhile, that dispute and how it was settled. Empty when none.
pub fn render_for_opening(conn: &Connection, node: Uuid) -> Result<String> {
    let Some(sent) = SendBackRepo::new(conn).latest_into_stay(node)? else {
        return render_settled_dispute(conn, node);
    };
    let mut text = if sent.is_dispute() {
        let mut text = format!(
            "## Dispute\n\nThe {} sent this task back from `{}` to `{}` to dispute the obligations below, which this \
             phase wrote. Settle it before you certify the phase: fix the obligation (change or remove it), or \
             uphold it (keep it, and say how it is meant, sharpening its wording if that would have spared the \
             dispute)",
            sent.sender(),
            sent.from_state,
            sent.to_state,
        );
        text.push_str(if sent.to_state == "design" {
            ", or pass it up to `proposed` when it only restates a requirement. Record how you settled it with the \
             dispute resolution command.\n"
        } else {
            ". You alone read the ticket: compare the chain with it. If the ticket itself is in doubt, ask the user \
             a real question (what the ticket says, what the code shows). Record how you settled it with the \
             dispute resolution command.\n"
        });
        render_chain(conn, &sent, &mut text)?;
        text
    } else {
        format!(
            "## Sent back\n\nThe {} sent this task back from `{}` to `{}`. Start from this cause, not from scratch:\n\n{}\n",
            sent.sender(),
            sent.from_state,
            sent.to_state,
            sent.reason
        )
    };
    if !sent.evidence.is_empty() {
        text.push_str("\nEvidence:\n");
        for evidence in &sent.evidence {
            text.push_str(&format!("- {} {}\n", evidence.kind, evidence.id));
        }
    }
    if !sent.obligations.is_empty() {
        text.push_str("\nObligations named:\n");
        render_obligations(conn, &sent, &mut text)?;
    }
    Ok(text)
}

/// The dispute the node's current state raised, once its phase settled it and
/// the node came forward: the dispute, how it was settled and the note, so
/// the agent sees why the obligation reads as it does. The latest such
/// dispute, while the node is in the stay that followed its settling.
fn render_settled_dispute(conn: &Connection, node: Uuid) -> Result<String> {
    let Some(state) = NodeRepo::new(conn).get_lifecycle(node)? else {
        return Ok(String::new());
    };
    let since: i64 = conn
        .query_row(
            "SELECT updated_at FROM node_lifecycle WHERE node_id = ?1",
            params![uuid_to_blob(node)],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let Some(sent) = SendBackRepo::new(conn)
        .list_for_node(node)?
        .into_iter()
        .rev()
        .find(|s| s.moved() && s.is_dispute() && s.from_state == state && s.parent_id.is_none())
    else {
        return Ok(String::new());
    };
    let Some(_resolved_at) = sent.resolved_at.filter(|at| *at <= since) else {
        return Ok(String::new());
    };
    let how = match sent.resolution_kind.as_str() {
        RESOLUTION_FIXED => "fixed: the obligation was changed or removed",
        RESOLUTION_UPHELD => "upheld: the obligation stands",
        _ => "passed up and settled",
    };
    let mut text = format!(
        "## Dispute settled\n\nYou sent this task back from `{}` to dispute the obligations below. It was {how}:\n\n{}\n\nWhat you disputed:\n\n{}\n",
        sent.from_state, sent.resolution, sent.reason
    );
    text.push_str("\nObligations named (as they read now):\n");
    render_obligations(conn, &sent, &mut text)?;
    Ok(text)
}

#[cfg(test)]
mod tests;
