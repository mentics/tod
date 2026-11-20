//! Triage's writes (`doc/requests/design.md` section 8, `doc/lifecycle/triage.md`).
//!
//! An agent's judgment request is filed with status `triage`
//! ([`crate::decisions::DECISION_TRIAGE`]) and read by a triage session, which
//! ends it one of five ways, each a verb of `tod-cli triage`:
//!
//! - **answer**: it decides, with a rationale (`decided_by = triage`, the
//!   session linked on the answer). Refused for a request whose options carry
//!   obligation edits, or whose chosen option moves the node: the user decides
//!   those.
//! - **escalate**: it hands the request to the user with a brief, which
//!   replaces the question; the request becomes `pending` and the session stays
//!   linked as the user's chat partner for it.
//! - **convert-suggestion**: the request was work for later; it is filed as a
//!   suggestion and the request closes.
//! - **convert-dispute**: the request disputes an obligation; it becomes a
//!   send-back with obligations named (`crate::send_back`) and the request closes.
//! - **settle** (in the chat, after escalation): the user decided with it
//!   (`decided_by = user`).
//!
//! plus **lesson**, the one reflection a closed request gets. Every verb checks
//! that the writing session is the triage session linked to the request.

use crate::conversation::{ConversationRepo, ProtocolKind};
use crate::decisions::{DecisionRepo, EvidenceRef};
use anyhow::{Result, bail};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// One write of triage (`InterviewCommand::Triage`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// The app started the session that reads `decision`, making its
    /// conversation (a task-focused triage one) when `conversation` is new.
    Start { decision: Uuid, conversation: Uuid },
    Answer {
        decision: Uuid,
        conversation: Uuid,
        #[serde(default)]
        option: Option<i64>,
        #[serde(default)]
        text: Option<String>,
        rationale: String,
    },
    Escalate {
        decision: Uuid,
        conversation: Uuid,
        brief: String,
        #[serde(default)]
        options: Option<Vec<String>>,
    },
    ConvertSuggestion {
        decision: Uuid,
        conversation: Uuid,
        title: String,
        body: String,
        rationale: String,
    },
    ConvertDispute {
        decision: Uuid,
        conversation: Uuid,
        to: String,
        reason: String,
        obligations: Vec<Uuid>,
        #[serde(default)]
        evidence: Vec<EvidenceRef>,
    },
    /// The user settled an escalated request in the chat.
    Settle {
        decision: Uuid,
        conversation: Uuid,
        #[serde(default)]
        option: Option<i64>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        rationale: String,
    },
    Lesson {
        decision: Uuid,
        conversation: Uuid,
        category: String,
        summary: String,
        #[serde(default)]
        user_quote: String,
    },
}

/// The error for a session that is not a triage session.
pub const WHO_MAY: &str = "only the triage session that reads a request writes this";

/// The conversation `actor` writes as, which must be a triage session and the
/// one the operation names. `app` ops (`Start`) skip the actor check.
fn check_session(conn: &Connection, actor: &str, conversation: Uuid) -> Result<crate::conversation::Conversation> {
    let Some(found) = ConversationRepo::new(conn).get(conversation)? else {
        bail!("conversation {conversation} not found: {WHO_MAY}");
    };
    if found.protocol != ProtocolKind::Triage {
        bail!("a {} session may not do this: {WHO_MAY}", found.protocol.as_str());
    }
    let named = crate::conversation::actor_conversation(actor)
        .or_else(|| crate::conversation::agent_actor_conversation(actor));
    if named.is_some_and(|id| id != found.id) {
        bail!("conversation {} is not the one writing ({actor})", found.id);
    }
    Ok(found)
}

/// Runs `op` as `actor` and returns its result.
pub fn run_op(conn: &Connection, actor: &str, op: &Op) -> Result<Value> {
    let decisions = DecisionRepo::new(conn);
    match op {
        Op::Start { decision, conversation } => {
            let target = decisions.get(*decision)?.ok_or_else(|| anyhow::anyhow!("decision {decision} not found"))?;
            let conversations = ConversationRepo::new(conn);
            match conversations.get(*conversation)? {
                Some(found) if found.protocol != ProtocolKind::Triage => {
                    bail!("conversation {conversation} is not a triage session");
                }
                Some(_) => {}
                // The session is made here, about the request's task, so the
                // link and the row are one write.
                None => {
                    conversations.create_with_id(
                        *conversation,
                        crate::conversation::Focus::Node(target.node_id),
                        ProtocolKind::Triage,
                        None,
                        None,
                        None,
                    )?;
                }
            }
            decisions.link_triage(*decision, *conversation)?;
            Ok(json!({ "decision": decision.to_string() }))
        }
        Op::Answer { decision, conversation, option, text, rationale } => {
            check_session(conn, actor, *conversation)?;
            let answer =
                decisions.answer_triaged(*decision, *conversation, *option, text.as_deref(), rationale)?;
            Ok(json!({ "id": answer.id }))
        }
        Op::Escalate { decision, conversation, brief, options } => {
            check_session(conn, actor, *conversation)?;
            decisions.escalate(*decision, *conversation, brief, options.as_deref())?;
            Ok(json!({ "decision": decision.to_string() }))
        }
        Op::ConvertSuggestion { decision, conversation, title, body, rationale } => {
            check_session(conn, actor, *conversation)?;
            let found = decisions.get(*decision)?.ok_or_else(|| anyhow::anyhow!("decision {decision} not found"))?;
            if found.triage_conversation_id != Some(*conversation) || !found.in_triage() {
                bail!("decision {decision} is not a request this triage session is reading");
            }
            if title.trim().is_empty() || body.trim().is_empty() {
                bail!("a suggestion needs a title and a body that stands alone");
            }
            let mut new = crate::suggestions::NewSuggestion::follow_up_ticket(
                found.node_id,
                title.trim(),
                body.trim(),
                crate::suggestions::FILED_BY_TRIAGE,
            );
            new.conversation_id = Some(*conversation);
            let filed = crate::suggestions::file(conn, &new)?;
            let what = format!("Not a decision: filed as a suggestion, \"{}\".", title.trim());
            let answer = decisions.close_converted(*decision, *conversation, &what, rationale)?;
            Ok(json!({ "suggestion": filed.id.to_string(), "already_open": filed.already_open, "answer": answer.id }))
        }
        Op::ConvertDispute { decision, conversation, to, reason, obligations, evidence } => {
            let session = check_session(conn, actor, *conversation)?;
            let found = decisions.get(*decision)?.ok_or_else(|| anyhow::anyhow!("decision {decision} not found"))?;
            if found.triage_conversation_id != Some(*conversation) || !found.in_triage() {
                bail!("decision {decision} is not a request this triage session is reading");
            }
            if session.focus.node_id() != Some(found.node_id) {
                bail!("this triage session is not about the request's task");
            }
            if obligations.is_empty() {
                bail!("a dispute names the obligations it disputes (--obligation)");
            }
            let outcome = crate::send_back::execute(
                conn,
                actor,
                &crate::send_back::Request {
                    conversation_id: *conversation,
                    to: to.clone(),
                    reason: reason.clone(),
                    evidence: evidence.clone(),
                    obligations: obligations.clone(),
                },
            )?;
            if outcome["outcome"] != crate::send_back::OUTCOME_MOVED {
                // The loop guard stopped it: nothing moved, the request stands.
                return Ok(outcome);
            }
            // The move withdraws the open requests of the stay it ends, this
            // one included; when it is still open, it closes as the dispute.
            let still_open = decisions.get(*decision)?.is_some_and(|d| crate::decisions::is_open(&d.status));
            if still_open {
                let what = format!("Not a decision: sent back to {to} as a dispute of an obligation.");
                decisions.close_converted(*decision, *conversation, &what, reason)?;
            }
            Ok(outcome)
        }
        Op::Settle { decision, conversation, option, text, rationale } => {
            check_session(conn, actor, *conversation)?;
            let answer = decisions.answer_in_chat(*decision, *conversation, *option, text.as_deref(), rationale)?;
            Ok(json!({ "id": answer.id }))
        }
        Op::Lesson { decision, conversation, category, summary, user_quote } => {
            check_session(conn, actor, *conversation)?;
            let lesson =
                crate::lessons::record_triage_lesson(conn, *decision, *conversation, category, summary, user_quote)?;
            Ok(json!({ "id": lesson.id.to_string() }))
        }
    }
}

#[cfg(test)]
mod tests;
