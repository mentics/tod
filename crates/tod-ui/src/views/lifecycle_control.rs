//! The lifecycle's moving parts, shared by every surface that drives them —
//! the lifecycle panel and the conversation view's lifecycle buttons. One
//! entity per window holds each node's gate-check state, so a check started
//! from either surface shows (and keeps running) in both.
//!
//! A **gate check** sends one one-shot agent turn (mirroring
//! `tod_core::gate`'s context/response split): the state agent for the node's
//! *current* lifecycle evaluates its forward gate and replies with one
//! strict YAML document — `result`, plus, when criteria exist for the
//! transition, a `gate_results` list with per-row `outcome` and `action`.
//! When criteria are present, the reply's `gate_results` are always just
//! recorded (never auto-advances the lifecycle, even on `result: pass`): the
//! surface shows them with a Waive per failing row, and a separate **Advance**
//! ([`LifecycleController::advance_after_criteria`]) — offered only once every
//! row reads pass/waived — makes the actual lifecycle transition. A
//! prose-only gate (no criteria for the transition) advances directly off
//! the agent's `result: pass`.
//!
//! Criteria the app can answer from its own data
//! (`tod_core::gate::evaluate_derived_criterion` — e.g. `ready` → `active`'s
//! "has Agent and Files configured") are evaluated directly and saved as
//! `derived`; they are never sent to the agent, and a transition with only
//! such criteria runs no agent turn at all.
//!
//! Every transition that lands here fires the new state's **on-entry** turn
//! ([`LifecycleController::run_on_entry`]).
//!
//! Observers are notified whenever anything shown changes, including the
//! node's lifecycle, so they re-read it from the store.

use crate::interview::agent::{AgentRunState, RunId, SharedAgent};
use crate::interview::{TodPaths, TodSettings};
use gpui::{Context, Task};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tod_agent::{SessionOpening, SessionPurpose, SessionTurn};
use tod_core::gate::{
    GateAction, GateCheckRequest, PlanStepWithLinks, build_gate_check_message,
    build_on_entry_message, evaluate_derived_criterion, parse_gate_reply,
};
use tod_core::process_bundle::{ProcessManifest, TodInstallPaths, state_role_doc};
use tod_core::task::model::{next_lifecycle, previous_lifecycle, state_has_agent};
use tod_store::AgentRole;
use tod_store::fleet::FleetStore;
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::OutlineMutation;
use tod_store::outline::repos::NodeRepo;
use tod_store::outline::{
    GateCriterion, NodeGateEvaluation, OUTCOME_PASS, OUTCOME_WAIVED, SOURCE_AGENT, SOURCE_DERIVED,
    SOURCE_HUMAN,
};
use uuid::Uuid;

const POLL_INTERVAL: Duration = Duration::from_millis(300);

/// One row of per-criterion detail shown after a gate check completes.
#[derive(Debug, Clone, PartialEq)]
pub struct CriterionOutcome {
    pub criterion_id: Uuid,
    pub label: String,
    pub outcome: String,
    pub detail: Option<String>,
    /// How the user can resolve this row in-app if it's failing — reported
    /// by the agent per row. `Interview` offers an Open interview button
    /// alongside Waive; `None` leaves Waive as the only option.
    pub action: GateAction,
}

impl CriterionOutcome {
    /// Neither passed nor waived: blocks the transition.
    pub fn is_failing(&self) -> bool {
        self.outcome != OUTCOME_PASS && self.outcome != OUTCOME_WAIVED
    }
}

struct PendingGateCheck {
    run_id: Option<RunId>,
    to_state: String,
}

/// Gate-check state for one node, kept alive independent of whether that
/// node is on screen anywhere — a run keeps running (and is polled) after
/// the surface that started it moves away, and shows again when it comes
/// back before it finishes.
#[derive(Default)]
pub struct GateCheckState {
    pending: Option<PendingGateCheck>,
    pub gate_status: String,
    pub gate_error: Option<String>,
    pub criteria_detail: Vec<CriterionOutcome>,
    /// The criteria catalog fetched for the most recent gate check on this
    /// node, kept around so criteria_detail rows can show a real label.
    criteria_catalog: Vec<GateCriterion>,
    /// Rows the app evaluated itself while an agent turn evaluates the rest
    /// — merged into `criteria_detail` when that reply lands.
    derived_detail: Vec<CriterionOutcome>,
    /// Set after one click on **Revert** — a second click while armed
    /// actually applies it.
    pub revert_armed: bool,
    /// Same two-click confirm as `revert_armed`, for **Force advance**.
    pub force_advance_armed: bool,
    /// Run id of an in-flight on-entry turn — distinct from `pending` (a
    /// gate check evaluating the *forward* gate), since both can run at once.
    on_entry_run: Option<RunId>,
    /// Status line for the on-entry turn, shown alongside `gate_status`.
    pub on_entry_status: String,
}

impl GateCheckState {
    /// A gate check is in flight.
    pub fn in_flight(&self) -> bool {
        self.pending.is_some()
    }

    pub fn on_entry_running(&self) -> bool {
        self.on_entry_run.is_some()
    }

    /// The recorded criteria all read pass/waived, so Advance can go ahead.
    pub fn all_clear(&self) -> bool {
        !self.criteria_detail.is_empty() && self.criteria_detail.iter().all(|r| !r.is_failing())
    }
}

pub struct LifecycleController {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    gate_states: HashMap<String, GateCheckState>,
    _poll_task: Task<()>,
}

impl LifecycleController {
    pub fn new(cx: &mut Context<Self>, fleet: Arc<FleetStore>, agent: SharedAgent) -> Self {
        let _poll_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let Ok(()) = this.update(cx, |this, cx| {
                    if this.gate_states.values().any(|s| s.busy()) {
                        this.poll_gate_checks(cx);
                    }
                }) else {
                    break;
                };
            }
        });
        Self {
            fleet,
            agent,
            gate_states: HashMap::new(),
            _poll_task,
        }
    }

    pub fn state(&self, task_id: &str) -> Option<&GateCheckState> {
        self.gate_states.get(task_id)
    }

    /// A gate check is in flight for `task_id`.
    pub fn in_flight(&self, task_id: &str) -> bool {
        self.state(task_id).is_some_and(GateCheckState::in_flight)
    }

    /// Task ids with a gate check (or on-entry run) currently in flight —
    /// used by the app shell to warn before closing the window.
    pub fn running_task_ids(&self) -> Vec<String> {
        self.gate_states
            .iter()
            .filter(|(_, state)| state.busy())
            .map(|(task_id, _)| task_id.clone())
            .collect()
    }

    /// Task ids with a gate check or on-entry run in flight, paired with a
    /// short status — for the task list's "running" badge. These turns
    /// aren't recorded as agent runs, so without this a gate check would
    /// leave no visible trace in the task list while it runs.
    pub fn in_flight_activity(&self) -> HashMap<String, String> {
        self.gate_states
            .iter()
            .filter_map(|(task_id, state)| {
                if state.pending.is_some() {
                    Some((task_id.clone(), state.gate_status.clone()))
                } else if state.on_entry_run.is_some() {
                    Some((task_id.clone(), state.on_entry_status.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Clear both two-click confirmations, e.g. when a surface shows the
    /// node afresh.
    pub fn disarm(&mut self, task_id: &str) {
        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.revert_armed = false;
            state.force_advance_armed = false;
        }
    }

    /// Drop finished state for `task_id`; a run still in flight is kept so
    /// it can finish.
    pub fn forget(&mut self, task_id: &str) {
        if self.gate_states.get(task_id).is_some_and(|s| !s.busy()) {
            self.gate_states.remove(task_id);
        }
    }

    /// The node's lifecycle and title, as the store has them now.
    fn node(&self, task_id: &str) -> Option<(String, String)> {
        self.fleet
            .get_node(task_id)
            .ok()
            .flatten()
            .map(|node| (node.lifecycle, node.title))
    }

    /// Record `lifecycle` as the node's state. `Err` carries the message.
    fn set_lifecycle(&self, node_id: Uuid, lifecycle: &str) -> Result<(), String> {
        self.fleet
            .enqueue_outline(OutlineMutation::SetLifecycle {
                node_id,
                state: lifecycle.to_string(),
            })
            .map_err(|err| format!("{err:#}"))?;
        let _ = self.fleet.writer().flush();
        Ok(())
    }

    /// Advance the node to the next lifecycle state directly, bypassing the
    /// gate criteria. First call arms; a second call while armed applies it.
    pub fn force_advance(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let Some((lifecycle, _)) = self.node(task_id) else {
            return;
        };
        let Some(next) = next_lifecycle(&lifecycle) else {
            return;
        };
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return;
        };
        let state = self.gate_states.entry(task_id.to_string()).or_default();
        if !state.force_advance_armed {
            state.force_advance_armed = true;
            state.gate_error = None;
            state.gate_status = format!(
                "Click Force advance again to confirm — skips the gate criteria, moves to {next}."
            );
            cx.notify();
            return;
        }
        state.force_advance_armed = false;
        if let Err(err) = self.set_lifecycle(node_id, next) {
            let state = self.gate_states.entry(task_id.to_string()).or_default();
            state.gate_error = Some(format!("Failed to advance lifecycle: {err}"));
            cx.notify();
            return;
        }
        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.gate_status = format!("Advanced to {next} (gate bypassed).");
            state.criteria_detail.clear();
        }
        self.run_on_entry(task_id, next, cx);
        cx.notify();
    }

    /// Move the node one lifecycle state back, bypassing the forward gate —
    /// e.g. to send a `planning` node back to `design` so the next
    /// transition regenerates its plan steps. First call arms; a second call
    /// while armed applies it.
    pub fn revert(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let Some((lifecycle, _)) = self.node(task_id) else {
            return;
        };
        let Some(prev) = previous_lifecycle(&lifecycle) else {
            return;
        };
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return;
        };
        let state = self.gate_states.entry(task_id.to_string()).or_default();
        if !state.revert_armed {
            state.revert_armed = true;
            state.gate_error = None;
            state.gate_status = format!("Click Revert again to confirm — moves back to {prev}.");
            cx.notify();
            return;
        }
        state.revert_armed = false;
        if let Err(err) = self.set_lifecycle(node_id, prev) {
            let state = self.gate_states.entry(task_id.to_string()).or_default();
            state.gate_error = Some(format!("Failed to revert lifecycle: {err}"));
            cx.notify();
            return;
        }
        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.gate_status = format!("Reverted to {prev}.");
            state.criteria_detail.clear();
        }
        cx.notify();
    }

    /// Move the node one lifecycle state back at once, with no confirmation:
    /// for a caller whose own button already says what it does. `true` when
    /// the node moved.
    pub fn revert_now(&mut self, task_id: &str, cx: &mut Context<Self>) -> bool {
        let Some((lifecycle, _)) = self.node(task_id) else {
            return false;
        };
        let Some(prev) = previous_lifecycle(&lifecycle) else {
            return false;
        };
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return false;
        };
        let result = self.set_lifecycle(node_id, prev);
        let state = self.gate_states.entry(task_id.to_string()).or_default();
        state.revert_armed = false;
        let moved = result.is_ok();
        match result {
            Ok(()) => {
                state.gate_error = None;
                state.gate_status = format!("Reverted to {prev}.");
                state.criteria_detail.clear();
            }
            Err(err) => state.gate_error = Some(format!("Failed to revert lifecycle: {err}")),
        }
        cx.notify();
        moved
    }

    /// Waive one failing gate criterion — the fine-grained alternative to
    /// `force_advance`. Persists as `SOURCE_HUMAN`. This never advances the
    /// lifecycle by itself: once every row reads pass/waived, the user still
    /// asks for [`Self::advance_after_criteria`].
    pub fn waive(&mut self, task_id: &str, criterion_id: Uuid, cx: &mut Context<Self>) {
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return;
        };
        let Some(row) = self.gate_states.get_mut(task_id).and_then(|s| {
            s.criteria_detail
                .iter_mut()
                .find(|r| r.criterion_id == criterion_id)
        }) else {
            return;
        };
        row.outcome = OUTCOME_WAIVED.to_string();
        row.detail = Some("Waived by user".to_string());

        let saved = self
            .fleet
            .enqueue_outline(OutlineMutation::ApplyGateResults {
                node_id,
                results: vec![(
                    criterion_id,
                    OUTCOME_WAIVED.to_string(),
                    Some("Waived by user".to_string()),
                    tod_store::outline::repos::gate::ACTION_NONE.to_string(),
                )],
                forward_state: None,
                source: SOURCE_HUMAN.to_string(),
            });
        let Some(state) = self.gate_states.get_mut(task_id) else {
            return;
        };
        match saved {
            Ok(()) => {
                let _ = self.fleet.writer().flush();
                state.gate_status = if state.all_clear() {
                    "All criteria satisfied — advance when ready.".into()
                } else {
                    "Criterion waived.".into()
                };
            }
            Err(err) => state.gate_error = Some(format!("Failed to waive criterion: {err:#}")),
        }
        cx.notify();
    }

    /// Advance the node to its next lifecycle state once every recorded
    /// criterion reads pass/waived; does nothing otherwise. A pure lifecycle
    /// write — the criteria results are already recorded.
    pub fn advance_after_criteria(&mut self, task_id: &str, cx: &mut Context<Self>) {
        if !self.state(task_id).is_some_and(GateCheckState::all_clear) {
            return;
        }
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return;
        };
        let Some((lifecycle, _)) = self.node(task_id) else {
            return;
        };
        let Some(next) = next_lifecycle(&lifecycle) else {
            return;
        };

        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::ApplyGateResults {
                node_id,
                results: Vec::new(),
                forward_state: Some(next.to_string()),
                source: SOURCE_HUMAN.to_string(),
            })
        {
            if let Some(state) = self.gate_states.get_mut(task_id) {
                state.gate_error = Some(format!("Failed to advance lifecycle: {err:#}"));
            }
            cx.notify();
            return;
        }
        let _ = self.fleet.writer().flush();

        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.gate_status = format!("Advanced to {next}.");
            state.criteria_detail.clear();
        }
        self.run_on_entry(task_id, next, cx);
        cx.notify();
    }

    /// Repopulate `criteria_detail` for `task_id` from the most recently
    /// persisted gate-check evaluations for its current forward transition,
    /// so a check run before an app restart reappears without running it
    /// again. Only fills in state that's still empty. The per-row `action`
    /// isn't persisted, so a reloaded row falls back to `None`.
    pub fn load_persisted(&mut self, task_id: &str) {
        if self
            .gate_states
            .get(task_id)
            .is_some_and(|s| s.pending.is_some() || !s.criteria_detail.is_empty())
        {
            return;
        }
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return;
        };
        let Some((lifecycle, _)) = self.node(task_id) else {
            return;
        };
        let Some(to_state) = next_lifecycle(&lifecycle) else {
            return;
        };
        let Ok(rows) = self
            .fleet
            .gate_criteria_for_transition(node_id, &lifecycle, to_state)
        else {
            return;
        };
        let criteria_catalog: Vec<GateCriterion> = rows.iter().map(|(c, _)| c.clone()).collect();
        let criteria_detail: Vec<CriterionOutcome> = rows
            .iter()
            .filter_map(|(c, eval): &(GateCriterion, Option<NodeGateEvaluation>)| {
                eval.as_ref().map(|e| CriterionOutcome {
                    criterion_id: c.id,
                    label: c.label.clone(),
                    outcome: e.outcome.clone(),
                    detail: e.detail.clone(),
                    action: GateAction::None,
                })
            })
            .collect();
        if criteria_detail.is_empty() {
            return;
        }

        let state = self.gate_states.entry(task_id.to_string()).or_default();
        state.criteria_catalog = criteria_catalog;
        state.criteria_detail = criteria_detail;
        state.gate_status = if state.all_clear() {
            "All criteria satisfied — advance when ready.".into()
        } else {
            "Gate check (from last run) — see criteria below.".into()
        };
    }

    fn fail_gate_check(&mut self, task_id: &str, message: String, cx: &mut Context<Self>) {
        let state = self.gate_states.entry(task_id.to_string()).or_default();
        state.pending = None;
        state.gate_error = Some(message);
        state.gate_status = "Gate check failed".into();
        cx.notify();
    }

    /// Kick off one gate-check agent turn for the node's transition to its
    /// next lifecycle state. Does nothing while one is already in flight.
    pub fn run_gate_check(&mut self, task_id: &str, cx: &mut Context<Self>) {
        if self.in_flight(task_id) {
            return;
        }
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return;
        };
        let Some((from_state, node_title)) = self.node(task_id) else {
            return;
        };
        let Some(to_state) = next_lifecycle(&from_state).map(str::to_string) else {
            return;
        };

        {
            let state = self.gate_states.entry(task_id.to_string()).or_default();
            state.gate_error = None;
            state.criteria_detail.clear();
            state.revert_armed = false;
            state.force_advance_armed = false;
            state.gate_status = "Preparing gate check…".into();
            state.pending = Some(PendingGateCheck {
                run_id: None,
                to_state: to_state.clone(),
            });
        }
        cx.notify();

        let criteria =
            match self
                .fleet
                .gate_criteria_for_transition(node_id, &from_state, &to_state)
            {
                Ok(criteria) => criteria,
                Err(err) => return self.fail_gate_check(task_id, format!("{err:#}"), cx),
            };
        let criteria_catalog: Vec<GateCriterion> =
            criteria.iter().map(|(c, _)| c.clone()).collect();

        // Criteria the app can answer from its own data never reach the
        // agent: it isn't shown that data, so it could only guess.
        let mut derived_detail = Vec::new();
        let mut agent_criteria = Vec::new();
        for (criterion, eval) in criteria {
            match self
                .fleet
                .read(|conn| evaluate_derived_criterion(conn, node_id, &criterion))
            {
                Ok(Some(derived)) => derived_detail.push(CriterionOutcome {
                    criterion_id: criterion.id,
                    label: criterion.label.clone(),
                    outcome: derived.outcome.to_string(),
                    detail: Some(derived.detail),
                    action: GateAction::None,
                }),
                Ok(None) => agent_criteria.push((criterion, eval)),
                Err(err) => return self.fail_gate_check(task_id, format!("{err:#}"), cx),
            }
        }
        if !derived_detail.is_empty() {
            let results = derived_detail
                .iter()
                .map(|row| {
                    (
                        row.criterion_id,
                        row.outcome.clone(),
                        row.detail.clone(),
                        tod_store::outline::repos::gate::ACTION_NONE.to_string(),
                    )
                })
                .collect();
            if let Err(err) = self
                .fleet
                .enqueue_outline(OutlineMutation::ApplyGateResults {
                    node_id,
                    results,
                    forward_state: None,
                    source: SOURCE_DERIVED.to_string(),
                })
            {
                return self.fail_gate_check(
                    task_id,
                    format!("Failed to save gate check: {err:#}"),
                    cx,
                );
            }
            let _ = self.fleet.writer().flush();

            // Nothing left for an agent to judge — the table is complete.
            if agent_criteria.is_empty() {
                let state = self.gate_states.entry(task_id.to_string()).or_default();
                state.pending = None;
                state.criteria_catalog = criteria_catalog;
                state.derived_detail.clear();
                state.criteria_detail = derived_detail;
                state.gate_status = if state.all_clear() {
                    "All criteria satisfied — advance when ready.".into()
                } else {
                    "Gate check: blocked — see criteria below.".into()
                };
                cx.notify();
                return;
            }
        }
        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.derived_detail = derived_detail;
        }

        let turn = self.state_turn(
            task_id,
            node_id,
            &node_title,
            &from_state,
            &to_state,
            agent_criteria,
        );
        let sent = turn.and_then(|turn| match self.agent.lock() {
            Ok(mut provider) => provider
                .send_session_turn(turn)
                .map_err(|err| anyhow::anyhow!("Launch agent failed: {err:#}")),
            Err(_) => Err(anyhow::anyhow!("Agent busy — try again shortly")),
        });
        match sent {
            Ok(handle) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    if let Some(pending) = state.pending.as_mut() {
                        pending.run_id = Some(handle.id);
                    }
                    state.gate_status = "Running gate check…".into();
                    state.criteria_catalog = criteria_catalog;
                }
            }
            Err(err) => self.fail_gate_check(task_id, format!("{err:#}"), cx),
        }
        cx.notify();
    }

    /// The one-shot turn for the state agent of `from_state`: a gate check
    /// of the transition to `to_state`, or — when they are equal — the
    /// state's on-entry work.
    fn state_turn(
        &self,
        task_id: &str,
        node_id: Uuid,
        node_title: &str,
        from_state: &str,
        to_state: &str,
        criteria: Vec<(GateCriterion, Option<NodeGateEvaluation>)>,
    ) -> anyhow::Result<SessionTurn> {
        let paths = TodPaths::discover()?;
        let settings = TodSettings::load(&paths).unwrap_or_default();
        let cwd = self.fleet.files_dir_or_data_root(task_id);
        let options = self
            .fleet
            .resolve_agent_for_node(task_id)
            .ok()
            .flatten()
            .map(|agent| agent.launch_options(&settings, AgentRole::Default))
            .unwrap_or_else(|| settings.launch_options_for(AgentRole::Default));
        let body = self
            .fleet
            .get_extra_content(node_id, EXTRA_CONTENT_DETAILS)
            .ok()
            .flatten();
        let obligations = self
            .fleet
            .list_obligations_for_node(node_id)
            .unwrap_or_default();
        let ancestor_context = self
            .fleet
            .read(|conn| {
                tod_core::node_context::render_inherited_context(
                    conn,
                    &NodeRepo::new(conn),
                    node_id,
                    None,
                )
            })
            .unwrap_or_default();
        let plan_steps = self
            .fleet
            .list_plan_steps_for_node(node_id)
            .unwrap_or_default()
            .into_iter()
            .map(|step| {
                let depends_on = self
                    .fleet
                    .list_plan_step_dependencies(step.id)
                    .unwrap_or_default();
                let satisfies = self
                    .fleet
                    .list_plan_step_obligations(step.id)
                    .unwrap_or_default();
                PlanStepWithLinks {
                    step,
                    depends_on,
                    satisfies,
                }
            })
            .collect();
        let media = tod_core::media::MediaPaths::discover()?;
        let install = TodInstallPaths::discover()?;
        let manifest = ProcessManifest::load(&install)?;
        let role_doc = state_role_doc(&manifest, from_state)?;
        let request = GateCheckRequest {
            data_root: paths.data_root(),
            node_id,
            node_title: node_title.to_string(),
            node_lifecycle: from_state.to_string(),
            node_body: body,
            obligations,
            ancestor_context,
            plan_steps,
            from_state: from_state.to_string(),
            to_state: to_state.to_string(),
            criteria,
        };
        let on_entry = from_state == to_state;
        let (message, key, title) = if on_entry {
            (
                build_on_entry_message(&media, &request, &role_doc)?,
                format!("on-entry-{}", Uuid::new_v4()),
                format!("On entry: {node_title} ({from_state})"),
            )
        } else {
            (
                build_gate_check_message(&media, &request, &role_doc)?,
                format!("gate-check-{}", Uuid::new_v4()),
                format!("Gate check: {node_title} ({from_state} \u{2192} {to_state})"),
            )
        };
        Ok(SessionTurn {
            key,
            owner_id: task_id.to_string(),
            title,
            cwd,
            options,
            resume_session_id: None,
            opening: Some(SessionOpening { context: None }),
            message,
            purpose: SessionPurpose::Chat,
            env: Vec::new(),
        })
    }

    /// Poll every node with an in-flight gate check or on-entry run.
    fn poll_gate_checks(&mut self, cx: &mut Context<Self>) {
        let pending_ids: Vec<String> = self
            .gate_states
            .iter()
            .filter(|(_, s)| s.busy())
            .map(|(id, _)| id.clone())
            .collect();
        for task_id in pending_ids {
            self.poll_gate_check(&task_id, cx);
            self.poll_on_entry(&task_id, cx);
        }
    }

    /// Fire the on-entry turn for `task_id`, which just landed in `lifecycle`
    /// — the new state's own agent doing its state's "On entry"
    /// responsibilities (e.g. `planning` writing plan steps), per
    /// `assets/process/agents/state/base.md`. Called from every place a
    /// transition lands here. Idempotent by design (the prompt tells the
    /// agent to add only what's missing).
    fn run_on_entry(&mut self, task_id: &str, lifecycle: &str, cx: &mut Context<Self>) {
        // Verification and review run in their own conversations, from Verify
        // and Review, not as on-entry turns.
        if !state_has_agent(lifecycle) || matches!(lifecycle, "verifying" | "review") {
            return;
        }
        if self
            .gate_states
            .get(task_id)
            .is_some_and(|s| s.on_entry_run.is_some())
        {
            return;
        }
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return;
        };
        let title = self
            .node(task_id)
            .map(|(_, title)| title)
            .unwrap_or_default();
        let sent = self
            .state_turn(task_id, node_id, &title, lifecycle, lifecycle, Vec::new())
            .map_err(|err| format!("On-entry setup failed: {err:#}"))
            .and_then(|turn| match self.agent.lock() {
                Ok(mut provider) => provider
                    .send_session_turn(turn)
                    .map_err(|err| format!("On-entry setup failed to launch: {err:#}")),
                Err(_) => Err("On-entry setup failed to launch: agent busy".to_string()),
            });
        let state = self.gate_states.entry(task_id.to_string()).or_default();
        match sent {
            Ok(handle) => {
                state.on_entry_run = Some(handle.id);
                state.on_entry_status = format!("Running {lifecycle} on-entry setup…");
            }
            Err(message) => state.on_entry_status = message,
        }
        cx.notify();
    }

    fn poll_on_entry(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let Some(run_id) = self.gate_states.get(task_id).and_then(|s| s.on_entry_run) else {
            return;
        };
        let Ok(mut agent) = self.agent.try_lock() else {
            return;
        };
        let Some(run_state) = agent.poll_run(run_id) else {
            return;
        };
        drop(agent);

        match run_state {
            AgentRunState::InFlight(activity) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    state.on_entry_status =
                        activity.unwrap_or_else(|| "Running on-entry setup…".into());
                }
            }
            AgentRunState::NeedsPermission(request) => {
                crate::ui::agent_permission::queue_permission_request(self.agent.clone(), request);
            }
            AgentRunState::Success(response) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    state.on_entry_run = None;
                    let summary = response.unwrap_or_default();
                    let summary = summary.trim();
                    state.on_entry_status = if summary.is_empty() {
                        "On-entry setup complete.".into()
                    } else {
                        format!("On-entry setup: {summary}")
                    };
                }
            }
            AgentRunState::Failure(message) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    state.on_entry_run = None;
                    state.on_entry_status = format!("On-entry setup failed: {message}");
                }
            }
        }
        cx.notify();
    }

    fn poll_gate_check(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let Some(run_id) = self
            .gate_states
            .get(task_id)
            .and_then(|s| s.pending.as_ref())
            .and_then(|p| p.run_id)
        else {
            return;
        };
        let Ok(mut agent) = self.agent.try_lock() else {
            return;
        };
        let Some(run_state) = agent.poll_run(run_id) else {
            return;
        };
        drop(agent);

        let to_state = match self
            .gate_states
            .get(task_id)
            .and_then(|s| s.pending.as_ref())
        {
            Some(p) => p.to_state.clone(),
            None => return,
        };

        match run_state {
            AgentRunState::InFlight(activity) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    state.gate_status = activity.unwrap_or_else(|| "Running gate check…".into());
                }
            }
            AgentRunState::NeedsPermission(request) => {
                crate::ui::agent_permission::queue_permission_request(self.agent.clone(), request);
            }
            AgentRunState::Success(response) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    state.pending = None;
                }
                self.apply_gate_reply(task_id, response.unwrap_or_default(), &to_state, cx);
            }
            AgentRunState::Failure(message) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    state.pending = None;
                    state.gate_error = Some(message);
                    state.gate_status = "Gate check failed".into();
                }
            }
        }
        cx.notify();
    }

    fn apply_gate_reply(
        &mut self,
        task_id: &str,
        text: String,
        to_state: &str,
        cx: &mut Context<Self>,
    ) {
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return;
        };

        let reply = match parse_gate_reply(&text) {
            Ok(reply) => reply,
            Err(err) => {
                let state = self.gate_states.entry(task_id.to_string()).or_default();
                state.gate_error = Some(format!("Could not parse agent reply: {err:#}"));
                state.gate_status = "Gate check reply was not understood".into();
                return;
            }
        };

        let catalog = self
            .gate_states
            .get(task_id)
            .map(|s| s.criteria_catalog.clone())
            .unwrap_or_default();
        let label_for = |id: Uuid| {
            catalog
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.label.clone())
                .unwrap_or_else(|| id.to_string())
        };
        // Whether this transition has structured criteria at all — decides
        // whether advancing needs a separate user click or can follow the
        // agent's own verdict, for a prose-only gate.
        let has_criteria = !catalog.is_empty();

        let results: Vec<(Uuid, String, Option<String>, String)> = reply
            .gate_results
            .iter()
            .map(|row| {
                let action = if row.action == GateAction::Interview {
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
        // Rows the app evaluated itself (already saved when the check began)
        // join the agent's rows, back in catalog order.
        let derived_detail = self
            .gate_states
            .get_mut(task_id)
            .map(|s| std::mem::take(&mut s.derived_detail))
            .unwrap_or_default();
        let mut criteria_detail: Vec<CriterionOutcome> = derived_detail
            .into_iter()
            .chain(reply.gate_results.iter().map(|row| CriterionOutcome {
                criterion_id: row.criterion_id,
                label: label_for(row.criterion_id),
                outcome: row.outcome.clone(),
                detail: row.detail.clone(),
                action: row.action,
            }))
            .collect();
        criteria_detail.sort_by_key(|r| catalog.iter().position(|c| c.id == r.criterion_id));

        // A gate with structured criteria always stops here and shows them —
        // even a `result: pass` reply only records the agent's per-row
        // verdicts; advancing is a separate, explicit Advance once every row
        // is pass/waived. Only a prose-only gate advances directly off the
        // agent's own result.
        let advances = reply.result.advances() && !has_criteria;
        let forward_state = advances.then(|| to_state.to_string());

        if !results.is_empty() || advances {
            if let Err(err) = self
                .fleet
                .enqueue_outline(OutlineMutation::ApplyGateResults {
                    node_id,
                    results,
                    forward_state: forward_state.clone(),
                    source: SOURCE_AGENT.to_string(),
                })
            {
                let state = self.gate_states.entry(task_id.to_string()).or_default();
                state.gate_error = Some(format!("Failed to save gate check: {err:#}"));
                return;
            }
            let _ = self.fleet.writer().flush();
        }

        let state = self.gate_states.entry(task_id.to_string()).or_default();
        state.criteria_detail = criteria_detail;
        if let Some(new_state) = forward_state.clone() {
            state.gate_status = format!("Advanced to {new_state}.");
        } else if has_criteria {
            state.gate_status = if state.all_clear() {
                "All criteria satisfied — advance when ready.".into()
            } else if reply.paused {
                "Gate check: blocked — see criteria below.".into()
            } else {
                "Gate check did not advance the lifecycle — see criteria below.".into()
            };
        } else {
            state.gate_status = if reply.paused {
                "Gate check: blocked — see findings below.".into()
            } else {
                "Gate check did not advance the lifecycle.".into()
            };
        }
        if !reply.findings.trim().is_empty() {
            state.gate_error = None;
        }

        if let Some(new_state) = forward_state {
            self.run_on_entry(task_id, &new_state, cx);
        }
    }
}

impl GateCheckState {
    /// A gate check or on-entry turn is running.
    fn busy(&self) -> bool {
        self.pending.is_some() || self.on_entry_run.is_some()
    }
}

/// Where implementation (and verification) of `task_id` would run: the node
/// needs a resolved Agent and a ready Files directory — what the `ready` →
/// `active` gate requires (`tod_core::gate::derived`). `Err` carries the
/// user-facing reason.
pub fn implement_directory(
    fleet: &FleetStore,
    task_id: &str,
) -> Result<std::path::PathBuf, String> {
    if fleet
        .resolve_agent_for_node(task_id)
        .ok()
        .flatten()
        .is_none()
    {
        return Err(
            "Enable the Agent capability on this node (or an ancestor) to implement.".into(),
        );
    }
    tod_store::fleet::resolve_launch_cwd(fleet, task_id).map_err(|err| format!("{err:#}"))
}
