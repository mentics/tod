//! Lifecycle gate-check orchestration — a single agent turn that decides
//! whether a node may advance to the next lifecycle state.
//!
//! Mirrors `crate::interview`'s shape (context assembly, then a routing
//! helper) but is one-shot: no driver, no multi-turn session management. The
//! caller (an interactive UI, or eventually `tod-cli`) sends one
//! [`context::GateCheckRequest`] as a session turn via `tod_agent::AgentProvider`,
//! waits for the reply, and parses it with [`response::parse_gate_reply`].

pub mod context;
pub mod derived;
pub mod response;
pub mod routing;

pub use derived::{DerivedOutcome, evaluate_derived_criterion, node_action_configs};
pub use context::{
    GATE_CHECK_CONTEXT_KEY, GateCheckRequest, ON_ENTRY_CONTEXT_KEY, PlanStepWithLinks,
    build_gate_check_message, build_on_entry_message,
};
pub use response::{GateAction, GateCheckReply, GateOutcome, GateResultRow, parse_gate_reply};
pub use routing::{gate_criteria_for, gate_criteria_for_with_conn};
