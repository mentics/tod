//! Taking a context change (`doc/cloud-sandboxes/autonomous-nodes.md`,
//! "Changes that affect a running node").
//!
//! The orchestrator marks the node's `cloud_nodes.context_changed_at` when
//! the user (or another node) changes something the node depends on, and
//! pokes it. The supervisor looks at every stopping point, after its pull
//! ([`changed_since`]). When the mark is newer than the one it last took, the
//! autopilot is stopped there (never mid-turn), and [`take`]:
//!
//! - moves the node back when `tod_core::lifecycle_validity` says its state
//!   no longer holds (the supervisor acts for the user here), saying why in
//!   the conversation's transcript and the log;
//! - ends the current conversation's agent session, so its next turn starts
//!   a fresh one from a new snapshot of the context (as a rotation does);
//!
//! and the autopilot continues. The mark the supervisor took is kept in its
//! own state ([`SEEN_FILE`]) rather than cleared in the synced row, whose
//! whole row would otherwise go back up and could undo a newer mark.

use anyhow::Result;
use std::path::Path;
use tod_core::conversation::driver::{AgentAccess, ConversationDriver};
use tod_store::conversation::TurnRole;
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use uuid::Uuid;

/// The reason the autopilot is stopped with to take a context change.
pub const CONTEXT_CHANGED: &str = "context changed";

/// The last `context_changed_at` taken, beside the local copy.
pub const SEEN_FILE: &str = "context-seen";

pub fn load_seen(state_dir: &Path) -> i64 {
    std::fs::read_to_string(state_dir.join(SEEN_FILE)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

pub fn save_seen(state_dir: &Path, at: i64) -> Result<()> {
    std::fs::write(state_dir.join(SEEN_FILE), at.to_string())?;
    Ok(())
}

/// The node's context mark, when it is newer than `seen`.
pub fn changed_since(store: &FleetStore, node: Uuid, seen: i64) -> Result<Option<i64>> {
    let row = store.read(|conn| tod_store::cloud_nodes::get(conn, node))?;
    Ok(row.and_then(|r| r.context_changed_at).filter(|at| *at > seen))
}

/// What [`take`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    /// The state the node was moved back to, if it was.
    pub moved_back_to: Option<String>,
    /// The conversation whose session was ended.
    pub rotated: Option<Uuid>,
}

/// Takes a context change: see the module docs. `current` is the
/// autopilot's current conversation, if one is to be continued.
pub fn take<A: AgentAccess + ?Sized>(
    store: &FleetStore,
    agent: &mut A,
    node: Uuid,
    current: Option<Uuid>,
) -> Result<Taken> {
    let mut note = String::from("The context changed: the next turn starts a fresh agent session with it.");
    let regression = store.read(|conn| tod_core::lifecycle_validity::regression(conn, node))?;
    let mut moved_back_to = None;
    if let Some(regression) = regression {
        let from = tod_core::lifecycle::current_state(store, node)?;
        tod_core::lifecycle::set_lifecycle(store, node, regression.target)?;
        tracing::warn!(
            %node, from, to = regression.target, reasons = ?regression.reasons,
            "context changed and the node's state no longer holds: moved it back"
        );
        note = format!(
            "The context changed and `{from}` no longer holds, so the supervisor moved the node back to `{}`: {}",
            regression.target,
            regression.reasons.join("; ")
        );
        moved_back_to = Some(regression.target.to_string());
    } else {
        tracing::info!(%node, "context changed: rebuilding the agent's context");
    }
    if let Some(id) = current {
        agent.with(|a| a.close_session(&ConversationDriver::session_key(id)));
        store.interview(
            ACTOR_USER,
            InterviewCommand::SetConversationSession { conversation_id: id, agent_session_id: None, session_name: None },
        )?;
        store.interview(
            ACTOR_USER,
            InterviewCommand::AppendConversationTurn {
                conversation_id: id,
                role: TurnRole::Rotation,
                body: note,
                parts: Vec::new(),
                sent_context: None,
            },
        )?;
    }
    Ok(Taken { moved_back_to, rotated: current })
}
