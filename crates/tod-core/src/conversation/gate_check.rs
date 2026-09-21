//! The lifecycle's two state-agent conversations: the **gate check** (does the
//! node's current state's work satisfy the gate to its next state?) and
//! **on entry** (the state's own setup work, run when a node lands in it).
//!
//! Both used to be one-shot turns run beside the conversation view, so nothing
//! the agent said was kept anywhere the user could read it. They are ordinary
//! conversations now — a transcript, a picker entry that names the transition,
//! and follow-up messages — that differ from the rest only in the context the
//! agent is given and, for the gate check, in that the app reads the reply:
//! [`GateCheckProtocol::on_reply`] parses the structured reply, records the
//! per-criterion results, advances a prose-only gate that passed, and stores
//! the report the conversation view shows.

use super::implement::node_id;
use super::protocol::{Protocol, ProtocolEnv, RunNotice};
use crate::gate::response::GateBlocker;
use crate::gate::{
    GateCheckRequest, PlanStepWithLinks, build_gate_check_message, build_on_entry_message,
    evaluate_derived_criterion, parse_gate_reply,
};
use crate::process_bundle::{ProcessManifest, TodInstallPaths, state_role_doc};
use crate::task::model::next_lifecycle;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use tod_store::conversation::{Conversation, ConversationRepo, Focus, ProtocolKind};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use tod_store::outline::repos::NodeRepo;
use tod_store::outline::{EXTRA_CONTENT_DETAILS, OutlineMutation, SOURCE_AGENT, SOURCE_DERIVED};
use uuid::Uuid;

/// The key a gate check's report is stored under in its conversation.
const REPORT_KEY: &str = "gate_check";

/// What a gate check concluded, as stored on its conversation and shown by the
/// conversation view. The per-criterion rows are not here: the app records
/// those on the node (`gate_evaluations`) as it always has.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateReportRecord {
    /// `pass | blocked | needs_human | no_change`.
    pub result: String,
    pub summary: String,
    /// The one step the agent recommends next.
    pub next: String,
    pub blockers: Vec<StoredBlocker>,
    pub findings: String,
    /// The gate did not pass and nothing says why.
    pub no_reasons: bool,
    /// The state the node moved to off this reply, when it did.
    pub advanced_to: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredBlocker {
    pub kind: String,
    pub reference: String,
    pub what: String,
    pub action: String,
}

impl From<&GateBlocker> for StoredBlocker {
    fn from(b: &GateBlocker) -> Self {
        Self {
            kind: b.kind.clone(),
            reference: b.reference.clone(),
            what: b.what.clone(),
            action: b.action.clone(),
        }
    }
}

impl GateReportRecord {
    fn to_value(&self) -> Value {
        json!({ REPORT_KEY: self })
    }

    fn from_value(value: &Value) -> Option<Self> {
        serde_json::from_value(value.get(REPORT_KEY)?.clone()).ok()
    }

    /// A gate-check report as stored on its conversation; `None` for any
    /// other protocol's report.
    pub fn from_stored(value: &Value) -> Option<Self> {
        Self::from_value(value)
    }
}

/// The node's latest gate-check conversation for its current transition, and
/// what it concluded. `None` when there is none, or it concluded nothing the
/// app could read.
pub fn latest_gate_report(
    conn: &Connection,
    node: Uuid,
    from_state: &str,
) -> Result<Option<(Conversation, GateReportRecord)>> {
    let repo = ConversationRepo::new(conn);
    let Some(conversation) =
        repo.latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::GateCheck)?
    else {
        return Ok(None);
    };
    // A check of an earlier state says nothing about this one.
    if conversation.from_state.as_deref() != Some(from_state) {
        return Ok(None);
    }
    // ...and so does one from a previous visit: a node sent back and advanced
    // again starts the state afresh.
    let entered_at: Option<i64> = conn
        .query_row(
            "SELECT updated_at FROM node_lifecycle WHERE node_id = ?1",
            [node.as_bytes().to_vec()],
            |row| row.get(0),
        )
        .optional()?;
    if entered_at.is_some_and(|entered| conversation.created_at < entered) {
        return Ok(None);
    }
    let record = repo
        .latest_report(conversation.id)?
        .and_then(|value| GateReportRecord::from_value(&value));
    Ok(record.map(|record| (conversation, record)))
}

/// The states a conversation about `focus` is between: the node's current
/// lifecycle state and the one after it. `None` off a node.
fn current_and_next(fleet: &FleetStore, focus: Focus) -> Option<(String, Option<String>)> {
    let node = focus.node_id()?;
    let lifecycle = fleet.get_node(&node.to_string()).ok()??.lifecycle;
    let next = next_lifecycle(&lifecycle).map(str::to_string);
    Some((lifecycle, next))
}

/// The transition this conversation is about, from its row.
fn transition(env: &ProtocolEnv<'_>) -> Result<(String, String)> {
    let conversation = env
        .fleet
        .read(|conn| ConversationRepo::new(conn).get(env.conversation_id))?
        .context("conversation not found")?;
    match (conversation.from_state, conversation.to_state) {
        (Some(from), Some(to)) => Ok((from, to)),
        _ => anyhow::bail!("this conversation records no lifecycle transition"),
    }
}

/// Everything the state agent is told about the node, for the two messages.
/// `criteria` are the transition's criteria the agent judges (empty on entry).
fn build_request<'a>(
    env: &'a ProtocolEnv<'_>,
    node: Uuid,
    from: &str,
    to: &str,
    criteria: Vec<(tod_store::outline::GateCriterion, Option<tod_store::outline::NodeGateEvaluation>)>,
) -> Result<GateCheckRequest<'a>> {
    let fleet = env.fleet;
    let node_row = fleet
        .get_node(&node.to_string())?
        .with_context(|| format!("node {node} not found"))?;
    let obligations = fleet.list_obligations_for_node(node).unwrap_or_default();
    let ancestor_context = fleet
        .read(|conn| {
            crate::node_context::render_inherited_context(conn, &NodeRepo::new(conn), node, None)
        })
        .unwrap_or_default();
    let plan_steps = fleet
        .list_plan_steps_for_node(node)
        .unwrap_or_default()
        .into_iter()
        .map(|step| {
            let depends_on = fleet.list_plan_step_dependencies(step.id).unwrap_or_default();
            let satisfies = fleet.list_plan_step_obligations(step.id).unwrap_or_default();
            PlanStepWithLinks {
                step,
                depends_on,
                satisfies,
            }
        })
        .collect();
    // The retrospective is the one state agent that has to know what went
    // wrong along the way; the plan and obligations only show how it ended.
    let work_history = if from == "learn" {
        fleet
            .read(|conn| crate::node_context::render_work_history(conn, node))
            .unwrap_or_default()
    } else {
        String::new()
    };
    Ok(GateCheckRequest {
        data_root: env.data_root,
        node_id: node,
        node_title: node_row.title,
        node_lifecycle: from.to_string(),
        node_body: fleet
            .get_extra_content(node, EXTRA_CONTENT_DETAILS)
            .ok()
            .flatten(),
        obligations,
        ancestor_context,
        plan_steps,
        work_history,
        from_state: from.to_string(),
        to_state: to.to_string(),
        criteria,
    })
}

fn role_doc(state: &str) -> Result<String> {
    let manifest = ProcessManifest::load(&TodInstallPaths::discover()?)?;
    state_role_doc(&manifest, state)
}

fn state_cwd(env: &ProtocolEnv<'_>) -> Result<PathBuf> {
    let node = node_id(env)?;
    Ok(env.fleet.files_dir_or_data_root(&node.to_string()))
}

// ── Gate check ──────────────────────────────────────────────────────────

/// Does the node satisfy the gate to its next state?
pub struct GateCheckProtocol;

impl GateCheckProtocol {
    /// The transition's criteria the agent is left to judge. The ones the app
    /// can answer from its own data are answered and saved here, not shown to
    /// the agent: it is not given that data, so it could only guess. Saving
    /// is idempotent, so a second call (a resumed session) is harmless.
    fn agent_criteria(
        fleet: &FleetStore,
        node: Uuid,
        from: &str,
        to: &str,
    ) -> Result<(
        bool,
        Vec<(tod_store::outline::GateCriterion, Option<tod_store::outline::NodeGateEvaluation>)>,
    )> {
        let mut agent_criteria = Vec::new();
        let mut derived_rows = Vec::new();
        for (criterion, eval) in fleet.gate_criteria_for_transition(node, from, to)? {
            match fleet.read(|conn| evaluate_derived_criterion(conn, node, &criterion))? {
                Some(derived) => derived_rows.push((
                    criterion.id,
                    derived.outcome.to_string(),
                    Some(derived.detail),
                    tod_store::outline::repos::gate::ACTION_NONE.to_string(),
                )),
                None => agent_criteria.push((criterion, eval)),
            }
        }
        let had_derived = !derived_rows.is_empty();
        if had_derived {
            fleet.enqueue_outline(OutlineMutation::ApplyGateResults {
                node_id: node,
                results: derived_rows,
                forward_state: None,
                source: SOURCE_DERIVED.to_string(),
            })?;
            fleet.writer().flush()?;
        }
        Ok((had_derived, agent_criteria))
    }

    /// Record a parsed reply: the agent's per-criterion rows, the node's
    /// advance when the gate has no criteria and the agent passed it, and the
    /// report the view shows.
    fn apply(env: &ProtocolEnv<'_>, reply_text: &str) -> Result<Vec<RunNotice>> {
        let (from, to) = transition(env)?;
        let node = node_id(env)?;
        // A reply in a check of a transition the node has since made says
        // nothing about where it is now, and must not move it again.
        let lifecycle = env
            .fleet
            .get_node(&node.to_string())?
            .map(|n| n.lifecycle)
            .unwrap_or_default();
        if lifecycle != from {
            return Ok(Vec::new());
        }
        let has_report = env
            .fleet
            .read(|conn| ConversationRepo::new(conn).latest_report(env.conversation_id))?
            .is_some();
        let reply = match parse_gate_reply(reply_text) {
            Ok(reply) => reply,
            // A follow-up answer in the conversation is not a new verdict.
            Err(_) if has_report => return Ok(Vec::new()),
            Err(err) => {
                return Ok(vec![RunNotice::Error(format!(
                    "The gate check's reply was not understood ({err:#}). \
                     Its full reply is in the transcript; ask it to answer again."
                ))]);
            }
        };

        // A gate with structured criteria always stops for the user, even on
        // a `pass`; only a prose-only gate advances off the agent's verdict.
        let has_criteria = !env
            .fleet
            .gate_criteria_for_transition(node, &from, &to)?
            .is_empty();
        let advances = reply.result.advances() && !has_criteria;
        let forward_state = advances.then(|| to.clone());
        let results: Vec<_> = reply
            .gate_results
            .iter()
            .map(|row| {
                let action = if row.action == crate::gate::GateAction::Interview {
                    tod_store::outline::repos::gate::ACTION_INTERVIEW
                } else {
                    tod_store::outline::repos::gate::ACTION_NONE
                };
                (
                    row.criterion_id,
                    row.outcome.clone(),
                    row.detail.clone(),
                    action.to_string(),
                )
            })
            .collect();
        if !results.is_empty() || advances {
            env.fleet.enqueue_outline(OutlineMutation::ApplyGateResults {
                node_id: node,
                results,
                forward_state: forward_state.clone(),
                source: SOURCE_AGENT.to_string(),
            })?;
            env.fleet.writer().flush()?;
        }

        let record = GateReportRecord {
            result: match reply.result {
                crate::gate::GateOutcome::Pass => "pass",
                crate::gate::GateOutcome::Blocked => "blocked",
                crate::gate::GateOutcome::NeedsHuman => "needs_human",
                crate::gate::GateOutcome::NoChange => "no_change",
            }
            .to_string(),
            summary: reply.summary.clone(),
            next: reply.next.clone(),
            blockers: reply.blockers.iter().map(StoredBlocker::from).collect(),
            findings: reply.findings.clone(),
            no_reasons: reply.gives_no_reasons(),
            advanced_to: forward_state,
        };
        env.fleet.interview(
            ACTOR_USER,
            InterviewCommand::RecordConversationReport {
                conversation_id: env.conversation_id,
                body: record.to_value(),
            },
        )?;
        Ok(Vec::new())
    }
}

impl Protocol for GateCheckProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::GateCheck
    }

    fn surface(&self) -> &'static str {
        crate::session_name::GATE_CHECK_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some("Run the gate check.")
    }

    fn transition(&self, fleet: &FleetStore, focus: Focus) -> Option<(String, String)> {
        let (from, next) = current_and_next(fleet, focus)?;
        Some((from, next?))
    }

    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<PathBuf> {
        state_cwd(env)
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        let (from, to) = transition(env)?;
        let node = node_id(env)?;
        let (_, criteria) = Self::agent_criteria(env.fleet, node, &from, &to)?;
        let request = build_request(env, node, &from, &to, criteria)?;
        build_gate_check_message(env.media, &request, &role_doc(&from)?)
    }

    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        _budget_tokens: i64,
        _before_seq: Option<i64>,
    ) -> Result<String> {
        self.opening(env)
    }

    fn on_reply(&self, env: &ProtocolEnv<'_>, reply: &str) -> Vec<RunNotice> {
        match Self::apply(env, reply) {
            Ok(notices) => notices,
            Err(err) => vec![RunNotice::Error(format!(
                "Could not record the gate check: {err:#}"
            ))],
        }
    }
}

/// Answer the node's gate criteria the app can answer from its own data, and
/// save them. `true` when an agent still has something to judge — the gate has
/// a criterion only it can answer, or only prose rules; `false` when the app
/// answered every criterion, so there is nothing for a gate-check conversation
/// to add and the recorded rows are the whole result.
pub fn settle_derived_criteria(fleet: &FleetStore, node: Uuid) -> Result<bool> {
    let lifecycle = fleet
        .get_node(&node.to_string())?
        .with_context(|| format!("node {node} not found"))?
        .lifecycle;
    let Some(next) = next_lifecycle(&lifecycle) else {
        return Ok(false);
    };
    let (had_derived, agent_criteria) =
        GateCheckProtocol::agent_criteria(fleet, node, &lifecycle, next)?;
    Ok(!had_derived || !agent_criteria.is_empty())
}

// ── On entry ────────────────────────────────────────────────────────────

/// A state's own setup work, run when a node lands in it (`planning` writing
/// plan steps, for one). Idempotent by design: the agent adds only what is
/// missing.
pub struct OnEntryProtocol;

impl Protocol for OnEntryProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::OnEntry
    }

    fn surface(&self) -> &'static str {
        crate::session_name::ON_ENTRY_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some("Run this state's on-entry work.")
    }

    fn transition(&self, fleet: &FleetStore, focus: Focus) -> Option<(String, String)> {
        let (state, _) = current_and_next(fleet, focus)?;
        Some((state.clone(), state))
    }

    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<PathBuf> {
        state_cwd(env)
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        let (state, _) = transition(env)?;
        let node = node_id(env)?;
        let request = build_request(env, node, &state, &state, Vec::new())?;
        build_on_entry_message(env.media, &request, &role_doc(&state)?)
    }

    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        _budget_tokens: i64,
        _before_seq: Option<i64>,
    ) -> Result<String> {
        self.opening(env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_round_trips_through_its_stored_form() {
        let record = GateReportRecord {
            result: "blocked".into(),
            summary: "Two steps are stubs.".into(),
            next: "implement".into(),
            blockers: vec![StoredBlocker {
                kind: "plan_step".into(),
                reference: "031f62f7".into(),
                what: "placeholder".into(),
                action: "implement".into(),
            }],
            findings: String::new(),
            no_reasons: false,
            advanced_to: None,
        };
        assert_eq!(GateReportRecord::from_value(&record.to_value()), Some(record));
    }

    #[test]
    fn another_protocols_report_is_not_a_gate_report() {
        assert_eq!(
            GateReportRecord::from_value(&json!({ "review": "done" })),
            None
        );
    }
}
