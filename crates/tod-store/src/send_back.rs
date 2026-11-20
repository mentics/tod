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
//! review and the review of the pull request) and a fix session ([`SENDERS`]).
//! The target must be earlier than the node's state and before `pr_review`.
//!
//! **Guard rails** ([`guard`]). A send-back is refused, recorded as
//! `stopped`, and the run stops for the user (`NeedsHuman::SendBackLoop` in
//! `tod_core::autopilot`) when:
//!
//! - **Loop:** the same move (from `S` to `T`) was made before and the
//!   node's obligations, plan and design content are unchanged since
//!   ([`work_digest`] equals the one recorded then). Sending it back again
//!   would hand the same work the same way.
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
use crate::decisions::{EVIDENCE_KINDS, EvidenceRef};
use crate::interview::short_id;
use crate::lifecycle_reset::VerdictScope;
use crate::node_timeline::{KIND_SEND_BACK, TimelineEntry, TimelineRepo};
use crate::outline::repos::NodeRepo;
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
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_node_send_backs_node ON node_send_backs(node_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_node_send_backs_conversation ON node_send_backs(conversation_id, turn_seq);
";

/// The protocols whose sessions may send a node back.
pub const SENDERS: [ProtocolKind; 5] = [
    ProtocolKind::Phase,
    ProtocolKind::Evaluate,
    ProtocolKind::Verification,
    ProtocolKind::Review,
    ProtocolKind::Fix,
];

/// The error for a session (or none) that may not send a node back.
pub const WHO_MAY: &str = "only a phase agent, an evaluator, or a verification, review or fix session \
    may send a task back (`tod-cli lifecycle send-back`); anyone else asks the user with `tod-cli decisions ask`";

/// What kind of session a protocol's is, in words.
pub fn sender_noun(protocol: &str) -> &'static str {
    match protocol {
        "phase" => "phase agent",
        "evaluate" => "evaluator",
        "verification" => "verification session",
        "review" => "review session",
        "fix" => "fix session",
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
}

impl SendBack {
    pub fn moved(&self) -> bool {
        self.outcome == OUTCOME_MOVED
    }

    /// Who sent it, in words ("the review session").
    pub fn sender(&self) -> &'static str {
        sender_noun(&self.protocol)
    }
}

const COLUMNS: &str = "id, node_id, conversation_id, protocol, turn_seq, from_state, to_state, reason, \
    evidence, obligations, digest, outcome, stop_why, earlier, created_at";

fn map_row(row: &Row<'_>) -> rusqlite::Result<SendBack> {
    let conversation: Option<Vec<u8>> = row.get(2)?;
    let evidence: String = row.get(8)?;
    let obligations: String = row.get(9)?;
    let earlier: String = row.get(13)?;
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
            &format!("INSERT INTO node_send_backs ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"),
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
            ],
        )?;
        Ok(())
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

/// Why a send-back from `from` to `to` with `digest` must stop the run for
/// the user, if it must, with the earlier send-backs it repeats (oldest
/// first). See the module docs for the two rules.
pub fn guard(
    conn: &Connection,
    node: Uuid,
    from: &str,
    to: &str,
    digest: &str,
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
    {
        return Ok(Some((
            format!(
                "it was sent back from {from} to {to} before, and its obligations, plan and design are unchanged \
                 since: sending it back again would hand over the same work the same way"
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

/// Carry out an agent's send-back as `actor`, or record that the guard
/// refused it. Returns `{"outcome": "moved", "from", "to"}` or
/// `{"outcome": "stopped", "why"}`; a refusal is not an error, so its row is
/// kept (the caller tells the agent).
pub fn execute(conn: &Connection, actor: &str, request: &Request) -> Result<Value> {
    let Some(conversation) = ConversationRepo::new(conn).get(request.conversation_id)? else {
        bail!("conversation {} not found: {WHO_MAY}", request.conversation_id);
    };
    let named = crate::conversation::actor_conversation(actor)
        .or_else(|| crate::conversation::agent_actor_conversation(actor));
    if named.is_some_and(|id| id != conversation.id) {
        bail!("conversation {} is not the one writing ({actor})", conversation.id);
    }
    if !SENDERS.contains(&conversation.protocol) {
        bail!("a {} session may not send a task back: {WHO_MAY}", conversation.protocol.as_str());
    }
    let Some(node) = conversation.focus.node_id() else {
        bail!("conversation {} is not about a node", conversation.id);
    };
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
    let verdicts = crate::verification::VerdictRepo::new(conn).latest_for_node(node)?;
    for obligation in &request.obligations {
        let exists = conn
            .prepare("SELECT 1 FROM node_obligations WHERE id = ?1")?
            .exists(params![uuid_to_blob(*obligation)])?;
        if !exists {
            bail!("obligation {} not found", short_id(*obligation));
        }
        if to == "verifying" && !verdicts.get(obligation).is_some_and(|v| v.is_verified()) {
            bail!(
                "obligation {} is not verified on this task, so there is no verdict to reopen",
                short_id(*obligation)
            );
        }
    }
    if to == "verifying" && request.obligations.is_empty() {
        bail!(
            "a send-back to verifying names the obligations whose evidence falls short (--obligation): \
             only their verdicts are reopened"
        );
    }

    let digest = work_digest(conn, node)?;
    let turn_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM conversation_turns WHERE conversation_id = ?1",
        params![uuid_to_blob(conversation.id)],
        |row| row.get(0),
    )?;
    let stop = guard(conn, node, &from, to, &digest)?;
    let mut row = SendBack {
        id: Uuid::now_v7(),
        node_id: node,
        conversation_id: Some(conversation.id),
        protocol: conversation.protocol.as_str().to_string(),
        turn_seq,
        from_state: from.clone(),
        to_state: to.to_string(),
        reason: reason.to_string(),
        evidence: request.evidence.clone(),
        obligations: request.obligations.clone(),
        digest,
        outcome: OUTCOME_MOVED.to_string(),
        stop_why: String::new(),
        earlier: Vec::new(),
        created_at: 0,
    };
    let sender = row.sender();
    if let Some((why, earlier)) = stop {
        row.outcome = OUTCOME_STOPPED.to_string();
        row.stop_why = why.clone();
        row.earlier = earlier.into_iter().map(|s| s.reason).collect();
        row.created_at = now_ms();
        SendBackRepo::new(conn).insert(&row)?;
        let title = format!("The {sender} asked to send it back to {to}; stopped for you");
        TimelineRepo::new(conn).record(&TimelineEntry::new(node, KIND_SEND_BACK, &from, title, true, detail(&row)))?;
        return Ok(json!({ "outcome": OUTCOME_STOPPED, "why": why, "id": row.id }));
    }

    let moved_reason = format!("sent back by the {sender}: {reason}");
    apply_move(
        conn,
        &Move {
            node,
            to,
            by_user: false,
            reason: Some(&moved_reason),
            keep_decision: None,
            verdicts: if to == "verifying" { VerdictScope::Only(&request.obligations) } else { VerdictScope::All },
        },
    )?;
    // At or after the phase change, so the activity shows it heading the new stay.
    row.created_at = now_ms();
    SendBackRepo::new(conn).insert(&row)?;
    let title = format!("Sent back from {from} by the {sender}");
    TimelineRepo::new(conn).record(&TimelineEntry::new(node, KIND_SEND_BACK, to, title, false, detail(&row)))?;
    Ok(json!({ "outcome": OUTCOME_MOVED, "from": from, "to": to, "id": row.id }))
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

/// The section a phase's opening starts with when a send-back brought the
/// node there: what was wrong, the evidence, the state it came from, and
/// which kind of session sent it. Empty when none did.
pub fn render_for_opening(conn: &Connection, node: Uuid) -> Result<String> {
    let Some(sent) = SendBackRepo::new(conn).latest_into_stay(node)? else {
        return Ok(String::new());
    };
    let mut text = format!(
        "## Sent back\n\nThe {} sent this task back from `{}` to `{}`. Start from this cause, not from scratch:\n\n{}\n",
        sent.sender(),
        sent.from_state,
        sent.to_state,
        sent.reason
    );
    if !sent.evidence.is_empty() {
        text.push_str("\nEvidence:\n");
        for evidence in &sent.evidence {
            text.push_str(&format!("- {} {}\n", evidence.kind, evidence.id));
        }
    }
    if !sent.obligations.is_empty() {
        text.push_str("\nObligations named:\n");
        for obligation in &sent.obligations {
            let body: Option<String> = conn
                .query_row(
                    "SELECT body FROM node_obligations WHERE id = ?1",
                    params![uuid_to_blob(*obligation)],
                    |row| row.get(0),
                )
                .optional()?;
            match body {
                Some(body) => text.push_str(&format!("- [{}] {}\n", short_id(*obligation), body)),
                None => text.push_str(&format!("- [{}] (deleted since)\n", short_id(*obligation))),
            }
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests;
