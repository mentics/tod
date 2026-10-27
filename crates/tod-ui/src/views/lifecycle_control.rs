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

use std::collections::HashMap;
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
    pub criteria_detail: Vec<CriterionOutcome>,
    /// The criteria catalog for the node's forward transition, kept around
    /// so criteria_detail rows can show a real label.
    criteria_catalog: Vec<GateCriterion>,
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

    /// The node's lifecycle and title, as the store has them now.
    fn node(&self, task_id: &str) -> Option<(String, String)> {
        self.fleet
            .get_node(task_id)
            .ok()
            .flatten()
            .map(|node| (node.lifecycle, node.title))
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
