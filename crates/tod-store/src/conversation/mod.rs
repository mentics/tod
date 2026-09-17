//! Conversation log: conversations about a focus, their transcript turns, the
//! outline actions made during them, and per-conversation unsure flags.
//!
//! Every node, obligation, and plan-step mutation a conversation's agent makes
//! (actor `conversation:<uuid>`) is recorded in the same transaction with the
//! item's state before and after, so any action can be reversed later without
//! the Ctrl+Z history. Writes are [`crate::interview::InterviewCommand`]
//! variants, run on the fleet writer. Spec: `doc/conversation/spec.md`.

mod inverse;
mod project;
mod record;
mod repo;
mod types;

#[cfg(test)]
mod tests;

pub use inverse::{inverse, reverse_actions};
pub use project::{dependents, net_changes, stale};
pub use record::{
    apply_user_edit, classify, clear_flag, flag_item, normalize, record_and_execute, snapshot,
};
pub use repo::ConversationRepo;
pub use types::*;

/// Prefix of the interview actor a conversation's agent writes as (D13).
pub const ACTOR_PREFIX: &str = "conversation:";

/// The conversation an actor string names, if it is `conversation:<uuid>`.
pub fn actor_conversation(actor: &str) -> Option<uuid::Uuid> {
    actor
        .strip_prefix(ACTOR_PREFIX)
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
}

/// The actor string for a conversation's agent writes.
pub fn actor_for(conversation_id: uuid::Uuid) -> String {
    format!("{ACTOR_PREFIX}{conversation_id}")
}
