//! Lifecycle gates. Every gate is an app check (`derived`, settled by
//! `crate::phase::settle_gate`); `context` builds a state agent's message.
//! `response` reads the replies of gate-check conversations from before gates
//! were app checks, which their transcripts still show, and names the actions
//! a criterion row offers.

pub mod context;
pub mod derived;
pub mod response;
pub mod routing;

pub use context::{GateCheckRequest, PlanStepWithLinks, build_phase_message};
pub use derived::{DerivedOutcome, evaluate_derived_criterion, is_derived_slug};
pub use response::{GateAction, GateCheckReply, GateOutcome, GateResultRow, parse_gate_reply};
pub use routing::{gate_criteria_for, gate_criteria_for_with_conn};
