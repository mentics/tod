//! Failures that happen off the turn that caused them (a background copy of
//! an agent's session log), made visible: written into the node's latest
//! conversation as an error turn, where an unattended run's transcript shows
//! it too, and returned for the view to toast.

use tod_store::conversation::{ConversationRepo, Focus, TurnRole};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use uuid::Uuid;

/// Take what failed in the background since the last call and record it.
/// Whoever polls calls this; each failure is returned to exactly one caller.
pub fn record_pending(fleet: &FleetStore) -> Vec<String> {
    tod_store::fleet::session_log::take_problems()
        .into_iter()
        .map(|(node, message)| {
            match Uuid::parse_str(&node) {
                Ok(node) => record(fleet, node, &message),
                Err(err) => tracing::warn!("a problem for node {node:?} has no node id: {err}"),
            }
            message
        })
        .collect()
}

fn record(fleet: &FleetStore, node: Uuid, message: &str) {
    let latest = fleet.read(|conn| ConversationRepo::new(conn).latest_for_focus(Focus::Node(node)));
    match latest {
        Ok(Some(conversation)) => {
            let appended = fleet.interview(
                ACTOR_USER,
                InterviewCommand::AppendConversationTurn {
                    conversation_id: conversation.id,
                    role: TurnRole::Error,
                    body: message.to_string(),
                    parts: Vec::new(),
                    sent_context: None,
                    attachments: Vec::new(),
                },
            );
            if let Err(err) = appended {
                tracing::warn!(%node, "could not record a problem in the conversation: {err:#}");
            }
        }
        Ok(None) => {}
        Err(err) => tracing::warn!(%node, "could not find the conversation for a problem: {err:#}"),
    }
}
