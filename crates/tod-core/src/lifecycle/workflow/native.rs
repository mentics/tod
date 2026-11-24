//! Reusable actions backed by TOD's native task protocols. The action reports
//! evidence and outcomes; the published graph selects the next visit.

use super::{Revision, Value, ValueType, runner::ActionOutput};
use crate::autopilot::Outcome;
use std::collections::BTreeMap;

pub const PHASE: &str = "native_phase";
pub const IMPLEMENT: &str = "native_implement";
pub const VERIFY: &str = "native_verify";
pub const REVIEW_INPUT: &str = "native_review_input";
pub const REVIEW: &str = "native_review_round";
pub const FIX: &str = "native_fix_findings";
pub const OPEN_PR: &str = "native_open_pr";
pub const TEND_PR: &str = "native_tend_pr";

pub const ACTIONS: &[(&str, &str)] = &[
    (PHASE, "Work and certify task phase"),
    (IMPLEMENT, "Implement due plan steps"),
    (VERIFY, "Verify due work"),
    (REVIEW_INPUT, "Run review checks and capture the checkout revision"),
    (REVIEW, "Run review round"),
    (FIX, "Fix review findings"),
    (OPEN_PR, "Open or certify draft pull request"),
    (TEND_PR, "Tend pull request"),
];

pub fn is_native(id: &str) -> bool {
    ACTIONS.iter().any(|(action, _)| id == *action)
}

pub(super) fn phase_supported(id: &str, phase: &str) -> bool {
    match id {
        PHASE => crate::phase::has_phase_agent(phase),
        IMPLEMENT => phase == "active",
        VERIFY => phase == "verifying",
        REVIEW_INPUT | REVIEW | FIX => phase == "review",
        OPEN_PR => phase == "pr",
        TEND_PR => matches!(phase, "pr_draft" | "pr_review"),
        _ => false,
    }
}

pub(super) fn revision(id: &str) -> Revision {
    Revision {
        id: id.into(),
        number: 1,
    }
}

pub fn required_outputs(id: &str) -> &'static [(&'static str, ValueType)] {
    match id {
        VERIFY => &[("verified", ValueType::Boolean)],
        REVIEW => &[
            ("open_findings", ValueType::Integer),
            ("review_clear", ValueType::Boolean),
        ],
        REVIEW_INPUT | super::builtin::WORKSPACE_CAPTURE => &[("revision", ValueType::Text)],
        _ => &[],
    }
}

/// These values are calculated from actual task records. A native protocol
/// does not ask the provider to return a second JSON verdict.
#[derive(Debug)]
pub(crate) enum StepResult {
    Complete(ActionOutput),
    Suspended(Outcome),
}

impl StepResult {
    pub(crate) fn complete(evidence: Vec<String>, values: BTreeMap<String, Value>) -> Self {
        Self::Complete(ActionOutput {
            values,
            evidence,
            ..Default::default()
        })
    }

    pub(crate) fn stopped(reason: impl Into<String>) -> Self {
        Self::Suspended(Outcome::Stopped {
            reason: reason.into(),
        })
    }
}
