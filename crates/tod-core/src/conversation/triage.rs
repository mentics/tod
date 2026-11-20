//! Triage (`doc/requests/design.md` section 8, `doc/lifecycle/triage.md`): one
//! fresh, small session per judgment request an agent files. It reads the
//! request and what it needs to judge it (the asking session's last turns, the
//! task's state, obligations with their authority, a design summary, and what
//! the user already answered on the task) and ends it one of four ways, each a
//! `tod-cli triage` verb: **answer** it, **escalate** it to the user with a
//! brief, **convert** it to a suggestion, or **convert** it to a dispute of an
//! obligation. It edits nothing and holds no ticket.
//!
//! The autopilot of the asking node starts it (`Autopilot::triage_step`), as it
//! does an incident investigation; the asking agent waits for the answer, which
//! reaches it like the user's. An escalated request keeps its session linked: it
//! is the user's chat partner for the request (the drawer's **Discuss**), and
//! it records one lesson when the request closes.

use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, node_id};
use super::protocol::{Protocol, ProtocolEnv};
use crate::dynamic::DynamicContext;
use crate::node_context::{obligation_line, one_line};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::fmt::Write as _;
use tod_store::conversation::{ConversationRepo, ProtocolKind, TurnRole};
use tod_store::decisions::{Decision, DecisionRepo};
use tod_store::fleet::Workdir;
use tod_store::fleet::provision::resolve_launch_cwd;
use tod_store::interview::short_id;
use tod_store::outline::repos::{NodeRepo, ObligationRepo};
use uuid::Uuid;

pub struct TriageProtocol;

/// What the session is told to start with.
pub const STARTER: &str = "Triage the request below.";

/// What a session that stopped before it finished is resumed with.
pub const RESUME_MESSAGE: &str =
    "The request below is still waiting on triage. Finish it: answer it, escalate it, or convert it.";

/// The most turns of the asking session shown.
const TURNS: usize = 6;
/// The most characters of one of those turns.
const TURN_CHARS: usize = 700;
/// The most answered requests listed.
const ANSWERED: usize = 8;

fn cut(text: &str, chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(chars).collect();
    out.push_str(" ...");
    out
}

/// The request `conversation` reads, if it has been linked to one.
pub fn decision_for(conn: &Connection, conversation: Uuid) -> Result<Option<Decision>> {
    DecisionRepo::new(conn).for_triage_conversation(conversation)
}

/// How `decision` closed, in a line or two: who decided, what, and why. `None`
/// while it is open.
pub fn closed_as(conn: &Connection, decision: &Decision) -> Result<Option<String>> {
    let Some(found) = DecisionRepo::new(conn).get_with_answers(decision.id)? else { return Ok(None) };
    if tod_store::decisions::is_open(&decision.status) {
        return Ok(None);
    }
    let Some(answer) = found.answers.last() else { return Ok(None) };
    let picked = answer
        .option
        .and_then(|o| decision.options.get(usize::try_from(o).ok()?.checked_sub(1)?))
        .map(|label| format!("option \"{label}\""));
    let given = match (picked, answer.text.as_deref()) {
        (Some(label), Some(text)) => format!("{label} ({text})"),
        (Some(label), None) => label,
        (None, Some(text)) => format!("\"{text}\""),
        (None, None) => "(no text)".to_string(),
    };
    let mut out = format!("Decided by {}: {given}", answer.decided_by);
    if let Some(why) = answer.rationale.as_deref().filter(|r| !r.trim().is_empty()) {
        let _ = write!(out, "\nRationale: {}", why.trim());
    }
    Ok(Some(out))
}

/// What a triage session is told when the request it read has closed.
pub fn reflection_message(conn: &Connection, decision: &Decision) -> Result<String> {
    let closed = closed_as(conn, decision)?.unwrap_or_default();
    Ok(format!(
        "The request you triaged has closed.\n\n{closed}\n\nRecord one lesson about it with the `lesson` verb, once, \
         and write nothing else."
    ))
}

/// The request and what is needed to judge it, as the session sees it.
pub fn render(conn: &Connection, decision: &Decision) -> Result<String> {
    let nodes = NodeRepo::new(conn);
    let repo = DecisionRepo::new(conn);
    let mut out = String::new();

    let title = nodes.get(decision.node_id)?.map(|n| n.title).unwrap_or_default();
    let state = nodes.get_lifecycle(decision.node_id)?.unwrap_or_else(|| "(none)".to_string());
    let _ = writeln!(out, "Request `{}` on the task \"{}\", which is in `{state}`.\n", short_id(decision.id), one_line(&title));
    let _ = writeln!(out, "Status: {}. Reason the agent gave: `{}`. Asked by: {}.\n", decision.status, decision.reason, decision.protocol.as_deref().unwrap_or("an agent"));

    let (question, options) = match &decision.asked {
        Some(asked) => (asked.question.as_str(), asked.options.as_slice()),
        None => (decision.question.as_str(), decision.options.as_slice()),
    };
    let _ = writeln!(out, "### What the agent asked\n\n{}\n", question.trim());
    if options.is_empty() {
        out.push_str("It gave no options (free text).\n\n");
    } else {
        out.push_str("Options, numbered as the user sees them:\n");
        for (i, option) in options.iter().enumerate() {
            let n = i64::try_from(i + 1).unwrap_or(0);
            let mut notes = Vec::new();
            if let Some(to) = decision.move_for(n) {
                notes.push(format!("moves the task back to `{to}`: only the user decides this"));
            }
            for edit in decision.edits.iter().filter(|e| e.option == n) {
                notes.push(if edit.affirm {
                    format!("approves obligation {}: only the user decides this", short_id(edit.obligation_id))
                } else {
                    format!("rewrites obligation {}: only the user decides this", short_id(edit.obligation_id))
                });
            }
            let note = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join("; ")) };
            let _ = writeln!(out, "{n}. {}{note}", one_line(option));
        }
        out.push('\n');
    }
    if !decision.new_evidence.trim().is_empty() {
        let _ = writeln!(out, "New since it was answered before: {}\n", decision.new_evidence.trim());
    }
    if !decision.evidence.is_empty() {
        out.push_str("Evidence it cites:\n");
        for e in &decision.evidence {
            let _ = writeln!(out, "- {} {}", e.kind, e.id);
        }
        out.push('\n');
    }
    if !decision.blocks.is_empty() {
        let steps: Vec<String> = decision.blocks.iter().map(|id| short_id(*id)).collect();
        let _ = writeln!(out, "It holds up plan steps [{}].\n", steps.join(", "));
    }

    if let Some(closed) = closed_as(conn, decision)? {
        let _ = writeln!(out, "### How it closed\n\n{closed}\n");
    }

    // The asking session's last turns.
    if let Some(asker) = decision.conversation_id {
        let turns = ConversationRepo::new(conn).turns(asker)?;
        let shown: Vec<_> = turns
            .iter()
            .filter(|t| matches!(t.role, TurnRole::User | TurnRole::Agent | TurnRole::Continuation))
            .collect();
        let tail = &shown[shown.len().saturating_sub(TURNS)..];
        if !tail.is_empty() {
            out.push_str("### The last turns of the asking session\n\n");
            for turn in tail {
                let who = if turn.role == TurnRole::Agent { "agent" } else { "app" };
                let body = if turn.body.trim().is_empty() { "(no text)".to_string() } else { cut(&turn.body, TURN_CHARS) };
                let _ = writeln!(out, "- {who}: {}", one_line(&body));
            }
            out.push('\n');
        }
    }

    out.push_str("### Obligations\n\n");
    let obligations = ObligationRepo::new(conn).list_for_node(decision.node_id)?;
    if obligations.is_empty() {
        out.push_str("(none)\n\n");
    } else {
        for o in &obligations {
            let _ = writeln!(out, "- {}", obligation_line(o));
        }
        out.push('\n');
    }
    if let Some(summary) = nodes.get_summary(decision.node_id)? {
        let _ = writeln!(out, "### Design summary\n\n{}\n", summary.body.trim());
    }

    // What the user already answered on this task.
    let mut answered = Vec::new();
    for other in repo.list_for_node(decision.node_id)? {
        if other.id == decision.id || other.status != tod_store::decisions::DECISION_ANSWERED {
            continue;
        }
        let Some(found) = repo.get_with_answers(other.id)? else { continue };
        let Some(answer) = found.answers.last() else { continue };
        let given = match (answer.option.and_then(|o| other.options.get(usize::try_from(o).ok()?.checked_sub(1)?)), answer.text.as_deref()) {
            (Some(label), Some(text)) => format!("\"{label}\" ({text})"),
            (Some(label), None) => format!("\"{label}\""),
            (None, Some(text)) => format!("\"{text}\""),
            (None, None) => continue,
        };
        answered.push(format!(
            "- {} -> {given} (decided by {})",
            one_line(other.question.lines().next().unwrap_or("")),
            answer.decided_by
        ));
        if answered.len() >= ANSWERED {
            break;
        }
    }
    if !answered.is_empty() {
        out.push_str("### Requests already answered on this task\n\n");
        for line in answered {
            let _ = writeln!(out, "{line}");
        }
        out.push('\n');
    }
    Ok(out)
}

impl Protocol for TriageProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Triage
    }

    fn surface(&self) -> &'static str {
        crate::session_name::TRIAGE_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some(STARTER)
    }

    /// The task's checkout: read-only access to its files and pull request is
    /// what lets triage check a fact instead of asking about it.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<Workdir> {
        resolve_launch_cwd(env.fleet, &node_id(env)?.to_string())
    }

    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        let mut vars = vec![(IMPLEMENT_CONVERSATION_ENV.to_string(), env.conversation_id.to_string())];
        if let Ok(node) = node_id(env) {
            vars.push((IMPLEMENT_NODE_ENV.to_string(), node.to_string()));
        }
        vars
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        let text = env
            .fleet
            .read(|conn| {
                let decision = decision_for(conn, env.conversation_id)?
                    .with_context(|| format!("triage conversation {} is not linked to a request", env.conversation_id))?;
                render(conn, &decision)
            })
            .context("could not read the request to triage")?;
        let ctx = DynamicContext { data_root: Some(env.data_root), triage: &text, ..Default::default() };
        crate::context_recipes::build_message(env.media, &crate::context_recipes::TRIAGE, None, &ctx, "")
    }

    fn resume_snapshot(&self, env: &ProtocolEnv<'_>, _budget_tokens: i64, _before_seq: Option<i64>) -> Result<String> {
        self.opening(env)
    }
}

/// Plays the triage session for `--agent mock`.
///
/// - A request whose question has a line `triage-answer <N>` is answered with
///   option N.
/// - Any other request in triage is escalated with the brief
///   `Brief: <question>`.
/// - An escalated one the user settles in the chat (`settle <N>`) is settled
///   with option N.
/// - A closed one with no lesson gets one: `asker_could_decide` when triage
///   decided it, `necessary` otherwise.
pub fn mock_turn(access: &impl super::mock::Access, _node_id: Uuid, conversation_id: Uuid) -> Result<String> {
    use tod_store::interview::InterviewCommand;
    use tod_store::triage::Op;
    let (decision, last_user) = access.read(|conn| {
        let decision = decision_for(conn, conversation_id)?;
        let last_user = ConversationRepo::new(conn)
            .turns(conversation_id)?
            .iter()
            .rev()
            .find(|t| t.role == TurnRole::User)
            .map(|t| t.body.clone())
            .unwrap_or_default();
        Ok((decision, last_user))
    })?;
    let Some(decision) = decision else { return Ok(String::new()) };
    let (question, status) = (decision.question.clone(), decision.status.clone());
    let run = |op: Op| access.interview(InterviewCommand::Triage(op));
    if decision.in_triage() {
        let answer = question
            .lines()
            .find_map(|l| l.trim().strip_prefix("triage-answer ").and_then(|n| n.trim().parse::<i64>().ok()));
        match answer {
            Some(option) => {
                run(Op::Answer {
                    decision: decision.id,
                    conversation: conversation_id,
                    option: Some(option),
                    text: None,
                    rationale: "The task's own obligations settle it.".to_string(),
                })?;
            }
            None => {
                run(Op::Escalate {
                    decision: decision.id,
                    conversation: conversation_id,
                    brief: format!("Brief: {}", question.trim()),
                    options: None,
                })?;
            }
        }
    } else if status == tod_store::decisions::DECISION_PENDING {
        if let Some(option) = last_user.trim().strip_prefix("settle ").and_then(|n| n.trim().parse::<i64>().ok()) {
            run(Op::Settle {
                decision: decision.id,
                conversation: conversation_id,
                option: Some(option),
                text: None,
                rationale: "Settled with the user in the chat.".to_string(),
            })?;
        }
    } else if status == tod_store::decisions::DECISION_ANSWERED {
        let has_lesson = access.read(|conn| Ok(!tod_store::lessons::for_decision(conn, decision.id)?.is_empty()))?;
        if !has_lesson {
            let by_triage = access.read(|conn| {
                Ok(DecisionRepo::new(conn)
                    .get_with_answers(decision.id)?
                    .and_then(|d| d.answers.last().map(|a| a.decided_by == tod_store::decisions::DECIDED_BY_TRIAGE))
                    .unwrap_or(false))
            })?;
            let category = if by_triage {
                tod_store::lessons::CATEGORY_ASKER_COULD_DECIDE
            } else {
                tod_store::lessons::CATEGORY_NECESSARY
            };
            run(Op::Lesson {
                decision: decision.id,
                conversation: conversation_id,
                category: category.to_string(),
                summary: "Mock reflection.".to_string(),
                user_quote: String::new(),
            })?;
        }
    }
    Ok(String::new())
}
