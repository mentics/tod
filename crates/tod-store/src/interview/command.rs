//! Interview writes, each executed by the fleet writer inside one transaction.

use super::repo::{InterviewRepo, short_id};
use super::types::*;
use crate::outline::OutlineMutation;
use crate::outline::repos::{NodeRepo, ObligationRepo};
use crate::outline::types::Capability;
use crate::outline::uuid_blob::{now_ms, uuid_to_blob};
use crate::outline::{KIND_CONSTRAINT, KIND_REQUIREMENT};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum InterviewCommand {
    AddQuestion {
        node_id: Uuid,
        #[serde(default)]
        session_id: Option<Uuid>,
        #[serde(default)]
        phase: Option<String>,
        draft: QuestionDraft,
    },
    WithdrawQuestion {
        node_id: Uuid,
        seq: i64,
        reason: String,
    },
    DeferQuestion {
        node_id: Uuid,
        seq: i64,
    },
    /// Withdraw, as the app, every open question whose proposal targets an
    /// obligation that no longer exists.
    WithdrawStaleProposals {
        node_id: Uuid,
    },
    /// Withdraw every open/deferred question on a node and clear question-maker
    /// exhaustion, so the question maker starts fresh next turn.
    ResetQuestions {
        node_id: Uuid,
        #[serde(default)]
        session_id: Option<Uuid>,
    },
    /// Record the user's answer; option 1 on a question with a proposal
    /// applies that proposal in the same transaction.
    AnswerQuestion {
        node_id: Uuid,
        seq: i64,
        #[serde(default)]
        option: Option<i64>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        edited_text: Option<String>,
    },
    SubmitFreeform {
        node_id: Uuid,
        #[serde(default)]
        session_id: Option<Uuid>,
        phase: String,
        text: String,
    },
    MarkProcessed {
        node_id: Uuid,
        seq: i64,
        summary: String,
    },
    AddMemory {
        node_id: Uuid,
        kind: String,
        #[serde(default)]
        phase: Option<String>,
        body: String,
        #[serde(default)]
        question_seq: Option<i64>,
    },
    UpdateMemory {
        node_id: Uuid,
        seq: i64,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        status: Option<String>,
    },
    /// `reason: None` clears exhaustion.
    SetExhausted {
        session_id: Uuid,
        #[serde(default)]
        reason: Option<String>,
    },
    CreateAgentSession {
        id: Uuid,
        node_id: Uuid,
        #[serde(default)]
        interview_session_id: Option<Uuid>,
        phase: String,
        role: Role,
        lane: i64,
        synced_rev: i64,
        snapshot_tokens: i64,
    },
    RecordAgentTurn {
        id: Uuid,
        #[serde(default)]
        agent_session_id: Option<String>,
        #[serde(default)]
        synced_rev: Option<i64>,
        #[serde(default)]
        est_tokens: Option<i64>,
        #[serde(default)]
        turn_completed: bool,
    },
    RetireAgentSession {
        id: Uuid,
    },
    /// An obligation or content write from an agent, refused when `target`
    /// changed since the acting session's context was built.
    Outline {
        mutation: OutlineMutation,
        #[serde(default)]
        target: Option<Uuid>,
    },

    /// Append a note to a node's notes list.
    AddNote {
        node_id: Uuid,
        text: String,
    },

    // ── Conversation ────────────────────────────────────────────────────
    /// Start a conversation about `focus` with a caller-chosen id.
    CreateConversation {
        id: Uuid,
        focus: crate::conversation::Focus,
        #[serde(default)]
        platform: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        effort: Option<String>,
    },
    /// Append a transcript entry; returns its `seq`.
    AppendConversationTurn {
        conversation_id: Uuid,
        role: crate::conversation::TurnRole,
        #[serde(default)]
        body: String,
        /// An agent reply's streamed parts, when the provider reported them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        parts: Vec<crate::conversation::ReplyPart>,
    },
    /// Record the provider session the conversation continues (`None` clears
    /// it, so the next message starts a fresh one).
    SetConversationSession {
        conversation_id: Uuid,
        #[serde(default)]
        agent_session_id: Option<String>,
        #[serde(default)]
        session_name: Option<String>,
    },
    /// A user edit made from the conversation view: recorded as the user's
    /// action, and it clears the item's unsure flag.
    ConversationEdit {
        conversation_id: Uuid,
        mutation: OutlineMutation,
    },
    /// Flag an item the conversation changed as unsure.
    FlagConversationItem {
        conversation_id: Uuid,
        entity: crate::conversation::Entity,
        entity_id: Uuid,
        reason: String,
    },
    UnflagConversationItem {
        conversation_id: Uuid,
        entity: crate::conversation::Entity,
        entity_id: Uuid,
    },
    /// Reverse actions newest-first, as the user. Applies nothing (and
    /// returns `needs_confirmation`) when an item changed since the
    /// conversation last touched it, unless `force`, or when unselected
    /// actions depend on the selection, unless `include_dependents`.
    ReverseConversationActions {
        conversation_id: Uuid,
        action_ids: Vec<i64>,
        #[serde(default)]
        include_dependents: bool,
        #[serde(default)]
        force: bool,
    },
}

/// Execute `command` as `actor` (`user`, or an interview agent session id).
pub fn execute(
    conn: &Connection,
    media_root: &Path,
    actor: &str,
    command: &InterviewCommand,
) -> Result<Value> {
    let repo = InterviewRepo::new(conn);
    let agent = match Uuid::parse_str(actor) {
        Ok(id) => repo.get_agent_session(id)?,
        Err(_) => None,
    };
    let author = agent
        .as_ref()
        .map(|a| a.role.as_str())
        .unwrap_or(AUTHOR_USER);
    let now = now_ms();

    match command {
        InterviewCommand::AddQuestion {
            node_id,
            session_id,
            phase,
            draft,
        } => {
            if draft.question.trim().is_empty() {
                bail!("question is required");
            }
            let session_id = session_id.or(agent.as_ref().and_then(|a| a.interview_session_id));
            let phase = phase
                .clone()
                .or_else(|| agent.as_ref().map(|a| a.phase.clone()))
                .context("phase is required")?;
            check_phase(&phase)?;
            let proposal = draft
                .proposal
                .clone()
                .map(|p| normalize_proposal(&repo, &phase, p))
                .transpose()?;
            let seq = next_seq(conn, "interview_questions", *node_id)?;
            let id = Uuid::new_v4();
            conn.execute(
                "INSERT INTO interview_questions
                 (id, node_id, session_id, seq, phase, author, status, covers, context, question,
                  intent, recommend, options, proposal, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
                params![
                    uuid_to_blob(id),
                    uuid_to_blob(*node_id),
                    session_id.map(uuid_to_blob),
                    seq,
                    phase,
                    author,
                    serde_json::to_string(&draft.covers)?,
                    nonempty(&draft.context),
                    draft.question.trim(),
                    nonempty(&draft.intent),
                    nonempty(&draft.recommend),
                    serde_json::to_string(&draft.options)?,
                    proposal.as_ref().map(serde_json::to_string).transpose()?,
                    now,
                ],
            )?;
            Ok(json!({ "id": format!("q-{seq}") }))
        }

        InterviewCommand::WithdrawQuestion {
            node_id,
            seq,
            reason,
        } => {
            let q = question(&repo, *node_id, *seq)?;
            if q.status != STATUS_OPEN && q.status != STATUS_DEFERRED {
                bail!("{} is already {}", q.label(), q.status);
            }
            guard(&repo, agent.as_ref(), actor, q.id, || question_brief(&q))?;
            conn.execute(
                "UPDATE interview_questions
                 SET status = 'withdrawn', withdrawn_by = ?1, withdrawn_reason = ?2, updated_at = ?3
                 WHERE id = ?4",
                params![author, reason.trim(), now, uuid_to_blob(q.id)],
            )?;
            Ok(json!({ "id": q.label() }))
        }

        InterviewCommand::DeferQuestion { node_id, seq } => {
            let q = question(&repo, *node_id, *seq)?;
            if q.status != STATUS_OPEN {
                bail!("{} is {}", q.label(), q.status);
            }
            conn.execute(
                "UPDATE interview_questions SET status = 'deferred', updated_at = ?1 WHERE id = ?2",
                params![now, uuid_to_blob(q.id)],
            )?;
            Ok(json!({ "id": q.label() }))
        }

        InterviewCommand::WithdrawStaleProposals { node_id } => {
            let stale = repo.stale_proposal_questions(*node_id)?;
            for seq in &stale {
                conn.execute(
                    "UPDATE interview_questions
                     SET status = 'withdrawn', withdrawn_by = NULL, withdrawn_reason = ?1, updated_at = ?2
                     WHERE node_id = ?3 AND seq = ?4",
                    params![STALE_PROPOSAL_REASON, now, uuid_to_blob(*node_id), seq],
                )?;
            }
            let labels: Vec<String> = stale.iter().map(|seq| format!("q-{seq}")).collect();
            Ok(json!({ "withdrawn": labels }))
        }

        InterviewCommand::ResetQuestions {
            node_id,
            session_id,
        } => {
            let open = repo.list_questions(*node_id, &[STATUS_OPEN, STATUS_DEFERRED])?;
            for q in &open {
                conn.execute(
                    "UPDATE interview_questions
                     SET status = 'withdrawn', withdrawn_by = NULL, withdrawn_reason = ?1, updated_at = ?2
                     WHERE id = ?3",
                    params![RESET_QUESTIONS_REASON, now, uuid_to_blob(q.id)],
                )?;
            }
            if let Some(session_id) = session_id {
                conn.execute(
                    "UPDATE interview_sessions
                     SET question_maker_state = ?1, exhausted_reason = NULL, updated_at = ?2 WHERE id = ?3",
                    params![QUESTION_MAKER_IDLE, now, uuid_to_blob(*session_id)],
                )?;
            }
            let labels: Vec<String> = open.iter().map(|q| q.label()).collect();
            Ok(json!({ "withdrawn": labels }))
        }

        InterviewCommand::AnswerQuestion {
            node_id,
            seq,
            option,
            text,
            edited_text,
        } => {
            let q = question(&repo, *node_id, *seq)?;
            if q.status != STATUS_OPEN && q.status != STATUS_DEFERRED {
                bail!("{} is {}", q.label(), q.status);
            }
            if let Some(option) = option {
                let max = q.options.len().max(usize::from(q.proposal.is_some())) as i64;
                if *option < 1 || *option > max {
                    bail!("{} has no option {option}", q.label());
                }
            }
            let text = nonempty(text);
            let edited = nonempty(edited_text);
            if option.is_none() && text.is_none() && edited.is_none() {
                bail!("an answer needs an option or text");
            }
            let applied = match (&q.proposal, option) {
                (Some(proposal), Some(1)) => Some(apply_proposal(
                    conn,
                    media_root,
                    &q,
                    proposal,
                    edited.as_deref(),
                )?),
                _ => None,
            };
            conn.execute(
                "UPDATE interview_questions
                 SET status = 'answered', answer_option = ?1, answer_text = ?2,
                     answer_edited_text = ?3, applied = ?4, answered_at = ?5, updated_at = ?5
                 WHERE id = ?6",
                params![
                    option,
                    text,
                    edited,
                    applied.as_ref().map(Value::to_string),
                    now,
                    uuid_to_blob(q.id),
                ],
            )?;
            Ok(json!({ "id": q.label(), "applied": applied }))
        }

        InterviewCommand::SubmitFreeform {
            node_id,
            session_id,
            phase,
            text,
        } => {
            check_phase(phase)?;
            if text.trim().is_empty() {
                bail!("text is required");
            }
            let seq = next_seq(conn, "interview_questions", *node_id)?;
            conn.execute(
                "INSERT INTO interview_questions
                 (id, node_id, session_id, seq, phase, author, status, answer_text, answered_at,
                  created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'user', 'answered', ?6, ?7, ?7, ?7)",
                params![
                    uuid_to_blob(Uuid::new_v4()),
                    uuid_to_blob(*node_id),
                    session_id.map(uuid_to_blob),
                    seq,
                    phase,
                    text.trim(),
                    now,
                ],
            )?;
            Ok(json!({ "id": format!("q-{seq}") }))
        }

        InterviewCommand::MarkProcessed {
            node_id,
            seq,
            summary,
        } => {
            let q = question(&repo, *node_id, *seq)?;
            if q.status != STATUS_ANSWERED {
                bail!("{} is {}, not answered", q.label(), q.status);
            }
            conn.execute(
                "UPDATE interview_questions
                 SET processed_at = ?1, processed_summary = ?2, updated_at = ?1 WHERE id = ?3",
                params![now, summary.trim(), uuid_to_blob(q.id)],
            )?;
            Ok(json!({ "id": q.label() }))
        }

        InterviewCommand::AddMemory {
            node_id,
            kind,
            phase,
            body,
            question_seq,
        } => {
            if !MEMORY_KINDS.contains(&kind.as_str()) {
                bail!("unknown memory kind `{kind}` (expected context|handoff|parked|plan)");
            }
            if body.trim().is_empty() {
                bail!("body is required");
            }
            let phase = phase
                .clone()
                .or_else(|| agent.as_ref().map(|a| a.phase.clone()));
            if let Some(phase) = &phase {
                check_phase(phase)?;
            }
            if kind == MEMORY_PARKED && phase.is_none() {
                bail!("parked memory needs --phase");
            }
            if kind == MEMORY_PLAN {
                conn.execute(
                    "UPDATE interview_memory SET status = 'done', updated_at = ?1
                     WHERE node_id = ?2 AND kind = 'plan' AND status = 'open' AND phase IS ?3",
                    params![now, uuid_to_blob(*node_id), phase],
                )?;
            }
            let seq = next_seq(conn, "interview_memory", *node_id)?;
            conn.execute(
                "INSERT INTO interview_memory
                 (id, node_id, seq, kind, phase, status, author, question_seq, body, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?8, ?9, ?9)",
                params![
                    uuid_to_blob(Uuid::new_v4()),
                    uuid_to_blob(*node_id),
                    seq,
                    kind,
                    phase,
                    author,
                    question_seq,
                    body.trim(),
                    now,
                ],
            )?;
            Ok(json!({ "id": format!("m-{seq}") }))
        }

        InterviewCommand::UpdateMemory {
            node_id,
            seq,
            body,
            status,
        } => {
            let note = repo
                .get_memory(*node_id, *seq)?
                .with_context(|| format!("m-{seq} not found"))?;
            if let Some(status) = status {
                if status != MEMORY_OPEN && status != MEMORY_DONE {
                    bail!("status must be open or done");
                }
            }
            if body.as_deref().is_some_and(|b| b.trim().is_empty()) {
                bail!("body cannot be empty");
            }
            guard(&repo, agent.as_ref(), actor, note.id, || {
                Ok(format!(
                    "{} {} ({}): {}",
                    note.label(),
                    note.kind,
                    note.status,
                    note.body
                ))
            })?;
            conn.execute(
                "UPDATE interview_memory
                 SET body = COALESCE(?1, body), status = COALESCE(?2, status), updated_at = ?3
                 WHERE id = ?4",
                params![
                    body.as_deref().map(str::trim),
                    status,
                    now,
                    uuid_to_blob(note.id)
                ],
            )?;
            Ok(json!({ "id": note.label() }))
        }

        InterviewCommand::SetExhausted { session_id, reason } => {
            let state = if reason.is_some() {
                QUESTION_MAKER_EXHAUSTED
            } else {
                QUESTION_MAKER_IDLE
            };
            // Enforced structurally, not just by the question-maker prompt's own
            // "no open handoffs" rule — an agent turn that skips the "close
            // handoffs" step and declares exhaustion anyway would otherwise
            // strand a gate-check failure with nothing left to wake the
            // question maker again, regardless of how carefully the prompt
            // says not to.
            if reason.is_some() {
                let node_id: Option<Vec<u8>> = conn
                    .query_row(
                        "SELECT node_id FROM interview_sessions WHERE id = ?1",
                        params![uuid_to_blob(*session_id)],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(node_blob) = node_id {
                    let node_id = crate::outline::uuid_blob::blob_to_uuid_sql(&node_blob)?;
                    let open_handoffs = InterviewRepo::new(conn).list_memory(
                        node_id,
                        Some(MEMORY_HANDOFF),
                        Some(MEMORY_OPEN),
                    )?;
                    if !open_handoffs.is_empty() {
                        bail!(
                            "cannot declare exhaustion with {} open handoff note(s) — close them first with `memory update --status done`, or address what they ask for",
                            open_handoffs.len()
                        );
                    }
                }
            }
            let n = conn.execute(
                "UPDATE interview_sessions
                 SET question_maker_state = ?1, exhausted_reason = ?2, updated_at = ?3 WHERE id = ?4",
                params![state, reason, now, uuid_to_blob(*session_id)],
            )?;
            if n == 0 {
                bail!("interview session {session_id} not found");
            }
            Ok(json!({ "state": state }))
        }

        InterviewCommand::CreateAgentSession {
            id,
            node_id,
            interview_session_id,
            phase,
            role,
            lane,
            synced_rev,
            snapshot_tokens,
        } => {
            conn.execute(
                "INSERT INTO interview_agent_sessions
                 (id, node_id, interview_session_id, phase, role, lane, synced_rev, est_tokens,
                  snapshot_tokens, turns, state, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, 0, 'live', ?9)",
                params![
                    uuid_to_blob(*id),
                    uuid_to_blob(*node_id),
                    interview_session_id.map(uuid_to_blob),
                    phase,
                    role.as_str(),
                    lane,
                    synced_rev,
                    snapshot_tokens,
                    now,
                ],
            )?;
            Ok(json!({}))
        }

        InterviewCommand::RecordAgentTurn {
            id,
            agent_session_id,
            synced_rev,
            est_tokens,
            turn_completed,
        } => {
            conn.execute(
                "UPDATE interview_agent_sessions
                 SET agent_session_id = COALESCE(?1, agent_session_id),
                     synced_rev = COALESCE(?2, synced_rev),
                     est_tokens = COALESCE(?3, est_tokens),
                     turns = turns + ?4,
                     last_turn_at = CASE WHEN ?4 = 1 THEN ?5 ELSE last_turn_at END
                 WHERE id = ?6",
                params![
                    agent_session_id,
                    synced_rev,
                    est_tokens,
                    i64::from(*turn_completed),
                    now,
                    uuid_to_blob(*id),
                ],
            )?;
            if synced_rev.is_some() {
                // Advancing a watermark can free rows too; don't wait for a
                // session to retire before shrinking the log.
                trim_changes(conn, now)?;
            }
            Ok(json!({}))
        }

        InterviewCommand::RetireAgentSession { id } => {
            conn.execute(
                "UPDATE interview_agent_sessions SET state = 'retired' WHERE id = ?1",
                params![uuid_to_blob(*id)],
            )?;
            trim_changes(conn, now)?;
            Ok(json!({}))
        }

        InterviewCommand::Outline { mutation, target } => {
            if let Some(target) = target {
                guard(&repo, agent.as_ref(), actor, *target, || {
                    if let Some(o) = ObligationRepo::new(conn).get(*target)? {
                        return Ok(format!(
                            "{} {}{}: {}",
                            short_id(o.id),
                            o.kind,
                            o.section.map(|s| format!(" ({s})")).unwrap_or_default(),
                            o.body
                        ));
                    }
                    if let Some(s) = crate::outline::repos::PlanStepRepo::new(conn).get(*target)? {
                        return Ok(format!("{} {}: {}", short_id(s.id), s.status, s.body));
                    }
                    Ok(format!("{} was deleted", short_id(*target)))
                })?;
            }
            if actor != ACTOR_USER {
                crate::outline::check_references(conn, mutation)?;
            }
            if let Some(prefixed) = actor.strip_prefix(crate::conversation::ACTOR_PREFIX) {
                // A conversation's agent: record the write in its action log.
                let conversation_id = crate::conversation::actor_conversation(actor)
                    .with_context(|| format!("bad conversation actor id `{prefixed}`"))?;
                let conversations = crate::conversation::ConversationRepo::new(conn);
                if conversations.get(conversation_id)?.is_none() {
                    bail!("conversation {conversation_id} not found");
                }
                let turn_seq = conversations.max_user_seq(conversation_id)?;
                let action = crate::conversation::record_and_execute(
                    conn,
                    conversation_id,
                    crate::conversation::ActionActor::Agent,
                    turn_seq,
                    mutation.clone(),
                    media_root,
                )?;
                return Ok(json!({ "action": action }));
            }
            // An agent-driven obligation write always uses its own session's
            // phase — it cannot claim a different one via the CLI arg.
            let mut owned;
            let mutation = if let (OutlineMutation::CreateObligation { .. }, Some(agent)) =
                (mutation, agent.as_ref())
            {
                owned = mutation.clone();
                if let OutlineMutation::CreateObligation { phase, .. } = &mut owned {
                    *phase = agent.phase.clone();
                }
                &owned
            } else {
                mutation
            };
            mutation.execute(conn, media_root)?;
            Ok(json!({}))
        }

        InterviewCommand::CreateConversation {
            id,
            focus,
            platform,
            model,
            effort,
        } => {
            let conversation = crate::conversation::ConversationRepo::new(conn).create_with_id(
                *id,
                *focus,
                platform.as_deref(),
                model.as_deref(),
                effort.as_deref(),
            )?;
            Ok(json!({ "id": conversation.id.to_string() }))
        }
        InterviewCommand::AppendConversationTurn {
            conversation_id,
            role,
            body,
            parts,
        } => {
            let repo = crate::conversation::ConversationRepo::new(conn);
            if repo.get(*conversation_id)?.is_none() {
                bail!("conversation {conversation_id} not found");
            }
            let turn = repo.append_turn_with_parts(*conversation_id, *role, body, parts)?;
            Ok(json!({ "seq": turn.seq }))
        }
        InterviewCommand::SetConversationSession {
            conversation_id,
            agent_session_id,
            session_name,
        } => {
            crate::conversation::ConversationRepo::new(conn).set_agent_session(
                *conversation_id,
                agent_session_id.as_deref(),
                session_name.as_deref(),
            )?;
            Ok(json!({}))
        }
        InterviewCommand::ConversationEdit {
            conversation_id,
            mutation,
        } => {
            let action = crate::conversation::apply_user_edit(
                conn,
                *conversation_id,
                mutation.clone(),
                media_root,
            )?;
            Ok(json!({ "action": action }))
        }
        InterviewCommand::FlagConversationItem {
            conversation_id,
            entity,
            entity_id,
            reason,
        } => {
            crate::conversation::flag_item(conn, *conversation_id, *entity, *entity_id, reason)?;
            Ok(json!({}))
        }
        InterviewCommand::UnflagConversationItem {
            conversation_id,
            entity,
            entity_id,
        } => {
            crate::conversation::clear_flag(conn, *conversation_id, *entity, *entity_id)?;
            Ok(json!({}))
        }
        InterviewCommand::ReverseConversationActions {
            conversation_id,
            action_ids,
            include_dependents,
            force,
        } => {
            let outcome = crate::conversation::reverse_actions(
                conn,
                *conversation_id,
                action_ids,
                *include_dependents,
                *force,
                media_root,
            )?;
            Ok(serde_json::to_value(outcome)?)
        }
        InterviewCommand::AddNote { node_id, text } => {
            let text = text.trim();
            if text.is_empty() {
                bail!("note text is required");
            }
            let note = crate::fleet::repos::task::TaskRepo::new(conn).append_note(*node_id, text)?;
            Ok(json!({ "id": note.id.to_string() }))
        }
    }
}

/// Drop change-log rows no live agent session can still need — everything at
/// or below the oldest live watermark — except rows holding an obligation's
/// prior row, which stay restorable for [`OBLIGATION_SNAPSHOT_RETENTION_MS`].
pub fn trim_changes(conn: &Connection, now: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM interview_changes
         WHERE rev <= COALESCE(
                (SELECT MIN(synced_rev) FROM interview_agent_sessions WHERE state = 'live'),
                (SELECT COALESCE(MAX(rev), 0) FROM interview_changes))
           AND (prior IS NULL OR at < ?1)",
        params![now - OBLIGATION_SNAPSHOT_RETENTION_MS],
    )?;
    Ok(())
}

fn question(repo: &InterviewRepo<'_>, node_id: Uuid, seq: i64) -> Result<InterviewQuestion> {
    repo.get_question(node_id, seq)?
        .with_context(|| format!("q-{seq} not found on this node"))
}

fn question_brief(q: &InterviewQuestion) -> Result<String> {
    Ok(format!(
        "{} {}: {}",
        q.label(),
        q.status,
        q.question.as_deref().unwrap_or("(freeform)")
    ))
}

/// Refuse a write when an agent session is acting on something someone else
/// changed after that session's context was built.
fn guard(
    repo: &InterviewRepo<'_>,
    agent: Option<&AgentSessionRow>,
    actor: &str,
    entity_id: Uuid,
    current: impl FnOnce() -> Result<String>,
) -> Result<()> {
    if let Some(agent) = agent {
        if repo.changed_by_other_since(entity_id, agent.synced_rev, actor)? {
            bail!(
                "conflict: this changed after your context was built. Current: {}",
                current()?
            );
        }
    }
    Ok(())
}

fn check_phase(phase: &str) -> Result<()> {
    if !PHASES.contains(&phase) {
        bail!("unknown phase `{phase}` (expected requirements|design|planning)");
    }
    Ok(())
}

fn nonempty(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn next_seq(conn: &Connection, table: &str, node_id: Uuid) -> Result<i64> {
    Ok(conn.query_row(
        &format!("SELECT COALESCE(MAX(seq), 0) + 1 FROM {table} WHERE node_id = ?1"),
        params![uuid_to_blob(node_id)],
        |r| r.get(0),
    )?)
}

/// Check a proposal's shape and expand obligation id prefixes to full ids.
fn normalize_proposal(repo: &InterviewRepo<'_>, phase: &str, mut p: Proposal) -> Result<Proposal> {
    if phase == PHASE_PLANNING {
        bail!(
            "planning-phase questions can't carry a proposal — plan steps go through \
             `tod-cli plan add`, and obligation changes (rare during planning) go through \
             `tod-cli obligations` directly, outside the question/answer flow"
        );
    }
    let has_text = p.text.as_deref().is_some_and(|t| !t.trim().is_empty());
    match p.op {
        ProposalOp::Add => {
            let kind = p.kind.as_deref().unwrap_or_default();
            if kind != KIND_REQUIREMENT && kind != KIND_CONSTRAINT {
                bail!("proposal add needs kind: requirement|constraint");
            }
            if !has_text {
                bail!("proposal add needs text");
            }
        }
        ProposalOp::Update | ProposalOp::Delete => {
            let raw =
                p.id.as_deref()
                    .context("proposal needs the obligation id")?;
            p.id = Some(repo.resolve_obligation_id(raw)?.to_string());
            if p.op == ProposalOp::Update && !has_text {
                bail!("proposal update needs text");
            }
        }
        ProposalOp::Content => {
            let ty = p.content_type.as_deref().unwrap_or_default();
            if !["details", "design", "plan"].contains(&ty) {
                bail!("proposal content needs type: details|design|plan");
            }
            if !has_text {
                bail!("proposal content needs text");
            }
        }
    }
    p.replaces = p
        .replaces
        .iter()
        .map(|raw| repo.resolve_obligation_id(raw).map(|id| id.to_string()))
        .collect::<Result<_>>()?;
    Ok(p)
}

/// Apply an accepted proposal. Validation problems are recorded as
/// `{"error": …}` with nothing applied, so the answer still lands and the
/// answer processor can work out what the user meant.
fn apply_proposal(
    conn: &Connection,
    media_root: &Path,
    q: &InterviewQuestion,
    p: &Proposal,
    edited_text: Option<&str>,
) -> Result<Value> {
    let text = edited_text
        .or(p.text.as_deref())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let (mutations, ops) = match plan_proposal(conn, q, p, &text) {
        Ok(plan) => plan,
        Err(message) => return Ok(json!({ "error": message })),
    };
    for mutation in &mutations {
        mutation.execute(conn, media_root)?;
    }
    Ok(json!({ "ops": ops }))
}

fn plan_proposal(
    conn: &Connection,
    q: &InterviewQuestion,
    p: &Proposal,
    text: &str,
) -> std::result::Result<(Vec<OutlineMutation>, Vec<Value>), String> {
    let obligations = ObligationRepo::new(conn);
    let nodes = NodeRepo::new(conn);
    let existing = |raw: &str| -> std::result::Result<Uuid, String> {
        let id = Uuid::parse_str(raw).map_err(|_| format!("bad obligation id {raw}"))?;
        match obligations.get(id) {
            Ok(Some(_)) => Ok(id),
            Ok(None) => Err(format!("obligation {} no longer exists", short_id(id))),
            Err(err) => Err(err.to_string()),
        }
    };
    let require_spec = |node: Uuid| -> std::result::Result<(), String> {
        match nodes.list_capabilities(node) {
            Ok(caps) if caps.contains(&Capability::Spec) => Ok(()),
            Ok(_) => Err("the target node has no Spec capability".into()),
            Err(err) => Err(err.to_string()),
        }
    };
    let need_text = || {
        if text.is_empty() {
            Err("the proposal text is empty".to_string())
        } else {
            Ok(())
        }
    };

    let mut mutations = Vec::new();
    let mut ops = Vec::new();
    let mut primary: Option<Uuid> = None;
    match p.op {
        ProposalOp::Add => {
            need_text()?;
            let node = p.node.unwrap_or(q.node_id);
            require_spec(node)?;
            let id = Uuid::new_v4();
            mutations.push(OutlineMutation::CreateObligation {
                obligation_id: Some(id),
                node_id: node,
                kind: p.kind.clone().unwrap_or_default(),
                after_id: None,
                before: false,
                section: p.section.clone().filter(|s| !s.trim().is_empty()),
                body: text.to_string(),
                // The question this proposal answers already carries the
                // phase it was asked in — reuse it rather than trusting an
                // unauthenticated phase from the proposal itself.
                phase: q.phase.clone(),
            });
            ops.push(json!({ "op": "add", "id": id }));
        }
        ProposalOp::Update => {
            need_text()?;
            let id = existing(p.id.as_deref().unwrap_or_default())?;
            primary = Some(id);
            mutations.push(OutlineMutation::UpdateObligationBody {
                obligation_id: id,
                body: text.to_string(),
            });
            if let Some(section) = &p.section {
                mutations.push(OutlineMutation::UpdateObligationSection {
                    obligation_id: id,
                    section: Some(section.clone()).filter(|s| !s.trim().is_empty()),
                });
            }
            ops.push(json!({ "op": "update", "id": id }));
        }
        ProposalOp::Delete => {
            let id = existing(p.id.as_deref().unwrap_or_default())?;
            primary = Some(id);
            mutations.push(OutlineMutation::DeleteObligation { obligation_id: id });
            ops.push(json!({ "op": "delete", "id": id }));
        }
        ProposalOp::Content => {
            need_text()?;
            require_spec(q.node_id)?;
            let content_type = p.content_type.clone().unwrap_or_default();
            let body = if p.append {
                let current = nodes
                    .get_extra_content(q.node_id, &content_type)
                    .map_err(|e| e.to_string())?
                    .unwrap_or_default();
                if current.trim().is_empty() {
                    text.to_string()
                } else {
                    format!("{}\n\n{text}", current.trim_end())
                }
            } else {
                text.to_string()
            };
            mutations.push(OutlineMutation::SetExtraContent {
                node_id: q.node_id,
                content_type: content_type.clone(),
                body,
            });
            ops.push(json!({ "op": "content", "type": content_type }));
        }
    }
    for raw in &p.replaces {
        let id = existing(raw)?;
        if Some(id) == primary {
            continue;
        }
        mutations.push(OutlineMutation::DeleteObligation { obligation_id: id });
        ops.push(json!({ "op": "delete", "id": id }));
    }
    Ok((mutations, ops))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::{ListRepo, OutlineRepo};
    use crate::outline::types::{OutlineEntry, OutlineList};

    fn setup() -> (std::path::PathBuf, Connection, Uuid, Uuid) {
        let dir = std::env::temp_dir().join(format!("tod-interview-cmd-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let list_id = Uuid::new_v4();
        ListRepo::new(&conn)
            .insert(&OutlineList {
                id: list_id,
                slug: "t".into(),
                title: "T".into(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
            .unwrap();
        let node = NodeRepo::new(&conn).create_normal("n", "Node").unwrap();
        NodeRepo::new(&conn)
            .enable_capability(node.id, Capability::Spec)
            .unwrap();
        OutlineRepo::new(&conn)
            .insert(&OutlineEntry {
                node_id: node.id,
                list_id,
                parent_id: None,
                ordinal: 0,
                collapsed: false,
            })
            .unwrap();
        let session = Uuid::new_v4();
        conn.execute(
            "INSERT INTO interview_sessions (id, node_id, display_name, status, phase, created_at, updated_at)
             VALUES (?1, ?2, 'S', 'active', 'task-requirements-interview', 0, 0)",
            params![uuid_to_blob(session), uuid_to_blob(node.id)],
        )
        .unwrap();
        (dir, conn, node.id, session)
    }

    fn run(conn: &Connection, actor: &str, cmd: InterviewCommand) -> Result<Value> {
        let tx = conn.unchecked_transaction().unwrap();
        conn.execute(
            "UPDATE interview_actor SET actor = ?1 WHERE id = 1",
            [actor],
        )
        .unwrap();
        let out = execute(conn, Path::new("."), actor, &cmd);
        conn.execute("UPDATE interview_actor SET actor = 'user' WHERE id = 1", [])
            .unwrap();
        tx.commit().unwrap();
        out
    }

    fn draft(question: &str, proposal: Option<Proposal>) -> QuestionDraft {
        QuestionDraft {
            question: question.into(),
            options: vec!["Yes".into(), "No".into()],
            proposal,
            ..Default::default()
        }
    }

    #[test]
    fn accepting_a_proposal_applies_it_and_replaces_superseded_obligations() {
        let (dir, conn, node, session) = setup();
        let old = Uuid::new_v4();
        ObligationRepo::new(&conn)
            .insert_at(
                old,
                node,
                KIND_REQUIREMENT,
                0,
                None,
                "Notes reset each session.",
                PHASE_REQUIREMENTS,
            )
            .unwrap();
        let proposal = Proposal {
            op: ProposalOp::Add,
            kind: Some(KIND_REQUIREMENT.into()),
            section: Some("Persistence".into()),
            node: None,
            id: None,
            content_type: None,
            text: Some("Notes persist per node.".into()),
            append: false,
            replaces: vec![short_id(old)],
        };
        let added = run(
            &conn,
            ACTOR_USER,
            InterviewCommand::AddQuestion {
                node_id: node,
                session_id: Some(session),
                phase: Some(PHASE_REQUIREMENTS.into()),
                draft: draft("Keep notes?", Some(proposal)),
            },
        )
        .unwrap();
        assert_eq!(added["id"], "q-1");

        let answered = run(
            &conn,
            ACTOR_USER,
            InterviewCommand::AnswerQuestion {
                node_id: node,
                seq: 1,
                option: Some(1),
                text: None,
                edited_text: Some("Notes persist per node across restarts.".into()),
            },
        )
        .unwrap();
        assert_eq!(answered["applied"]["ops"].as_array().unwrap().len(), 2);
        let rows = ObligationRepo::new(&conn).list_for_node(node).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].body, "Notes persist per node across restarts.");
        assert_eq!(rows[0].section.as_deref(), Some("Persistence"));

        let repo = InterviewRepo::new(&conn);
        let q = repo.get_question(node, 1).unwrap().unwrap();
        assert_eq!(q.status, STATUS_ANSWERED);
        assert_eq!(repo.unprocessed_answers(node).unwrap().len(), 1);
        let changes = repo.changes_since(&[node], 0, "nobody").unwrap();
        assert!(
            changes
                .iter()
                .any(|c| c.entity == ENTITY_OBLIGATION && c.op == "delete")
        );
        assert!(
            changes
                .iter()
                .any(|c| c.entity == ENTITY_QUESTION && c.fields.contains(&"status".to_string()))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn agent_session(conn: &Connection, node: Uuid, session: Uuid) -> Uuid {
        let agent = Uuid::new_v4();
        let head = InterviewRepo::new(conn).head_rev().unwrap();
        run(
            conn,
            ACTOR_USER,
            InterviewCommand::CreateAgentSession {
                id: agent,
                node_id: node,
                interview_session_id: Some(session),
                phase: PHASE_DESIGN.into(),
                role: Role::Drafter,
                lane: 0,
                synced_rev: head,
                snapshot_tokens: 0,
            },
        )
        .unwrap();
        agent
    }

    fn restore(conn: &Connection, rev: i64) -> Result<Value> {
        run(
            conn,
            ACTOR_USER,
            InterviewCommand::Outline {
                mutation: OutlineMutation::RestoreObligation { rev },
                target: None,
            },
        )
    }

    #[test]
    fn deleted_obligations_survive_trimming_and_restore_in_place() {
        let (dir, conn, node, session) = setup();
        let ids: Vec<Uuid> = ["First.", "Second.", "Third."]
            .iter()
            .enumerate()
            .map(|(i, body)| {
                let id = Uuid::new_v4();
                ObligationRepo::new(&conn)
                    .insert_at(
                        id,
                        node,
                        KIND_REQUIREMENT,
                        i,
                        Some("Core"),
                        body,
                        PHASE_DESIGN,
                    )
                    .unwrap();
                id
            })
            .collect();

        // An agent deletes the first two, top-down.
        let agent = agent_session(&conn, node, session);
        for id in &ids[..2] {
            run(
                &conn,
                &agent.to_string(),
                InterviewCommand::Outline {
                    mutation: OutlineMutation::DeleteObligation { obligation_id: *id },
                    target: Some(*id),
                },
            )
            .unwrap();
        }
        let repo = InterviewRepo::new(&conn);
        let deleted = repo.deleted_obligations(node).unwrap();
        assert_eq!(
            deleted.iter().map(|s| s.obligation_id).collect::<Vec<_>>(),
            vec![ids[1], ids[0]]
        );
        assert_eq!(deleted[0].actor, agent.to_string());
        assert_eq!(deleted[0].prior.body, "Second.");

        // Retiring the agent trims the log, but not the deleted rows.
        run(
            &conn,
            ACTOR_USER,
            InterviewCommand::RetireAgentSession { id: agent },
        )
        .unwrap();
        let plain: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM interview_changes WHERE prior IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(plain, 0);
        assert_eq!(repo.deleted_obligations(node).unwrap(), deleted);

        // Newest first puts them back in their original order.
        for snapshot in &deleted {
            restore(&conn, snapshot.rev).unwrap();
        }
        let rows = ObligationRepo::new(&conn).list_for_node(node).unwrap();
        assert_eq!(rows.iter().map(|o| o.id).collect::<Vec<_>>(), ids);
        assert_eq!(rows[1].body, "Second.");
        assert_eq!(rows[1].section.as_deref(), Some("Core"));
        assert_eq!(rows[1].phase, PHASE_DESIGN);
        assert!(repo.deleted_obligations(node).unwrap().is_empty());

        let err = restore(&conn, deleted[0].rev).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_edit_restores_the_earlier_wording_and_the_restore_is_itself_restorable() {
        let (dir, conn, node, session) = setup();
        let id = Uuid::new_v4();
        ObligationRepo::new(&conn)
            .insert_at(
                id,
                node,
                KIND_CONSTRAINT,
                0,
                None,
                "The user's wording.",
                PHASE_REQUIREMENTS,
            )
            .unwrap();
        let agent = agent_session(&conn, node, session);
        run(
            &conn,
            &agent.to_string(),
            InterviewCommand::Outline {
                mutation: OutlineMutation::UpdateObligationBody {
                    obligation_id: id,
                    body: "The agent's wording.".into(),
                },
                target: Some(id),
            },
        )
        .unwrap();
        let repo = InterviewRepo::new(&conn);
        let history = repo.obligation_history(id).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].op, "update");
        assert_eq!(history[0].prior.body, "The user's wording.");

        restore(&conn, history[0].rev).unwrap();
        let row = ObligationRepo::new(&conn).get(id).unwrap().unwrap();
        assert_eq!(row.body, "The user's wording.");

        let history = repo.obligation_history(id).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].prior.body, "The agent's wording.");

        // An edit can't be restored onto an obligation that is gone.
        ObligationRepo::new(&conn).delete(id).unwrap();
        let err = restore(&conn, history[1].rev).unwrap_err();
        assert!(
            err.to_string().contains("restore its deletion first"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn deleted_obligations_past_retention_are_trimmed() {
        let (dir, conn, node, _session) = setup();
        let id = Uuid::new_v4();
        let obligations = ObligationRepo::new(&conn);
        obligations
            .insert_at(
                id,
                node,
                KIND_REQUIREMENT,
                0,
                None,
                "Old.",
                PHASE_REQUIREMENTS,
            )
            .unwrap();
        obligations.delete(id).unwrap();
        let repo = InterviewRepo::new(&conn);
        let rev = repo.deleted_obligations(node).unwrap()[0].rev;

        trim_changes(&conn, now_ms()).unwrap();
        assert!(repo.obligation_snapshot(rev).unwrap().is_some());

        trim_changes(&conn, now_ms() + OBLIGATION_SNAPSHOT_RETENTION_MS + 1).unwrap();
        assert!(repo.obligation_snapshot(rev).unwrap().is_none());
        let err = restore(&conn, rev).unwrap_err();
        assert!(err.to_string().contains("past retention"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn questions_whose_proposal_target_is_gone_are_withdrawn_as_stale() {
        let (dir, conn, node, session) = setup();
        let target = Uuid::new_v4();
        let unrelated = Uuid::new_v4();
        let obligations = ObligationRepo::new(&conn);
        obligations
            .insert_at(
                target,
                node,
                KIND_REQUIREMENT,
                0,
                None,
                "Old wording.",
                PHASE_REQUIREMENTS,
            )
            .unwrap();
        obligations
            .insert_at(
                unrelated,
                node,
                KIND_REQUIREMENT,
                1,
                None,
                "Still here.",
                PHASE_REQUIREMENTS,
            )
            .unwrap();
        let update = |id: Uuid| Proposal {
            op: ProposalOp::Update,
            kind: None,
            section: None,
            node: None,
            id: Some(short_id(id)),
            content_type: None,
            text: Some("New wording.".into()),
            append: false,
            replaces: Vec::new(),
        };
        for (question, proposal) in [
            ("Reword it?", update(target)),
            ("Reword the other?", update(unrelated)),
        ] {
            run(
                &conn,
                ACTOR_USER,
                InterviewCommand::AddQuestion {
                    node_id: node,
                    session_id: Some(session),
                    phase: Some(PHASE_REQUIREMENTS.into()),
                    draft: draft(question, Some(proposal)),
                },
            )
            .unwrap();
        }
        let repo = InterviewRepo::new(&conn);
        assert!(repo.stale_proposal_questions(node).unwrap().is_empty());

        ObligationRepo::new(&conn).delete(target).unwrap();
        assert_eq!(repo.stale_proposal_questions(node).unwrap(), vec![1]);
        let out = run(
            &conn,
            ACTOR_USER,
            InterviewCommand::WithdrawStaleProposals { node_id: node },
        )
        .unwrap();
        assert_eq!(out["withdrawn"], json!(["q-1"]));
        let q1 = repo.get_question(node, 1).unwrap().unwrap();
        assert_eq!(q1.status, STATUS_WITHDRAWN);
        assert_eq!(q1.withdrawn_by, None);
        assert_eq!(q1.withdrawn_reason.as_deref(), Some(STALE_PROPOSAL_REASON));
        assert!(repo.get_question(node, 2).unwrap().unwrap().is_open());
        assert!(repo.stale_proposal_questions(node).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn agent_writes_are_attributed_and_guarded() {
        let (dir, conn, node, session) = setup();
        let agent = Uuid::new_v4();
        run(
            &conn,
            ACTOR_USER,
            InterviewCommand::AddQuestion {
                node_id: node,
                session_id: Some(session),
                phase: Some(PHASE_REQUIREMENTS.into()),
                draft: draft("First?", None),
            },
        )
        .unwrap();
        let head = InterviewRepo::new(&conn).head_rev().unwrap();
        run(
            &conn,
            ACTOR_USER,
            InterviewCommand::CreateAgentSession {
                id: agent,
                node_id: node,
                interview_session_id: Some(session),
                phase: PHASE_REQUIREMENTS.into(),
                role: Role::QuestionMaker,
                lane: 0,
                synced_rev: head,
                snapshot_tokens: 10,
            },
        )
        .unwrap();

        // The user answers after the agent's context was built…
        run(
            &conn,
            ACTOR_USER,
            InterviewCommand::DeferQuestion {
                node_id: node,
                seq: 1,
            },
        )
        .unwrap();
        // …so the agent's withdraw is refused with the current state.
        let err = run(
            &conn,
            &agent.to_string(),
            InterviewCommand::WithdrawQuestion {
                node_id: node,
                seq: 1,
                reason: "stale".into(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("conflict"), "{err}");
        assert!(err.to_string().contains("deferred"), "{err}");

        // Agent-authored rows carry the role; its own changes are excluded from its deltas.
        run(
            &conn,
            &agent.to_string(),
            InterviewCommand::AddQuestion {
                node_id: node,
                session_id: None,
                phase: None,
                draft: draft("Second?", None),
            },
        )
        .unwrap();
        let repo = InterviewRepo::new(&conn);
        let q2 = repo.get_question(node, 2).unwrap().unwrap();
        assert_eq!(q2.author, AUTHOR_QUESTION_MAKER);
        assert_eq!(q2.session_id, Some(session));
        let delta = repo
            .changes_since(&[node], head, &agent.to_string())
            .unwrap();
        assert!(delta.iter().all(|c| c.entity_id != q2.id));
        assert!(delta.iter().any(|c| c.entity == ENTITY_QUESTION));
        let _ = std::fs::remove_dir_all(dir);
    }
}
