//! What the lifecycle does without an agent: advancing once every recorded
//! gate criterion reads pass/waived, forcing or reverting a transition, and deciding whether a state's agent starts on entry.
//!
//! Plain functions over [`FleetStore`], with no UI state. [`crate::autopilot`]
//! calls them headless, and the runner menu's Check now calls
//! [`check_now`]. `tod_ui::views::lifecycle_control::LifecycleController`
//! only keeps what the task panel shows of the last gate check. There is no
//! way to waive a criterion: the user moves a node on with the task panel's
//! lifecycle dropdown. A `waived` outcome already stored still reads as
//! passing.
//!
//! Which step comes next is [`crate::lifecycle_next`]; whether the current
//! state still holds is [`crate::lifecycle_validity`].

mod check_now;
pub mod operator;
pub use check_now::{CheckNow, check_now};

pub mod send_back;

use crate::task::model::{next_lifecycle, previous_lifecycle};
use anyhow::{Context, Result};
use tod_store::fleet::FleetStore;
use tod_store::outline::{
    GateCriterion, NodeGateEvaluation, OUTCOME_PASS, OUTCOME_WAIVED, OutlineMutation, SOURCE_HUMAN,
};
use uuid::Uuid;

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
    push_to_ticket(fleet, node, state);
    Ok(())
}

/// [`set_lifecycle`], by the user's own choice: the node's activity shows the
/// change as theirs.
pub fn set_lifecycle_by_user(fleet: &FleetStore, node: Uuid, state: &str) -> Result<()> {
    set_lifecycle_by_user_because(fleet, node, state, None)
}

/// [`set_lifecycle_by_user`] with the reason kept on the timeline entry. A
/// write that did not land is an error: the caller (the chip, a request's
/// answer) must not go on as if the node had moved.
pub fn set_lifecycle_by_user_because(fleet: &FleetStore, node: Uuid, state: &str, reason: Option<&str>) -> Result<()> {
    fleet
        .enqueue_outline(operator::user_move(node, state, reason))
        .map_err(|err| anyhow::anyhow!("{err:#}"))?;
    fleet
        .writer()
        .flush()
        .map_err(|err| anyhow::anyhow!("the move to `{state}` was not written: {err:#}"))?;
    push_to_ticket(fleet, node, state);
    Ok(())
}

/// Run the node's current state again as if it had just entered it: a new
/// stay starts in the store (what is counted per stay, such as the phase
/// conversation, a certificate or review rounds, starts over) and returns the
/// state. Nothing runs; the caller starts the runner with a new session.
pub fn restart_stay(fleet: &FleetStore, node: Uuid) -> Result<String> {
    let state = current_state(fleet, node)?;
    fleet
        .enqueue_outline(OutlineMutation::RestartLifecycleStay { node_id: node })
        .map_err(|err| anyhow::anyhow!("{err:#}"))?;
    let _ = fleet.writer().flush();
    Ok(state)
}

/// Brings the node's Linear ticket up to the state it entered.
fn push_to_ticket(fleet: &FleetStore, node: Uuid, state: &str) {
    if let Some(milestone) = crate::linear_sync::Milestone::for_lifecycle(state) {
        crate::linear_sync::push(fleet, node, milestone);
    }
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
    push_to_ticket(fleet, node, next);
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
/// `active` gate requires (`crate::gate::derived`). `Ok(None)`: the node's
/// own worktree or sandbox is not made yet, and the first turn makes it.
/// `Err` carries the user-facing reason. Reads the store only, so the UI
/// thread may call it.
pub fn implement_directory(
    fleet: &FleetStore,
    task_id: &str,
) -> std::result::Result<Option<tod_store::fleet::Workdir>, String> {
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
    tod_store::fleet::launch_cwd_if_made(fleet, task_id).map_err(|err| format!("{err:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::outline::repos::gate::ACTION_NONE;

    #[test]
    fn passing_then_advancing_moves_the_node() {
        let fx = crate::interview::test_support::fixture();
        // `ready` → `active` has one app-answered criterion; none recorded yet.
        set_lifecycle(&fx.fleet, fx.node, "ready").unwrap();
        let rows = recorded_criteria(&fx.fleet, fx.node).unwrap();
        assert!(!rows.is_empty());
        assert_eq!(advance_after_criteria(&fx.fleet, fx.node).unwrap(), None);
        for row in &rows {
            fx.fleet
                .enqueue_outline(OutlineMutation::ApplyGateResults {
                    node_id: fx.node,
                    results: vec![(
                        row.criterion.id,
                        OUTCOME_PASS.to_string(),
                        None,
                        ACTION_NONE.to_string(),
                    )],
                    forward_state: None,
                    source: SOURCE_HUMAN.to_string(),
                })
                .unwrap();
            let _ = fx.fleet.writer().flush();
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
    /// Sending a node back restarts from there: the reports certifying the
    /// states redone stop counting, and those of earlier states stay.
    #[test]
    fn moving_back_drops_the_reports_certifying_the_states_redone() {
        use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
        use tod_store::interview::{ACTOR_USER, InterviewCommand};
        let fx = crate::interview::test_support::fixture();
        let node = fx.node;
        let make = |protocol| {
            let id = Uuid::new_v4();
            fx.fleet
                .interview(
                    ACTOR_USER,
                    InterviewCommand::CreateConversation {
                        id,
                        protocol,
                        focus: Focus::Node(node),
                        platform: None,
                        model: None,
                        effort: None,
                    },
                )
                .unwrap();
            fx.fleet
                .interview(
                    ACTOR_USER,
                    InterviewCommand::RecordConversationReport {
                        conversation_id: id,
                        body: serde_json::json!({"review": "done"}),
                    },
                )
                .unwrap();
            id
        };
        let review = make(ProtocolKind::Review);
        let implement = make(ProtocolKind::Implementation);
        let has_report = |id| {
            fx.fleet
                .read(|conn| Ok(ConversationRepo::new(conn).latest_report(id)?.is_some()))
                .unwrap()
        };

        set_lifecycle(&fx.fleet, node, "pr").unwrap();
        // Moving forward, or staying put, drops nothing.
        set_lifecycle(&fx.fleet, node, "approved").unwrap();
        set_lifecycle(&fx.fleet, node, "approved").unwrap();
        assert!(has_report(review) && has_report(implement));

        // Back to `review`: the review is redone, the implementation stands.
        revert_to(&fx.fleet, node, "review").unwrap();
        assert!(!has_report(review));
        assert!(has_report(implement));

        // Further back to `active`: the implementation is redone too.
        revert_to(&fx.fleet, node, "active").unwrap();
        assert!(!has_report(implement));
    }
}
