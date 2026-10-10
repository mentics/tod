//! The lifecycle phase protocols (`doc/lifecycle/phase-agents.md`).
//!
//! [`PhaseProtocol`] is the agent for a state the generic phase agent works
//! (`crate::phase::PHASE_AGENT_STATES`): it does the state's work until the
//! gate out of it passes, asking the user through `tod-cli decisions` only for
//! what no agent can supply, and finishes by certifying the phase or, with
//! independent evaluation on, recording it ready. [`EvaluateProtocol`] is the
//! independent evaluator: a fresh session that certifies the phase or sends it
//! back with fixes, and cannot edit (`tod-cli` refuses its writes).
//!
//! They replace the gate-check and on-entry turns. Nothing in a reply is
//! parsed: what they decide, they record through `tod-cli phase`.

use super::context::{ReportedStale, delta};
use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, node_id};
use super::protocol::{
    Next, Protocol, ProtocolEnv, Stop, TurnContext, cap_or_stall, focus_cwd_or_scratch,
    hand_back_for_pending_decision,
};
use crate::context_recipes::{EVALUATE, PHASE};
use crate::gate::context::build_phase_message;
use crate::phase::{PhaseStanding, PhaseStep, independent_evaluation, render_status};
use anyhow::{Context, Result};
use rusqlite::Connection;
use tod_store::conversation::{Focus, ProtocolKind, actor_for};
use tod_store::decisions::{DECISION_ANSWERED, DecisionRepo, NewDecision, REASON_INTENT};
use tod_store::fleet::{FleetStore, Workdir};
use tod_store::interview::{ACTOR_ENV, ACTOR_USER, InterviewCommand, PHASE_REQUIREMENTS};
use tod_store::outline::repos::{NodeRepo, ObligationRepo, PlanStepRepo};
use tod_store::outline::types::Capability;
use tod_store::outline::{KIND_REQUIREMENT, OutlineMutation};
use tod_store::phase::{PHASE_CERTIFY, PHASE_REJECT, PhaseRepo, is_certifiable};
use uuid::Uuid;

/// The states a conversation about `focus` is between: the node's current
/// lifecycle state and the one after it. `None` off a node.
fn current_and_next(fleet: &FleetStore, focus: Focus) -> Option<(String, Option<String>)> {
    let node = focus.node_id()?;
    let lifecycle = fleet.get_node(&node.to_string()).ok()??.lifecycle;
    let next = crate::task::model::next_lifecycle(&lifecycle).map(str::to_string);
    Some((lifecycle, next))
}

/// The transition this conversation is about, from its row.
fn transition(env: &ProtocolEnv<'_>) -> Result<(String, String)> {
    let conversation = env
        .fleet
        .read(|conn| tod_store::conversation::ConversationRepo::new(conn).get(env.conversation_id))?
        .ok_or_else(|| anyhow::anyhow!("conversation not found"))?;
    match (conversation.from_state, conversation.to_state) {
        (Some(from), Some(to)) => Ok((from, to)),
        _ => anyhow::bail!("this conversation records no lifecycle transition"),
    }
}

/// Everything a phase agent or evaluator is told about the node.
fn build_request<'a>(
    env: &'a ProtocolEnv<'_>,
    node: Uuid,
    from: &str,
    to: &str,
    criteria: Vec<(tod_store::outline::GateCriterion, Option<tod_store::outline::NodeGateEvaluation>)>,
) -> Result<crate::gate::GateCheckRequest<'a>> {
    let fleet = env.fleet;
    let node_row = fleet
        .get_node(&node.to_string())?
        .ok_or_else(|| anyhow::anyhow!("node {node} not found"))?;
    let obligations = fleet.list_obligations_for_node(node).context("could not read the node's obligations")?;
    let ancestor_context = fleet
        .read(|conn| {
            crate::node_context::render_inherited_context(conn, &NodeRepo::new(conn), node, None)
        })
        .context("could not read the inherited context")?;
    let plan_steps = fleet
        .list_plan_steps_for_node(node)
        .context("could not read the plan")?
        .into_iter()
        .map(|step| {
            let depends_on = fleet.list_plan_step_dependencies(step.id)?;
            let satisfies = fleet.list_plan_step_obligations(step.id)?;
            Ok(crate::gate::PlanStepWithLinks {
                step,
                depends_on,
                satisfies,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // The retrospective is the one state agent that has to know what went
    // wrong along the way; the plan and obligations only show how it ended.
    let work_history = if from == "learn" {
        fleet
            .read(|conn| crate::node_context::render_work_history(conn, node))
            .context("could not read the node's work history")?
    } else {
        String::new()
    };
    Ok(crate::gate::GateCheckRequest {
        data_root: env.data_root,
        node_id: node,
        node_title: node_row.title,
        node_lifecycle: from.to_string(),
        node_body: fleet
            .get_extra_content(node, tod_store::outline::EXTRA_CONTENT_DETAILS)
            .ok()
            .flatten(),
        obligations,
        ancestor_context,
        plan_steps,
        work_history,
        skills: crate::skills_context::for_node(fleet, node, from),
        from_state: from.to_string(),
        to_state: to.to_string(),
        criteria,
    })
}

/// The state's role doc from the process bundle.
fn role_doc(state: &str) -> Result<String> {
    let manifest = crate::process_bundle::ProcessManifest::load(
        &crate::process_bundle::TodInstallPaths::discover()?,
    )?;
    crate::process_bundle::state_role_doc(&manifest, state)
}

/// The node's phase status, as the phase agent and the evaluator are told it.
fn status(env: &ProtocolEnv<'_>, node: Uuid, state: &str) -> Result<String> {
    let standing = env.fleet.read(|conn| PhaseStanding::load(conn, node, state))?;
    Ok(render_status(&standing, independent_evaluation(env.data_root)))
}

/// Both protocols' variables: the conversation actor, so the phase agent's
/// outline writes are a reversible change set and `tod-cli` knows the
/// evaluator for what it is, and the node and conversation `decisions` and
/// `phase` default to.
fn phase_env(env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
    let mut vars = vec![
        (ACTOR_ENV.to_string(), actor_for(env.conversation_id)),
        (
            IMPLEMENT_CONVERSATION_ENV.to_string(),
            env.conversation_id.to_string(),
        ),
    ];
    if let Ok(node) = node_id(env) {
        vars.push((IMPLEMENT_NODE_ENV.to_string(), node.to_string()));
    }
    vars
}

/// The state and the one after it, recorded on the conversation. The last
/// state has no next; it is recorded as itself.
fn state_and_next(fleet: &FleetStore, focus: Focus) -> Option<(String, String)> {
    let (state, next) = current_and_next(fleet, focus)?;
    let next = next.unwrap_or_else(|| state.clone());
    Some((state, next))
}

fn opening_for(
    env: &ProtocolEnv<'_>,
    recipe: &crate::context_recipes::ContextRecipe,
    purpose: &str,
) -> Result<String> {
    let (state, to) = transition(env)?;
    let node = node_id(env)?;
    let request = build_request(env, node, &state, &to, Vec::new())?;
    let tail = format!("\n{}", status(env, node, &state)?);
    build_phase_message(env.media, recipe, &request, &role_doc(&state)?, purpose, &tail)
}

/// What the runner sends a phase agent it (re)starts, and what the loop sends
/// when a turn ends with the phase unfinished: the phase status, which says
/// what the gate still lacks and what was sent back.
pub fn work_message(fleet: &FleetStore, data_root: &std::path::Path, node: Uuid) -> Result<String> {
    let state = crate::lifecycle::current_state(fleet, node)?;
    let standing = fleet.read(|conn| PhaseStanding::load(conn, node, &state))?;
    Ok(format!(
        "Carry this phase on until it is done.\n\n{}",
        render_status(&standing, independent_evaluation(data_root))
    ))
}

// ── Phase agent ─────────────────────────────────────────────────────────

pub struct PhaseProtocol;

impl Protocol for PhaseProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Phase
    }

    fn surface(&self) -> &'static str {
        crate::session_name::PHASE_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some("Do this phase's work.")
    }

    fn transition(&self, fleet: &FleetStore, focus: Focus) -> Option<(String, String)> {
        state_and_next(fleet, focus)
    }

    /// The node's working directory when it has one: design and planning
    /// read the code. An empty directory otherwise.
    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<Workdir> {
        focus_cwd_or_scratch(env, "phase")
    }

    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        phase_env(env)
    }

    /// Obligations are what the phases work on and the gates check, and
    /// a node holds them only with the spec capability. A lifecycle node
    /// made without it (a new task in the workbench) could never leave
    /// `proposed`, so it is turned on here rather than left to the agent.
    fn prepare(&self, env: &ProtocolEnv<'_>) -> Result<()> {
        let node = node_id(env)?;
        let has_spec = env.fleet.read(|conn| {
            Ok(NodeRepo::new(conn)
                .list_capabilities(node)?
                .contains(&Capability::Spec))
        })?;
        if !has_spec {
            // Through the writer's synchronous path: a queued mutation waits
            // out the debounce, and the agent's first obligation would not.
            env.fleet.interview(
                ACTOR_USER,
                InterviewCommand::Outline {
                    mutation: OutlineMutation::EnableCapabilities {
                        node_id: node,
                        capabilities: vec![Capability::Spec],
                    },
                    target: None,
                },
            )?;
        }
        Ok(())
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        opening_for(env, &PHASE, "phase")
    }

    /// The user's own edits and reversals since the last turn, as the
    /// outline conversation reports them.
    fn delta(
        &self,
        env: &ProtocolEnv<'_>,
        since_action_id: i64,
        reported: &mut ReportedStale,
    ) -> Result<String> {
        env.fleet
            .read(|conn| delta(conn, env.conversation_id, since_action_id, reported))
    }

    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        _budget_tokens: i64,
        _before_seq: Option<i64>,
    ) -> Result<String> {
        self.opening(env)
    }

    fn loops(&self) -> bool {
        true
    }

    fn progress(&self, env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        let (state, _) = transition(env)?;
        let node = node_id(env)?;
        let standing = env.fleet.read(|conn| PhaseStanding::load(conn, node, &state))?;
        Ok(Some(standing.marker))
    }

    /// Done once the gate passes or the phase waits on an evaluator; handed
    /// back while one of its questions is unanswered. Otherwise another turn,
    /// with the phase status.
    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        if let Some(done) = hand_back_for_pending_decision(turn)? {
            return Ok(done);
        }
        let env = turn.env;
        let (state, _) = transition(env)?;
        let node = node_id(env)?;
        if crate::lifecycle::current_state(env.fleet, node)? != state {
            return Ok(Next::Done(Stop::Complete));
        }
        let standing = env.fleet.read(|conn| PhaseStanding::load(conn, node, &state))?;
        if standing.step() != PhaseStep::Work {
            return Ok(Next::Done(Stop::Complete));
        }
        if let Some(done) = cap_or_stall(turn) {
            return Ok(done);
        }
        Ok(Next::Continue {
            message: format!(
                "The phase is not finished. Carry it on now, in this turn: do what \
                 is left, then finish it as the status below says. If only the \
                 user can supply what is missing, ask them through the \
                 `decisions` noun and end your turn.\n\n{}",
                render_status(&standing, independent_evaluation(env.data_root))
            ),
            reason: "phase not finished".to_string(),
        })
    }
}

// ── Independent evaluator ───────────────────────────────────────────────

pub struct EvaluateProtocol;

/// Whether `conversation` recorded a verdict (certify or reject) on the
/// node's current stay in `state`.
fn verdict_recorded(conn: &Connection, node: Uuid, state: &str, conversation: Uuid) -> Result<bool> {
    Ok(PhaseRepo::new(conn)
        .events_in_stay(node, state)?
        .iter()
        .any(|e| {
            e.conversation_id == Some(conversation)
                && (e.kind == PHASE_CERTIFY || e.kind == PHASE_REJECT)
        }))
}

impl Protocol for EvaluateProtocol {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Evaluate
    }

    fn surface(&self) -> &'static str {
        crate::session_name::EVALUATE_SURFACE
    }

    fn starter(&self) -> Option<&'static str> {
        Some("Evaluate whether this phase is done.")
    }

    fn transition(&self, fleet: &FleetStore, focus: Focus) -> Option<(String, String)> {
        state_and_next(fleet, focus)
    }

    fn cwd(&self, env: &ProtocolEnv<'_>) -> Result<Workdir> {
        focus_cwd_or_scratch(env, "evaluate")
    }

    fn turn_env(&self, env: &ProtocolEnv<'_>) -> Vec<(String, String)> {
        phase_env(env)
    }

    fn opening(&self, env: &ProtocolEnv<'_>) -> Result<String> {
        opening_for(env, &EVALUATE, "evaluate")
    }

    fn resume_snapshot(
        &self,
        env: &ProtocolEnv<'_>,
        _budget_tokens: i64,
        _before_seq: Option<i64>,
    ) -> Result<String> {
        self.opening(env)
    }

    fn loops(&self) -> bool {
        true
    }

    fn progress(&self, env: &ProtocolEnv<'_>) -> Result<Option<String>> {
        let (state, _) = transition(env)?;
        let node = node_id(env)?;
        let standing = env.fleet.read(|conn| PhaseStanding::load(conn, node, &state))?;
        Ok(Some(standing.marker))
    }

    fn next(&self, turn: &TurnContext<'_>) -> Result<Next> {
        if let Some(done) = hand_back_for_pending_decision(turn)? {
            return Ok(done);
        }
        let env = turn.env;
        let (state, _) = transition(env)?;
        let node = node_id(env)?;
        if env
            .fleet
            .read(|conn| verdict_recorded(conn, node, &state, env.conversation_id))?
        {
            return Ok(Next::Done(Stop::Complete));
        }
        if let Some(done) = cap_or_stall(turn) {
            return Ok(done);
        }
        Ok(Next::Continue {
            message: "You have not recorded a verdict. Finish the evaluation now: \
                      certify the phase, reject it with the fixes an agent can make, \
                      or ask the user what only they can answer."
                .to_string(),
            reason: "no verdict recorded".to_string(),
        })
    }
}

// ── Mock agents ─────────────────────────────────────────────────────────

/// Plays the phase agent for `--agent mock`, acting as the conversation.
/// `proposed` with no requirements asks the user what the node is for (free
/// text) and turns the answer into a requirement; `planning` with no plan
/// adds a step satisfying every requirement; `learn` records a
/// retrospective. Then it finishes the phase: `ready` with independent
/// evaluation on, else `certify`.
pub fn mock_turn(
    access: &impl super::mock::Access,
    node: Uuid,
    conversation: Uuid,
) -> Result<String> {
    let state = access
        .read(|conn| Ok(NodeRepo::new(conn).get_lifecycle(node)?))?
        // A node never given a state is `proposed`, as `FleetStore::get_node` reads it.
        .unwrap_or_else(|| "proposed".to_string());
    let standing = access.read(|conn| PhaseStanding::load(conn, node, &state))?;
    if standing.step() != PhaseStep::Work {
        return Ok(String::new());
    }
    let requirements: Vec<Uuid> = access.read(|conn| {
        Ok(ObligationRepo::new(conn)
            .list_for_node(node)?
            .into_iter()
            .filter(|o| o.kind == KIND_REQUIREMENT)
            .map(|o| o.id)
            .collect())
    })?;
    match state.as_str() {
        "proposed" if requirements.is_empty() => {
            let asked = access.read(|conn| {
                Ok(DecisionRepo::new(conn)
                    .list_for_node(node)?
                    .into_iter()
                    .filter(|d| d.conversation_id == Some(conversation))
                    .collect::<Vec<_>>())
            })?;
            if asked.iter().any(|d| d.status != DECISION_ANSWERED) {
                return Ok(String::new());
            }
            let answer = match asked.last() {
                Some(decision) => access.read(|conn| {
                    Ok(DecisionRepo::new(conn)
                        .get_with_answers(decision.id)?
                        .and_then(|d| d.answers.last().and_then(|a| a.text.clone())))
                })?,
                None => None,
            };
            let Some(answer) = answer.filter(|a| !a.trim().is_empty()) else {
                access.interview(InterviewCommand::AskDecision {
                    node_id: node,
                    conversation_id: Some(conversation),
                    protocol: Some(ProtocolKind::Phase.as_str().to_string()),
                    decision: NewDecision {
                        question: "What is this node for?".to_string(),
                        options: Vec::new(),
                        evidence: Vec::new(),
                        reason: REASON_INTENT.to_string(),
                    },
                })?;
                return Ok(String::new());
            };
            access.interview(InterviewCommand::Outline {
                mutation: OutlineMutation::CreateObligation {
                    obligation_id: Some(Uuid::new_v4()),
                    node_id: node,
                    kind: KIND_REQUIREMENT.into(),
                    after_id: None,
                    before: false,
                    section: None,
                    body: answer.trim().to_string(),
                    phase: PHASE_REQUIREMENTS.into(),
                },
                target: None,
            })?;
        }
        "planning" => {
            let steps = access.read(|conn| Ok(PlanStepRepo::new(conn).list_for_node(node)?))?;
            let step_id = match steps.first() {
                Some(step) => step.id,
                None => {
                    let step_id = Uuid::new_v4();
                    access.interview(InterviewCommand::Outline {
                        mutation: OutlineMutation::CreatePlanStep {
                            step_id: Some(step_id),
                            node_id: node,
                            after_id: None,
                            before: false,
                            body: "Build it.".into(),
                        },
                        target: None,
                    })?;
                    step_id
                }
            };
            // Every requirement no step delivers yet goes to the first step.
            for obligation_id in &requirements {
                let traced = access.read(|conn| {
                    Ok(!PlanStepRepo::new(conn)
                        .list_steps_for_obligation(*obligation_id)?
                        .is_empty())
                })?;
                if !traced {
                    access.interview(InterviewCommand::Outline {
                        mutation: OutlineMutation::LinkPlanStepObligation {
                            step_id,
                            obligation_id: *obligation_id,
                        },
                        target: None,
                    })?;
                }
            }
        }
        "learn" => {
            access.interview(InterviewCommand::RecordLearnOutput {
                node_id: node,
                content: "Mock retrospective: nothing to learn.".to_string(),
            })?;
        }
        _ => {}
    }
    if !is_certifiable(&state) {
        return Ok(String::new());
    }
    let command = if independent_evaluation(&access.data_root()) {
        InterviewCommand::PhaseReady {
            node_id: node,
            state: state.clone(),
        }
    } else {
        InterviewCommand::PhaseCertify {
            node_id: node,
            state: state.clone(),
            note: "Mock: the phase is done.".to_string(),
        }
    };
    access.interview(command)?;
    Ok(String::new())
}

/// Plays the evaluator for `--agent mock`: certifies, unless the node's
/// title contains `[reject]`, when it sends the phase back with a fix the
/// mock phase agent will not make, so the runner's loop guard can be seen.
pub fn mock_evaluate_turn(access: &impl super::mock::Access, node: Uuid) -> Result<String> {
    let (state, title) = access.read(|conn| {
        let repo = NodeRepo::new(conn);
        Ok((
            repo.get_lifecycle(node)?,
            repo.get(node)?.map(|n| n.title).unwrap_or_default(),
        ))
    })?;
    let state = state.unwrap_or_else(|| "proposed".to_string());
    let command = if title.contains("[reject]") {
        InterviewCommand::PhaseReject {
            node_id: node,
            state,
            fixes: vec!["Mock fix: the title asks for a rejection.".to_string()],
        }
    } else {
        InterviewCommand::PhaseCertify {
            node_id: node,
            state,
            note: "Mock: every item on the checklist holds.".to_string(),
        }
    };
    access.interview(command)?;
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::test_support::fixture;
    use crate::media::MediaPaths;
    use tod_store::outline::CreatePosition;

    /// A lifecycle node made without the spec capability gets it before
    /// its phase agent's first turn, so the obligations it writes land.
    #[test]
    fn a_phase_turn_turns_on_the_spec_capability() {
        let fx = fixture();
        let list_id = fx.fleet.list_outline_lists().unwrap()[0].id;
        let node = Uuid::new_v4();
        fx.fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "New task".into(),
            })
            .unwrap();
        fx.fleet
            .interview(
                ACTOR_USER,
                InterviewCommand::Outline {
                    mutation: OutlineMutation::EnableCapabilities {
                        node_id: node,
                        capabilities: vec![Capability::Lifecycle],
                    },
                    target: None,
                },
            )
            .unwrap();
        let caps = |fx: &crate::interview::test_support::Fixture| {
            fx.fleet
                .read(|conn| Ok(NodeRepo::new(conn).list_capabilities(node)?))
                .unwrap()
        };
        assert_eq!(caps(&fx), vec![Capability::Lifecycle]);

        let media = MediaPaths::discover().expect("media paths");
        let env = ProtocolEnv {
            fleet: &fx.fleet,
            media: &media,
            data_root: &fx.root,
            conversation_id: Uuid::new_v4(),
            focus: Focus::Node(node),
        };
        PhaseProtocol.prepare(&env).unwrap();
        assert!(caps(&fx).contains(&Capability::Spec));
        assert!(caps(&fx).contains(&Capability::Lifecycle));
        // Again: nothing to do.
        PhaseProtocol.prepare(&env).unwrap();
    }
}
