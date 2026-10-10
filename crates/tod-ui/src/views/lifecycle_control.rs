//! The lifecycle's moving parts, shared by every surface that drives them —
//! the lifecycle panel and the conversation view's lifecycle buttons. One
//! entity per window holds each node's gate-check display state.
//!
//! The agent runs are not here: a state's work, its evaluation, Implement,
//! Verify, and Review are conversations the conversation view runs. What is
//! here is what the lifecycle does without an agent: checking the gate
//! ([`LifecycleController::check_gate`], an app check through
//! `tod_core::phase::settle_gate`, which advances the node when it is clear),
//! waiving a criterion, advancing once every recorded criterion reads
//! pass/waived ([`LifecycleController::advance_after_criteria`]), forcing or
//! reverting a transition, and showing the criteria the last check recorded.
//! The store writes themselves are `tod_core`'s, shared with the headless
//! `tod_core::autopilot`; this entity keeps only what is shown.
//!
//! Observers are notified whenever anything shown changes, including the
//! node's lifecycle, so they re-read it from the store.

use crate::ui::off_thread::off_thread;
use gpui::Context;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tod_core::gate::GateAction;
use tod_core::task::model::next_lifecycle;
use tod_store::fleet::FleetStore;
use tod_store::outline::{GateCriterion, NodeGateEvaluation, OUTCOME_PASS, OUTCOME_WAIVED};
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
    /// The gate is being checked, on the background executor.
    pub checking: bool,
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
    checking: HashSet<String>,
}

impl LifecycleController {
    pub fn new(fleet: Arc<FleetStore>) -> Self {
        Self {
            fleet,
            gate_states: HashMap::new(),
            checking: HashSet::new(),
        }
    }

    /// Show where a gate check waiting on the node's incoming changes stands
    /// (`views::incoming_check`): `status` while it waits, `error` when it
    /// will not run. Both empty clears the line, as the gate check starts.
    pub fn report_before_gate(
        &mut self,
        task_id: &str,
        status: Option<String>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let state = self.gate_states.entry(task_id.to_string()).or_default();
        state.gate_status = status.unwrap_or_default();
        if error.is_some() {
            state.criteria_detail.clear();
        }
        state.gate_error = error;
        cx.notify();
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
        tod_core::lifecycle::set_lifecycle(&self.fleet, node_id, lifecycle)
            .map_err(|err| format!("{err:#}"))
    }

    /// Move the node straight back to `target`, several states if need be,
    /// with no confirmation: the caller's button names the state, and it is
    /// only offered when the node's current state no longer holds
    /// (`tod_core::lifecycle_validity`). The write is made off the UI thread.
    pub fn revert_to(&mut self, task_id: &str, target: &'static str, cx: &mut Context<Self>) {
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return;
        };
        let state = self.gate_states.entry(task_id.to_string()).or_default();
        state.revert_armed = false;
        state.force_advance_armed = false;
        let fleet = self.fleet.clone();
        let task_id = task_id.to_string();
        off_thread(
            cx,
            move || tod_core::lifecycle::set_lifecycle(&fleet, node_id, target).map_err(|e| format!("{e:#}")),
            move |this, result, cx| {
                let state = this.gate_states.entry(task_id).or_default();
                match result {
                    Ok(()) => {
                        state.gate_error = None;
                        state.gate_status = format!("Moved back to {target}.");
                        state.criteria_detail.clear();
                    }
                    Err(err) => state.gate_error = Some(format!("Failed to move back: {err}")),
                }
                cx.notify();
            },
        );
    }

    /// [`Self::revert_to`], waiting for the store and saying whether the node
    /// moved: for a caller that moves several nodes and counts them.
    pub fn revert_to_blocking(&mut self, task_id: &str, target: &str, cx: &mut Context<Self>) -> bool {
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return false;
        };
        let result = self.set_lifecycle(node_id, target);
        let state = self.gate_states.entry(task_id.to_string()).or_default();
        state.revert_armed = false;
        state.force_advance_armed = false;
        let moved = result.is_ok();
        match result {
            Ok(()) => {
                state.gate_error = None;
                state.gate_status = format!("Moved back to {target}.");
                state.criteria_detail.clear();
            }
            Err(err) => state.gate_error = Some(format!("Failed to move back: {err}")),
        }
        cx.notify();
        moved
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
            "Gate (last checked) — see criteria below.".into()
        };
    }
}

// Moved to `tod_core::lifecycle`; kept here so callers need not change.
