//! The user's own move of a node's lifecycle state, bypassing the gates.
//!
//! The task panel's phase chip and `tod-cli lifecycle set` are the same act:
//! both send [`user_move`] (the store's `SetLifecycleByUser`), which is where
//! a move back clears what certified the states being redone, reopens
//! verified verdicts, cancels what the node waited on, retakes or clears the
//! lifecycle baseline and shows on the node's activity as the user's.
//! `tod-cli` adds two rules of its own, because it is reachable from an agent
//! session: [`refuse_agent_session`] and [`check_operator_target`].

use crate::task::model::{LIFECYCLE_STATES, lifecycle_rank};
use anyhow::{Result, bail};
use tod_store::outline::OutlineMutation;
use uuid::Uuid;

/// The first state the operator command refuses to move a node into: marking
/// a pull request ready and choosing a reviewer are the user's own step.
pub const FIRST_REFUSED_STATE: &str = "pr_review";

/// Environment variables only an agent session has: the conversation's actor
/// (`tod_store::interview::ACTOR_ENV`) and the node and conversation it
/// works on (`conversation::implement`).
const AGENT_ENV: [&str; 3] = [
    tod_store::interview::ACTOR_ENV,
    crate::conversation::implement::IMPLEMENT_NODE_ENV,
    crate::conversation::implement::IMPLEMENT_CONVERSATION_ENV,
];

/// The mutation of the user's own move to `state`: the one thing the phase
/// chip and the CLI both send. `reason` is kept on the timeline entry.
pub fn user_move(node: Uuid, state: &str, reason: Option<&str>) -> OutlineMutation {
    OutlineMutation::SetLifecycleByUser {
        node_id: node,
        state: state.to_string(),
        reason: reason.map(str::to_string),
    }
}

/// An error when `env` (a lookup of environment variables) shows an agent
/// session. Nothing sets a lifecycle state except the lifecycle process, so a
/// node's agent must not move its own node.
pub fn refuse_agent_session(env: impl Fn(&str) -> Option<String>) -> Result<()> {
    for name in AGENT_ENV {
        if env(name).is_some_and(|v| !v.trim().is_empty()) {
            bail!(
                "changing a node's lifecycle state is for the operator only; it is refused in an agent \
                 session ({name} is set). Do the node's work; the lifecycle moves it on."
            );
        }
    }
    Ok(())
}

/// Whether the operator command may move a node from `current` to `target`:
/// `target` must be a lifecycle state, a different one, and before
/// [`FIRST_REFUSED_STATE`].
pub fn check_operator_target(current: &str, target: &str) -> Result<()> {
    if !LIFECYCLE_STATES.contains(&target) {
        bail!("`{target}` is not a lifecycle state (expected one of: {})", LIFECYCLE_STATES.join(", "));
    }
    if target == current {
        bail!("the node is already in `{current}`");
    }
    if lifecycle_rank(target) >= lifecycle_rank(FIRST_REFUSED_STATE) {
        bail!(
            "refusing to move a node into `{target}`: from `{FIRST_REFUSED_STATE}` on (marking the pull \
             request ready, choosing a reviewer, merging) the step is the user's, in the task panel"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_review_and_beyond_are_refused_and_earlier_states_are_not() {
        for target in ["pr_review", "approved", "merged", "released", "learn", "done"] {
            let err = check_operator_target("review", target).unwrap_err().to_string();
            assert!(err.contains("refusing to move"), "{target}: {err}");
        }
        for target in ["proposed", "design", "planning", "ready", "active", "verifying", "pr", "pr_draft"] {
            check_operator_target("review", target).unwrap_or_else(|e| panic!("{target}: {e}"));
        }
        // Moving back from beyond the line is allowed.
        check_operator_target("merged", "active").unwrap();
    }

    #[test]
    fn an_unknown_or_current_state_is_refused() {
        assert!(check_operator_target("review", "nonsense").unwrap_err().to_string().contains("not a lifecycle state"));
        assert!(check_operator_target("review", "review").unwrap_err().to_string().contains("already in"));
    }

    #[test]
    fn an_agent_session_is_refused() {
        for name in AGENT_ENV {
            let err = refuse_agent_session(|n| (n == name).then(|| "x".to_string())).unwrap_err().to_string();
            assert!(err.contains("operator only") && err.contains(name), "{err}");
        }
        refuse_agent_session(|_| None).unwrap();
        refuse_agent_session(|_| Some("  ".into())).unwrap();
    }
}
