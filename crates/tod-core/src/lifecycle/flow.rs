//! The built-in lifecycle's forward path and the work attached to its phases.
//! A work cycle runs inside one phase, so its agents share the same phase stay.

use tod_store::lifecycle_config;
use tod_store::settings::VALID_LIFECYCLE_STATES;

#[derive(Clone, Copy)]
pub struct Phase {
    pub id: &'static str,
    pub work: &'static str,
    /// Existing lifecycle config keys for the work in this phase.
    pub settings: &'static [&'static str],
    pub gate: &'static str,
    pub cycle: Option<WorkCycle>,
}

#[derive(Clone, Copy)]
pub struct WorkCycle {
    pub check: WorkAction,
    pub rework: WorkAction,
    pub rework_when: &'static str,
    pub retry_when: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkAction {
    Review,
    Fix,
}

impl WorkAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Review => "Review change",
            Self::Fix => "Fix findings",
        }
    }

    pub fn settings_key(self) -> &'static str {
        match self {
            Self::Review => lifecycle_config::REVIEW,
            Self::Fix => "fix",
        }
    }
}

impl WorkCycle {
    /// Reuse the check after rework. Only the check can clear the cycle.
    pub fn next(self, checked: bool, rework_due: bool, cleared: bool) -> Option<WorkAction> {
        if !checked {
            Some(self.check)
        } else if rework_due {
            Some(self.rework)
        } else {
            (!cleared).then_some(self.check)
        }
    }
}

pub const REVIEW_CYCLE: WorkCycle = WorkCycle {
    check: WorkAction::Review,
    rework: WorkAction::Fix,
    rework_when: "Open findings need answers",
    retry_when: "Answers recorded · review the change again",
};

pub const SUPPORTING_AGENTS: &[&str] = &[
    lifecycle_config::GUIDANCE,
    lifecycle_config::INCIDENT,
    lifecycle_config::TRIAGE,
    lifecycle_config::PROCESS_REVIEW,
];

pub const PHASES: [Phase; VALID_LIFECYCLE_STATES.len()] = [
    phase(
        "proposed",
        "Define the task requirements and certify the proposal.",
        &["proposed"],
        "Proposal certified",
    ),
    phase(
        "design",
        "Work out the design and certify it against the requirements.",
        &["design"],
        "Design certified",
    ),
    phase(
        "planning",
        "Write implementation steps and trace them to requirements.",
        &["planning"],
        "Plan certified",
    ),
    phase(
        "ready",
        "Prepare the agent and working files.",
        &[],
        "Implementation setup ready",
    ),
    phase(
        "active",
        "Implement due plan steps and run configured checks.",
        &["implement"],
        "Due steps implemented",
    ),
    phase(
        "verifying",
        "Verify due steps and obligations. Audit the evidence when required.",
        &["verify", "verify_evaluate"],
        "Verification clears",
    ),
    Phase {
        cycle: Some(REVIEW_CYCLE),
        ..phase(
            "review",
            "Review the change and resolve findings before shipping.",
            &["review_risk", "quick_fix"],
            "Clean review",
        )
    },
    phase(
        "pr",
        "Push the changes and open the pull requests.",
        &["pr"],
        "Pull requests open",
    ),
    phase(
        "pr_draft",
        "Tend draft checks and feedback, then wait for a person to take the review.",
        &["pr_draft", "small_review"],
        "Draft review ready",
    ),
    phase(
        "pr_review",
        "Respond to pull request feedback and check merge readiness.",
        &["pr_review"],
        "Pull requests mergeable",
    ),
    phase(
        "approved",
        "Wait for GitHub to report that the pull requests merged.",
        &[],
        "Pull requests merged",
    ),
    phase(
        "merged",
        "Certify the merge and wait for release when required.",
        &["merged"],
        "Release confirmed",
    ),
    phase(
        "released",
        "Complete and verify the work due at release.",
        &["released"],
        "Release work certified",
    ),
    phase(
        "learn",
        "Record lessons from the task.",
        &["learn"],
        "Retrospective recorded",
    ),
    phase(
        "done",
        "Close outstanding requests and finish the task.",
        &[],
        "",
    ),
];

const fn phase(
    id: &'static str,
    work: &'static str,
    settings: &'static [&'static str],
    gate: &'static str,
) -> Phase {
    Phase {
        id,
        work,
        settings,
        gate,
        cycle: None,
    }
}

pub fn find_phase(id: &str) -> Option<&'static Phase> {
    PHASES.iter().find(|phase| phase.id == id)
}

pub fn next_phase(id: &str) -> Option<&'static Phase> {
    let index = PHASES.iter().position(|phase| phase.id == id)?;
    PHASES.get(index + 1)
}

#[derive(Clone, Copy)]
pub struct ReturnPath {
    pub from: &'static str,
    pub to: &'static str,
    pub when: &'static str,
}

pub const FAILED_VERIFICATION: ReturnPath = ReturnPath {
    from: "verifying",
    to: "active",
    when: "Verification finishes with failed plan steps",
};

pub const RETURN_PATHS: &[ReturnPath] = &[
    FAILED_VERIFICATION,
    ReturnPath {
        from: "review",
        to: crate::fix_escalation::TARGET,
        when: "Repeated fixes point to a design problem",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_path_matches_stored_lifecycle() {
        assert_eq!(PHASES.map(|phase| phase.id), VALID_LIFECYCLE_STATES);
        for path in RETURN_PATHS {
            let from = PHASES
                .iter()
                .position(|phase| phase.id == path.from)
                .unwrap();
            let to = PHASES.iter().position(|phase| phase.id == path.to).unwrap();
            assert!(to < from);
        }
    }

    #[test]
    fn every_configuration_key_has_a_home_in_the_flowchart() {
        let mut keys = SUPPORTING_AGENTS.to_vec();
        for phase in PHASES {
            keys.extend_from_slice(phase.settings);
            if let Some(cycle) = phase.cycle {
                keys.extend([cycle.check.settings_key(), cycle.rework.settings_key()]);
            }
        }
        keys.sort_unstable();
        let mut expected = lifecycle_config::PHASES;
        expected.sort_unstable();
        assert_eq!(keys, expected);
    }
}
