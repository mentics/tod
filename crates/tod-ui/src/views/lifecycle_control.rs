//! The lifecycle's moving parts, shared by every surface that drives them —
//! the lifecycle panel and the conversation view's lifecycle buttons. One
//! entity per window holds each node's gate-check display state.
//!
//! The agent runs are not here. A **gate check** and a state's **on-entry**
//! work are conversations (`tod_core::conversation::gate_check`), run and
//! driven by the conversation view like Implement and Review: they have a
//! transcript, show in the picker with the transition they belong to, and take
//! follow-up messages. What is left here is what the lifecycle does without an
//! agent: waiving a criterion, advancing once every recorded criterion reads
//! pass/waived ([`LifecycleController::advance_after_criteria`]), forcing or
//! reverting a transition, and showing the criteria the last check recorded.
//!
//! Criteria the app can answer from its own data
//! (`tod_core::gate::evaluate_derived_criterion` — e.g. `ready` → `active`'s
//! "has Agent and Files configured") are evaluated directly and saved as
//! `derived`; they are never sent to the agent.
//!
//! Observers are notified whenever anything shown changes, including the
//! node's lifecycle, so they re-read it from the store.

use gpui::Context;
use std::collections::HashMap;
use std::sync::Arc;
use tod_core::gate::GateAction;
use tod_core::task::model::{next_lifecycle, previous_lifecycle};
use tod_store::fleet::FleetStore;
use tod_store::outline::OutlineMutation;
use tod_store::outline::{
    GateCriterion, NodeGateEvaluation, OUTCOME_PASS, OUTCOME_WAIVED, SOURCE_HUMAN,
};
use uuid::Uuid;

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

/// Gate-check display state for one node: the criteria rows the last check
/// recorded, and the lifecycle actions' own status lines.
#[derive(Default)]
pub struct GateCheckState {
    pub gate_status: String,
    pub gate_error: Option<String>,
    pub criteria_detail: Vec<CriterionOutcome>,
    /// The criteria catalog for the node's forward transition, kept around
    /// so criteria_detail rows can show a real label.
    criteria_catalog: Vec<GateCriterion>,
    /// Set after one click on **Revert** — a second click while armed
    /// actually applies it.
    pub revert_armed: bool,
    /// Same two-click confirm as `revert_armed`, for **Force advance**.
    pub force_advance_armed: bool,
}

impl GateCheckState {
    /// The recorded criteria all read pass/waived, so Advance can go ahead.
    pub fn all_clear(&self) -> bool {
        !self.criteria_detail.is_empty() && self.criteria_detail.iter().all(|r| !r.is_failing())
    }
}

pub struct LifecycleController {
    fleet: Arc<FleetStore>,
    gate_states: HashMap<String, GateCheckState>,
}

impl LifecycleController {
    pub fn new(fleet: Arc<FleetStore>) -> Self {
        Self {
            fleet,
            gate_states: HashMap::new(),
        }
    }

    pub fn state(&self, task_id: &str) -> Option<&GateCheckState> {
        self.gate_states.get(task_id)
    }

    /// Clear both two-click confirmations, e.g. when a surface shows the
    /// node afresh.
    pub fn disarm(&mut self, task_id: &str) {
        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.revert_armed = false;
            state.force_advance_armed = false;
        }
    }

    /// Re-read the criteria rows the last check recorded, replacing what is
    /// shown — after the app settled criteria itself, with no agent run.
    pub fn reload_criteria(&mut self, task_id: &str, cx: &mut Context<Self>) {
        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.criteria_detail.clear();
            state.gate_error = None;
        }
        self.load_persisted(task_id);
        cx.notify();
    }

    /// Drop the state kept for `task_id`.
    pub fn forget(&mut self, task_id: &str) {
        self.gate_states.remove(task_id);
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
    /// gate criteria. First call arms; a second call while armed applies it,
    /// returning the state entered — where the caller starts that state's
    /// on-entry conversation (see [`enters_with_agent`]).
    pub fn force_advance(&mut self, task_id: &str, cx: &mut Context<Self>) -> Option<&'static str> {
        let (lifecycle, _) = self.node(task_id)?;
        let next = next_lifecycle(&lifecycle)?;
        let node_id = Uuid::parse_str(task_id).ok()?;
        let state = self.gate_states.entry(task_id.to_string()).or_default();
        if !state.force_advance_armed {
            state.force_advance_armed = true;
            state.gate_error = None;
            state.gate_status = format!(
                "Click Force advance again to confirm — skips the gate criteria, moves to {next}."
            );
            cx.notify();
            return None;
        }
        state.force_advance_armed = false;
        if let Err(err) = self.set_lifecycle(node_id, next) {
            let state = self.gate_states.entry(task_id.to_string()).or_default();
            state.gate_error = Some(format!("Failed to advance lifecycle: {err}"));
            cx.notify();
            return None;
        }
        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.gate_status = format!("Advanced to {next} (gate bypassed).");
            state.criteria_detail.clear();
        }
        cx.notify();
        Some(next)
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
    /// write — the criteria results are already recorded. Returns the state
    /// entered, where the caller starts its on-entry conversation.
    pub fn advance_after_criteria(
        &mut self,
        task_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<&'static str> {
        if !self.state(task_id).is_some_and(GateCheckState::all_clear) {
            return None;
        }
        let node_id = Uuid::parse_str(task_id).ok()?;
        let (lifecycle, _) = self.node(task_id)?;
        let next = next_lifecycle(&lifecycle)?;

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
            return None;
        }
        let _ = self.fleet.writer().flush();

        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.gate_status = format!("Advanced to {next}.");
            state.criteria_detail.clear();
        }
        cx.notify();
        Some(next)
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
            .is_some_and(|s| !s.criteria_detail.is_empty())
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

/// Whether landing in `state` starts that state's agent on its on-entry work:
/// the state has an agent, and is not active, verifying or review, which run
/// in their own conversations from Implement, Verify and Review. Active's own
/// on-entry step (checking whether the work is already done) is the
/// implementation loop's first turn.
pub fn enters_with_agent(state: &str) -> bool {
    tod_core::task::model::state_has_agent(state)
        && !matches!(state, "active" | "verifying" | "review")
}
