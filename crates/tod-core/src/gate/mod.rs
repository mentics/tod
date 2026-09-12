//! Lifecycle gate-check orchestration — a single agent turn that decides
//! whether a node may advance to the next lifecycle state.
//!
//! Mirrors `crate::interview`'s shape (context assembly, then a routing
//! helper) but is one-shot: no driver, no multi-turn session management. The
//! caller (an interactive UI, or eventually `tod-cli`) sends one
//! [`context::GateCheckRequest`] as a session turn via `tod_agent::AgentProvider`,
//! waits for the reply, and parses it with [`response::parse_gate_reply`].

pub mod context;
pub mod response;
pub mod routing;

pub use context::{GATE_CHECK_CONTEXT_KEY, GateCheckRequest, build_gate_check_message};
pub use response::{GateCheckReply, GateOutcome, GateResultRow, parse_gate_reply};
pub use routing::{gate_criteria_for, gate_criteria_for_with_conn};
