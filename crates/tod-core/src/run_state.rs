//! What a node's runner says it is doing, as the one synced `run_state` row
//! (`tod_store::run_state`; `doc/lifecycle/next-actor.md`, "The run state").
//!
//! [`for_outcome`] is the single, exhaustive map from how a run ended
//! ([`Outcome`], and inside it every [`NeedsHuman`]) to that row. There is no
//! wildcard arm anywhere in it: a new outcome or reason is a compile error
//! here until it has been given a kind, a phase and words, so it cannot reach
//! the user as "not started". The local runner and the cloud supervisor both
//! write through it.
//!
//! [`you_reason`] is the read side: the part of the row `next_actor` shows
//! as the user's.

use crate::autopilot::local::{FAILED_PREFIX, PAUSED, STOPPED, stopped_by_user};
use crate::autopilot::{BudgetLimit, NeedsHuman, Outcome};
use crate::next_actor::YouReason;
use anyhow::{Result, anyhow};
use tod_store::fleet::FleetStore;
use tod_store::run_state::{Draft, Phase, RESUMABLE_BY_NOBODY, RESUMABLE_BY_RUNNER, RESUMABLE_BY_YOU, RunState};
use uuid::Uuid;

/// The stable word for why, in `reason_kind`.
pub mod kind {
    /// The app is sending the node to the cloud; its supervisor has not yet reported in.
    pub const SENDING: &str = "sending";
    /// A supervisor or runner is at work on the node.
    pub const WORKING: &str = "working";
    /// The node reached `done`.
    pub const DONE: &str = "done";
    /// Waiting on something outside (a review, CI, a time); the run wakes by itself.
    pub const WAITING_OUTSIDE: &str = "waiting_outside";
    /// Waiting to try again at a time (the agent failed, a usage limit).
    pub const WAITING_RETRY: &str = "waiting_retry";
    /// Stopped for a request the user answers where requests are answered.
    pub const REQUEST: &str = "request";
    /// The user paused it.
    pub const PAUSED: &str = "paused";
    /// The user stopped it mid-turn.
    pub const STOPPED_NOW: &str = "stopped_now";
    /// The app or supervisor itself failed, or an agent turn did.
    pub const FAILED: &str = "failed";
    /// The run spent its budget.
    pub const BUDGET: &str = "budget";
    /// The run was stopped for a reason of its own wording.
    pub const STOPPED: &str = "stopped";
    /// The user answered a stop question "leave it".
    pub const LEFT_STOPPED: &str = "left_stopped";
    /// A permission nobody has granted.
    pub const PERMISSION: &str = "permission";
    /// The node has no plan to build.
    pub const NO_PLAN: &str = "no_plan";
    /// The independent evaluator rejected the same work twice.
    pub const EVALUATION_STUCK: &str = "evaluation_stuck";
    /// The pull request's agent handed back.
    pub const PR_BLOCKED: &str = "pr_blocked";
    /// The pull request keeps needing work.
    pub const PR_STUCK: &str = "pr_stuck";
    /// A step changed nothing.
    pub const NO_PROGRESS: &str = "no_progress";
    /// The same problem came back too often.
    pub const LOOPING: &str = "looping";
}

/// How a run's words differ by who ran it.
#[derive(Clone, Copy)]
pub struct Words<'a> {
    /// What a spent budget says (`budget_text`, or the supervisor's, which
    /// knows the budget's limits).
    pub budget: &'a dyn Fn(&BudgetLimit) -> String,
    /// Who to name when a run was stopped for a reason of its own wording
    /// (`"the supervisor"`); `None` leaves the reason as it is.
    pub stopped_by: Option<&'a str>,
}

impl Words<'static> {
    /// The local runner's words.
    pub fn local() -> Self {
        Self { budget: &budget_text, stopped_by: None }
    }
}

/// A spent budget, in short.
pub fn budget_text(limit: &BudgetLimit) -> String {
    match limit {
        BudgetLimit::Sessions { used, .. } => format!("budget spent: {used} sessions"),
        BudgetLimit::Time { elapsed_secs, .. } => {
            format!("budget spent: {} of work", crate::runner_status::format_elapsed(*elapsed_secs as i64 * 1000))
        }
    }
}

fn draft(phase: Phase, kind: &str, text: impl Into<String>, resumable_by: &str) -> Draft {
    Draft { phase, reason_kind: kind.into(), reason_text: text.into(), resumable_by: resumable_by.into() }
}

/// The app has begun sending the node to the cloud (written before anything
/// is seeded, so the node never reads as "not started" while it boots).
pub fn sending() -> Draft {
    draft(
        Phase::Starting,
        kind::SENDING,
        "sending the node to the cloud; its supervisor reports in when it has started",
        RESUMABLE_BY_RUNNER,
    )
}

/// A runner is at work.
pub fn working(what: &str) -> Draft {
    draft(Phase::Working, kind::WORKING, what, RESUMABLE_BY_NOBODY)
}

/// The run is waiting on `what` outside, and wakes by itself.
pub fn waiting_outside(what: &str) -> Draft {
    draft(Phase::Waiting, kind::WAITING_OUTSIDE, what, RESUMABLE_BY_RUNNER)
}

/// The user answered a stop question "leave it".
pub fn left_stopped(reason: &str) -> Draft {
    draft(Phase::Stopped, kind::LEFT_STOPPED, reason, RESUMABLE_BY_YOU)
}

/// The runner itself failed (not the node's work).
pub fn runner_failed(error: &str) -> Draft {
    draft(Phase::Stopped, kind::FAILED, error, RESUMABLE_BY_YOU)
}

/// A `Stopped` outcome whose reason says the run waits for a time.
fn is_scheduled_wait(reason: &str) -> bool {
    reason.starts_with("waiting") || reason.starts_with("usage limit: waiting")
}

/// What a run that ended with `outcome` leaves in the row. Exhaustive over
/// [`Outcome`] and [`NeedsHuman`]; see the module comment.
pub fn for_outcome(outcome: &Outcome, words: Words<'_>) -> Draft {
    match outcome {
        Outcome::Done => draft(Phase::Done, kind::DONE, "the node reached done", RESUMABLE_BY_NOBODY),
        Outcome::Waiting { what, .. } => waiting_outside(what),
        Outcome::BudgetExhausted { limit } => {
            draft(Phase::Stopped, kind::BUDGET, (words.budget)(limit), RESUMABLE_BY_YOU)
        }
        Outcome::Stopped { reason } => {
            if stopped_by_user(outcome) {
                return if reason == PAUSED {
                    draft(Phase::Stopped, kind::PAUSED, "you paused it; resume it to carry on", RESUMABLE_BY_YOU)
                } else {
                    debug_assert_eq!(reason, STOPPED);
                    draft(Phase::Stopped, kind::STOPPED_NOW, "you stopped it; run it again to carry on", RESUMABLE_BY_YOU)
                };
            }
            if is_scheduled_wait(reason) {
                return draft(Phase::Waiting, kind::WAITING_RETRY, reason.clone(), RESUMABLE_BY_RUNNER);
            }
            if let Some(error) = reason.strip_prefix(FAILED_PREFIX) {
                return draft(Phase::Stopped, kind::FAILED, error, RESUMABLE_BY_YOU);
            }
            let text = match words.stopped_by {
                Some(who) => format!("{who} stopped: {reason}"),
                None => reason.clone(),
            };
            draft(Phase::Stopped, kind::STOPPED, text, RESUMABLE_BY_YOU)
        }
        Outcome::NeedsHuman { reason } => {
            let text = reason.describe();
            match reason {
                // Requests: the user answers them where requests are answered,
                // and the run goes on by itself once none is left.
                NeedsHuman::Decision { .. }
                | NeedsHuman::BlockedSteps { .. }
                | NeedsHuman::FailingCriteria { .. }
                | NeedsHuman::GateNotPassed { .. } => draft(Phase::Waiting, kind::REQUEST, text, RESUMABLE_BY_YOU),
                NeedsHuman::NoPlan => draft(Phase::Stopped, kind::NO_PLAN, text, RESUMABLE_BY_YOU),
                NeedsHuman::EvaluationStuck { .. } => draft(Phase::Stopped, kind::EVALUATION_STUCK, text, RESUMABLE_BY_YOU),
                NeedsHuman::PrBlocked { .. } => draft(Phase::Stopped, kind::PR_BLOCKED, text, RESUMABLE_BY_YOU),
                NeedsHuman::PrStuck { .. } => draft(Phase::Stopped, kind::PR_STUCK, text, RESUMABLE_BY_YOU),
                NeedsHuman::Permission { .. } => draft(Phase::Stopped, kind::PERMISSION, text, RESUMABLE_BY_YOU),
                // The row carries the error itself, as `outcome_reason` shows it.
                NeedsHuman::AgentFailed { error } => draft(Phase::Stopped, kind::FAILED, error.clone(), RESUMABLE_BY_YOU),
                NeedsHuman::NoProgress { .. } => draft(Phase::Stopped, kind::NO_PROGRESS, text, RESUMABLE_BY_YOU),
                NeedsHuman::Looping { .. } => draft(Phase::Stopped, kind::LOOPING, text, RESUMABLE_BY_YOU),
            }
        }
    }
}

/// The user's side of a row: what `next_actor` shows when the row says the
/// node is stopped. `None` for every other phase (a run that is starting or
/// working is proved by a lease or heartbeat, a wait is the wait row's, a
/// request is `attention`'s, `done` is the lifecycle's).
pub fn you_reason(state: &RunState) -> Option<YouReason> {
    if state.phase != Phase::Stopped {
        return None;
    }
    Some(match state.reason_kind.as_str() {
        kind::PAUSED | kind::STOPPED_NOW => YouReason::Paused,
        kind::FAILED => YouReason::Failed { error: state.reason_text.clone() },
        _ => YouReason::Stopped { reason: state.reason_text.clone() },
    })
}

/// The actor that records a run state from the app or the local runner.
pub const ACTOR: &str = "run-state";

/// Records `draft` as the node's run state, written by `writer` (`app`,
/// `local`). Returns whether the row changed.
pub fn record(fleet: &FleetStore, node: Uuid, draft: Draft, writer: &str) -> Result<bool> {
    let result = fleet
        .interview(
            ACTOR,
            tod_store::interview::InterviewCommand::SetRunState {
                node_id: node,
                draft,
                writer: writer.to_string(),
                at: tod_store::outline::uuid_blob::now_ms(),
            },
        )
        .map_err(|e| anyhow!("record the node's run state: {e}"))?;
    Ok(result["changed"].as_bool().unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(outcome: Outcome) -> Draft {
        for_outcome(&outcome, Words { budget: &budget_text, stopped_by: Some("the supervisor") })
    }

    fn needs(reason: NeedsHuman) -> Draft {
        d(Outcome::NeedsHuman { reason })
    }

    fn shown(draft: &Draft) -> Option<YouReason> {
        you_reason(&RunState {
            node_id: Uuid::nil(),
            phase: draft.phase,
            reason_kind: draft.reason_kind.clone(),
            reason_text: draft.reason_text.clone(),
            since: 1,
            resumable_by: draft.resumable_by.clone(),
            updated_at: 1,
            writer: "t".into(),
        })
    }

    #[test]
    fn every_outcome_maps_to_a_phase_a_kind_and_words_the_user_is_shown() {
        let cases: Vec<(Outcome, Phase, &str, &str, Option<YouReason>)> = vec![
            (Outcome::Done, Phase::Done, kind::DONE, "the node reached done", None),
            (
                Outcome::Waiting { what: "a review of the pull request".into(), due_at_ms: 5 },
                Phase::Waiting,
                kind::WAITING_OUTSIDE,
                "a review of the pull request",
                None,
            ),
            (
                Outcome::BudgetExhausted { limit: BudgetLimit::Sessions { used: 60, elapsed_secs: 0 } },
                Phase::Stopped,
                kind::BUDGET,
                "budget spent: 60 sessions",
                Some(YouReason::Stopped { reason: "budget spent: 60 sessions".into() }),
            ),
            (
                Outcome::Stopped { reason: PAUSED.into() },
                Phase::Stopped,
                kind::PAUSED,
                "you paused it; resume it to carry on",
                Some(YouReason::Paused),
            ),
            (
                Outcome::Stopped { reason: STOPPED.into() },
                Phase::Stopped,
                kind::STOPPED_NOW,
                "you stopped it; run it again to carry on",
                Some(YouReason::Paused),
            ),
            (
                Outcome::Stopped { reason: "waiting: the agent failed (2 in a row), trying again at 9: boom".into() },
                Phase::Waiting,
                kind::WAITING_RETRY,
                "waiting: the agent failed (2 in a row), trying again at 9: boom",
                None,
            ),
            (
                Outcome::Stopped { reason: format!("{FAILED_PREFIX}the store is locked") },
                Phase::Stopped,
                kind::FAILED,
                "the store is locked",
                Some(YouReason::Failed { error: "the store is locked".into() }),
            ),
            (
                Outcome::Stopped { reason: "a new session was requested".into() },
                Phase::Stopped,
                kind::STOPPED,
                "the supervisor stopped: a new session was requested",
                Some(YouReason::Stopped { reason: "the supervisor stopped: a new session was requested".into() }),
            ),
        ];
        for (outcome, phase, kind, text, you) in cases {
            let got = d(outcome.clone());
            assert_eq!((got.phase, got.reason_kind.as_str(), got.reason_text.as_str()), (phase, kind, text), "{outcome:?}");
            assert_eq!(shown(&got), you, "{outcome:?}");
        }
    }

    #[test]
    fn every_needs_human_reason_maps_and_says_what_the_user_can_do() {
        let requests = [
            NeedsHuman::Decision { pending: 2 },
            NeedsHuman::BlockedSteps { count: 1 },
            NeedsHuman::FailingCriteria { criteria: vec!["tests".into()] },
            NeedsHuman::GateNotPassed { result: "fail".into(), summary: "s".into() },
        ];
        for reason in requests {
            let got = needs(reason.clone());
            assert_eq!((got.phase, got.reason_kind.as_str(), got.resumable_by.as_str()), (Phase::Waiting, kind::REQUEST, RESUMABLE_BY_YOU));
            assert_eq!(got.reason_text, reason.describe());
            assert_eq!(shown(&got), None, "a request is attention's, not the run state's");
        }
        let stops: Vec<(NeedsHuman, &str)> = vec![
            (NeedsHuman::NoPlan, kind::NO_PLAN),
            (NeedsHuman::EvaluationStuck { fixes: vec!["x".into()] }, kind::EVALUATION_STUCK),
            (NeedsHuman::PrBlocked { why: "w".into() }, kind::PR_BLOCKED),
            (NeedsHuman::PrStuck { why: "w".into() }, kind::PR_STUCK),
            (NeedsHuman::Permission { title: "edit a.rs".into() }, kind::PERMISSION),
            (NeedsHuman::NoProgress { step: "verify".into() }, kind::NO_PROGRESS),
            (NeedsHuman::Looping { what: "a test".into(), times: 3 }, kind::LOOPING),
        ];
        for (reason, expected) in stops {
            let got = needs(reason.clone());
            assert_eq!((got.phase, got.reason_kind.as_str()), (Phase::Stopped, expected), "{reason:?}");
            assert_eq!(got.reason_text, reason.describe(), "{reason:?}");
            assert_eq!(shown(&got), Some(YouReason::Stopped { reason: reason.describe() }), "{reason:?}");
        }
        let failed = needs(NeedsHuman::AgentFailed { error: "429".into() });
        assert_eq!((failed.phase, failed.reason_kind.as_str(), failed.reason_text.as_str()), (Phase::Stopped, kind::FAILED, "429"));
        assert_eq!(shown(&failed), Some(YouReason::Failed { error: "429".into() }));
        // The words users read, pinned for the ones that tell them what to do.
        assert_eq!(
            needs(NeedsHuman::NoPlan).reason_text,
            "the node is ready to be built but has no plan steps; ask the agent to make a plan or add the steps yourself"
        );
        assert!(needs(NeedsHuman::PrStuck { why: "w".into() }).reason_text.contains("then run it again"));
    }

    #[test]
    fn the_rows_the_app_and_runner_write_themselves_say_what_they_mean() {
        let s = sending();
        assert_eq!((s.phase, s.reason_kind.as_str(), s.resumable_by.as_str()), (Phase::Starting, kind::SENDING, RESUMABLE_BY_RUNNER));
        assert_eq!(s.reason_text, "sending the node to the cloud; its supervisor reports in when it has started");
        assert_eq!(working("planning").phase, Phase::Working);
        assert_eq!(waiting_outside("CI").resumable_by, RESUMABLE_BY_RUNNER);
        let left = left_stopped("you left it");
        assert_eq!(shown(&left), Some(YouReason::Stopped { reason: "you left it".into() }));
        assert_eq!(runner_failed("x").reason_kind, kind::FAILED);
    }

    #[test]
    fn the_local_runner_names_no_one_for_a_reason_of_its_own() {
        let got = for_outcome(&Outcome::Stopped { reason: "because".into() }, Words::local());
        assert_eq!(got.reason_text, "because");
    }
}
