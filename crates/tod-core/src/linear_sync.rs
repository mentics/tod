//! Keeping a node's Linear ticket at least as far along as the node is.
//!
//! The ticket only ever moves forward (see
//! [`tod_store::linear::choose_state`]): accepting a ticket puts it at least
//! in an "up next" state, starting work at least in progress, the review
//! phases in review, and merging in done. A ticket that is already further
//! along, or canceled, is left alone.
//!
//! The push is best effort and never blocks the caller: [`push`] reads the
//! node's ticket and goes to Linear on a thread of its own, and does nothing
//! for a node with no Linear-shaped ticket or when no API key is stored. A
//! failure is logged; the next milestone tries again.

use crate::linear_import::parse_ticket_reference;
use tod_store::credentials::{CredentialStore, resolve_linear_api_key};
use tod_store::fleet::FleetStore;
use tod_store::linear::StateGoal;
use uuid::Uuid;

/// A point in the node's life that the ticket should have caught up with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Milestone {
    /// The ticket was taken on (quick-accepted, or past design).
    Accepted,
    /// Work started: the runner, or implementation.
    Started,
    /// Under review: code review, or the pull request.
    InReview,
    /// Merged.
    Done,
}

impl Milestone {
    fn goal(self) -> StateGoal {
        match self {
            Self::Accepted => StateGoal { kind: "unstarted", name_hints: &["up next", "todo", "ready"] },
            Self::Started => StateGoal { kind: "started", name_hints: &[] },
            Self::InReview => StateGoal { kind: "started", name_hints: &["review"] },
            Self::Done => StateGoal { kind: "completed", name_hints: &[] },
        }
    }

    /// What a node entering lifecycle `state` has reached, if anything the
    /// ticket tracks.
    pub fn for_lifecycle(state: &str) -> Option<Self> {
        match state {
            "design" | "planning" | "ready" => Some(Self::Accepted),
            "active" | "verifying" => Some(Self::Started),
            "review" | "pr" | "approved" => Some(Self::InReview),
            "merged" | "released" | "learn" | "done" => Some(Self::Done),
            _ => None,
        }
    }
}

/// Pushes `node`'s ticket forward to `milestone`, off the calling thread.
/// Cheap and silent when there is nothing to do.
pub fn push(fleet: &FleetStore, node: Uuid, milestone: Milestone) {
    let Ok(Some(task)) = fleet.get_node(&node.to_string()) else {
        return;
    };
    let Some(ticket) = task.ticket.as_deref().and_then(parse_ticket_reference) else {
        return;
    };
    push_ticket(fleet, ticket, milestone);
}

/// [`push`] for a ticket already known, e.g. the source of an accepted copy.
pub fn push_ticket(fleet: &FleetStore, ticket: String, milestone: Milestone) {
    let root = fleet.paths().root().to_path_buf();
    let spawned = std::thread::Builder::new().name("tod-linear-sync".into()).spawn(move || {
        let Some(key) = resolve_linear_api_key(&CredentialStore::from_data_root(&root)) else {
            return;
        };
        match tod_store::linear::ensure_state_at_least(&key, &ticket, &milestone.goal()) {
            Ok(Some(state)) => tracing::info!(%ticket, %state, "moved Linear ticket"),
            Ok(None) => {}
            Err(err) => tracing::warn!(%ticket, "Linear ticket not updated: {err}"),
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("Linear sync: could not start: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_states_map_to_milestones() {
        assert_eq!(Milestone::for_lifecycle("proposed"), None);
        assert_eq!(Milestone::for_lifecycle("planning"), Some(Milestone::Accepted));
        assert_eq!(Milestone::for_lifecycle("active"), Some(Milestone::Started));
        assert_eq!(Milestone::for_lifecycle("pr"), Some(Milestone::InReview));
        assert_eq!(Milestone::for_lifecycle("merged"), Some(Milestone::Done));
        assert_eq!(Milestone::for_lifecycle("bogus"), None);
    }

    #[test]
    fn a_node_without_a_ticket_does_nothing() {
        let fx = crate::interview::test_support::fixture();
        push(&fx.fleet, fx.node, Milestone::Started);
    }
}
