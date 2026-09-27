//! What the lifecycle does without an agent: advancing once every recorded
//! gate criterion reads pass/waived, waiving a criterion, forcing or reverting
//! a transition, and deciding whether a state's agent starts on entry.
//!
//! Plain functions over [`FleetStore`], with no UI state: the lifecycle
//! panel and the conversation view reach them through
//! `tod_ui::views::lifecycle_control::LifecycleController`, which keeps only
//! what is shown (status lines, two-click confirmations), and
//! [`crate::autopilot`] calls them headless.
//!
//! Which step comes next is [`crate::lifecycle_next`]; whether the current
//! state still holds is [`crate::lifecycle_validity`].

use crate::task::model::{next_lifecycle, previous_lifecycle, state_has_agent};
use anyhow::{Context, Result};
use tod_store::fleet::FleetStore;
use tod_store::outline::repos::gate::ACTION_NONE;
use tod_store::outline::{
    GateCriterion, NodeGateEvaluation, OUTCOME_PASS, OUTCOME_WAIVED, OutlineMutation, SOURCE_HUMAN,
};
use uuid::Uuid;

/// What a waived criterion's detail says.
pub const WAIVED_DETAIL: &str = "Waived by user";

/// The node's current lifecycle state, as the store has it now.
pub fn current_state(fleet: &FleetStore, node: Uuid) -> Result<String> {
    Ok(fleet
        .get_node(&node.to_string())?
        .with_context(|| format!("node {node} not found"))?
        .lifecycle)
}

/// Record `state` as the node's lifecycle, bypassing every gate.
pub fn set_lifecycle(fleet: &FleetStore, node: Uuid, state: &str) -> Result<()> {
    fleet
        .enqueue_outline(OutlineMutation::SetLifecycle {
            node_id: node,
            state: state.to_string(),
        })
        .map_err(|err| anyhow::anyhow!("{err:#}"))?;
    let _ = fleet.writer().flush();
    Ok(())
}

/// Move the node to its next state, bypassing the gate criteria. Returns the
/// state entered; `None` when it is already in the last state.
pub fn force_advance(fleet: &FleetStore, node: Uuid) -> Result<Option<&'static str>> {
    let Some(next) = next_lifecycle(&current_state(fleet, node)?) else {
        return Ok(None);
    };
    set_lifecycle(fleet, node, next)?;
    Ok(Some(next))
}

/// Move the node one state back. Returns the state entered; `None` when it is
/// in the first state (or an unknown one).
pub fn revert(fleet: &FleetStore, node: Uuid) -> Result<Option<&'static str>> {
    let Some(prev) = previous_lifecycle(&current_state(fleet, node)?) else {
        return Ok(None);
    };
    set_lifecycle(fleet, node, prev)?;
    Ok(Some(prev))
}

/// Move the node straight back to `target`, several states if need be.
pub fn revert_to(fleet: &FleetStore, node: Uuid, target: &str) -> Result<()> {
    set_lifecycle(fleet, node, target)
}

/// Waive one gate criterion for the node (`SOURCE_HUMAN`). Never advances by
/// itself: see [`advance_after_criteria`].
pub fn waive(fleet: &FleetStore, node: Uuid, criterion: Uuid) -> Result<()> {
    fleet
        .enqueue_outline(OutlineMutation::ApplyGateResults {
            node_id: node,
            results: vec![(
                criterion,
                OUTCOME_WAIVED.to_string(),
                Some(WAIVED_DETAIL.to_string()),
                ACTION_NONE.to_string(),
            )],
            forward_state: None,
            source: SOURCE_HUMAN.to_string(),
        })
        .map_err(|err| anyhow::anyhow!("{err:#}"))?;
    let _ = fleet.writer().flush();
    Ok(())
}

/// Move the node to its next state, recording the advance as the gate's
/// (`ApplyGateResults` with no rows). The caller has already decided the
/// criteria allow it. Returns the state entered.
pub fn advance(fleet: &FleetStore, node: Uuid) -> Result<Option<&'static str>> {
    let Some(next) = next_lifecycle(&current_state(fleet, node)?) else {
        return Ok(None);
    };
    fleet
        .enqueue_outline(OutlineMutation::ApplyGateResults {
            node_id: node,
            results: Vec::new(),
            forward_state: Some(next.to_string()),
            source: SOURCE_HUMAN.to_string(),
        })
        .map_err(|err| anyhow::anyhow!("{err:#}"))?;
    let _ = fleet.writer().flush();
    Ok(Some(next))
}

/// One criterion of the node's forward transition and what was last recorded
/// for it (`None` when it has never been evaluated).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedCriterion {
    pub criterion: GateCriterion,
    pub evaluation: Option<NodeGateEvaluation>,
}

impl RecordedCriterion {
    /// Recorded, and pass or waived.
    pub fn is_clear(&self) -> bool {
        self.evaluation
            .as_ref()
            .is_some_and(|e| e.outcome == OUTCOME_PASS || e.outcome == OUTCOME_WAIVED)
    }
}

/// The criteria of the node's forward transition (current state to the next),
/// each with what was last recorded for it. Empty when the gate has none or
/// the node is in its last state.
pub fn recorded_criteria(fleet: &FleetStore, node: Uuid) -> Result<Vec<RecordedCriterion>> {
    let lifecycle = current_state(fleet, node)?;
    let Some(next) = next_lifecycle(&lifecycle) else {
        return Ok(Vec::new());
    };
    Ok(fleet
        .gate_criteria_for_transition(node, &lifecycle, next)?
        .into_iter()
        .map(|(criterion, evaluation)| RecordedCriterion {
            criterion,
            evaluation,
        })
        .collect())
}

/// Whether recorded rows allow the advance: at least one criterion was
/// recorded, and every one recorded reads pass/waived. The same rule the
/// lifecycle panel's Advance follows.
pub fn all_clear(rows: &[RecordedCriterion]) -> bool {
    let recorded: Vec<_> = rows.iter().filter(|r| r.evaluation.is_some()).collect();
    !recorded.is_empty() && recorded.iter().all(|r| r.is_clear())
}

/// Advance the node once every recorded criterion of its forward transition
/// reads pass/waived; `Ok(None)` (and nothing written) otherwise.
pub fn advance_after_criteria(fleet: &FleetStore, node: Uuid) -> Result<Option<&'static str>> {
    if !all_clear(&recorded_criteria(fleet, node)?) {
        return Ok(None);
    }
    advance(fleet, node)
}

/// Where implementation (and verification) of `task_id` would run: the node
/// needs a resolved Agent and a ready Files directory — what the `ready` →
/// `active` gate requires (`crate::gate::derived`). `Err` carries the
/// user-facing reason.
pub fn implement_directory(
    fleet: &FleetStore,
    task_id: &str,
) -> std::result::Result<tod_store::fleet::Workdir, String> {
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
    state_has_agent(state) && !matches!(state, "active" | "verifying" | "review")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_entry_agents_skip_the_states_with_their_own_conversations() {
        assert!(enters_with_agent("planning"));
        assert!(enters_with_agent("design"));
        for state in ["active", "verifying", "review", "ready", "done", "approved"] {
            assert!(!enters_with_agent(state), "{state}");
        }
    }

    #[test]
    fn waiving_then_advancing_moves_the_node() {
        let fx = crate::interview::test_support::fixture();
        // `ready` → `active` has one app-answered criterion; record it failing.
        set_lifecycle(&fx.fleet, fx.node, "ready").unwrap();
        let rows = recorded_criteria(&fx.fleet, fx.node).unwrap();
        assert!(!rows.is_empty());
        assert_eq!(advance_after_criteria(&fx.fleet, fx.node).unwrap(), None);
        for row in &rows {
            waive(&fx.fleet, fx.node, row.criterion.id).unwrap();
        }
        assert!(all_clear(&recorded_criteria(&fx.fleet, fx.node).unwrap()));
        assert_eq!(
            advance_after_criteria(&fx.fleet, fx.node).unwrap(),
            Some("active")
        );
        assert_eq!(current_state(&fx.fleet, fx.node).unwrap(), "active");
        assert_eq!(revert(&fx.fleet, fx.node).unwrap(), Some("ready"));
        revert_to(&fx.fleet, fx.node, "proposed").unwrap();
        assert_eq!(revert(&fx.fleet, fx.node).unwrap(), None);
        assert_eq!(force_advance(&fx.fleet, fx.node).unwrap(), Some("design"));
    }
}
