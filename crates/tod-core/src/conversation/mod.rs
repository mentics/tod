//! The conversation view's agent: turning the user's direction into changes
//! anywhere in the project, one conversation at a time.
//!
//! A conversation has a focus (the project, a node, an obligation, or a plan
//! step) and one agent session at a time. [`driver::ConversationDriver`]
//! sends each message and records the reply; [`context`] builds what the
//! agent is told; [`mock`] plays the agent for `--agent mock`. The
//! conversation log itself (turns, recorded actions, flags, reversal) lives in
//! `tod_store::conversation`. Spec: `doc/conversation/spec.md`.

pub mod context;
pub mod driver;
pub mod implement;
pub mod mock;
pub mod protocol;
pub mod review;
pub mod verify;

#[cfg(test)]
mod tests;

pub use driver::{
    ConversationConfig, ConversationDriver, ConversationEvent, ConversationStatus, ROTATION_NOTE,
};
pub use protocol::{CONTINUATION_CAP, Next, Protocol, ProtocolEnv, protocol_for};
