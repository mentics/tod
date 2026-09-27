//! What a task's runner is doing right now, as the task panel's runner line
//! shows it (`doc/ui/task-panel.md`, "Runner").
//!
//! The runner is the node's autopilot run on this machine
//! (`crate::autopilot::local`), when there is one: [`RunnerStatus::with_runner`]
//! reads it first. Otherwise the status is derived from what exists — the
//! node's lifecycle state, what it is waiting on the user for
//! (`crate::attention`), and the [`ConversationStatus`] of any conversation
//! running on it ([`RunnerStatus::derive`]).

use crate::autopilot::local::{FAILED_PREFIX, Live, stopped_by_user};
use crate::autopilot::{BudgetLimit, NeedsHuman, Outcome};
use crate::conversation::ConversationStatus;

/// The lifecycle state that means the task is finished.
pub const DONE_STATE: &str = "done";

/// What the runner is doing. Times are milliseconds since the epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerStatus {
    /// An agent is working: what it is doing, since when, and the tokens it
    /// has spent so far (when the provider reports them).
    Running {
        activity: Option<String>,
        since: Option<i64>,
        tokens: Option<u64>,
    },
    /// Nothing can continue until the user answers something.
    Waiting { since: i64 },
    /// The last agent turn failed, or the runner itself did.
    Failed { error: String },
    /// The user paused the runner.
    Paused,
    /// The runner stopped for a reason that is not a request (its budget
    /// ran out, a step changed nothing, …): resuming is the user's call.
    Stopped { reason: String },
    /// Nothing running, nothing waiting.
    Idle,
    /// The task is finished.
    Done,
}

impl RunnerStatus {
    /// Build the status from what exists today.
    ///
    /// - `lifecycle`: the node's lifecycle state.
    /// - `waiting_since`: `NodeAttention::waiting_since`.
    /// - `conversation`: the status of the conversation on the node that
    ///   matters most (a running one, else one whose last turn failed).
    /// - `run_since`: when the caller first saw that conversation running.
    ///
    /// A running agent wins; a pending permission request is a wait on the
    /// user. Then a request waiting on the user, then a failed turn, then
    /// done, else idle.
    pub fn derive(
        lifecycle: &str,
        waiting_since: Option<i64>,
        conversation: Option<&ConversationStatus>,
        run_since: Option<i64>,
    ) -> Self {
        if let Some(status) = conversation.filter(|s| s.running) {
            if status.permission.is_some()
                && let Some(since) = run_since.or(waiting_since)
            {
                return Self::Waiting { since };
            }
            return Self::Running {
                activity: status.activity.clone().filter(|a| !a.trim().is_empty()),
                since: run_since,
                tokens: status.live_usage.as_ref().map(|u| {
                    let t = &u.total;
                    t.input + t.output + t.cache_read + t.cache_write
                }),
            };
        }
        if let Some(since) = waiting_since {
            return Self::Waiting { since };
        }
        if let Some(error) = conversation.and_then(|s| s.last_error.clone()) {
            return Self::Failed { error };
        }
        if lifecycle == DONE_STATE {
            return Self::Done;
        }
        Self::Idle
    }

    /// [`Self::derive`], reading the node's local runner first when there is
    /// one. A conversation the user runs by hand while the runner is stopped
    /// (answering a request) shows over the stopped runner.
    pub fn with_runner(
        runner: Option<Runner<'_>>,
        lifecycle: &str,
        waiting_since: Option<i64>,
        conversation: Option<&ConversationStatus>,
        run_since: Option<i64>,
    ) -> Self {
        match runner {
            Some(Runner::Running { live, since }) => {
                let status = &live.status;
                if status.permission.is_some() {
                    return Self::Waiting { since };
                }
                Self::Running {
                    activity: status
                        .activity
                        .clone()
                        .filter(|a| !a.trim().is_empty())
                        .or_else(|| Some(runner_activity(live))),
                    since: Some(since),
                    tokens: status.live_usage.as_ref().map(|u| {
                        let t = &u.total;
                        t.input + t.output + t.cache_read + t.cache_write
                    }),
                }
            }
            Some(Runner::Ended(outcome)) if !conversation.is_some_and(|c| c.running) => match outcome {
                Outcome::Done => Self::Done,
                outcome if stopped_by_user(outcome) => Self::Paused,
                Outcome::NeedsHuman { reason } if reason.is_request() && waiting_since.is_some() => {
                    Self::Waiting {
                        since: waiting_since.unwrap_or_default(),
                    }
                }
                Outcome::NeedsHuman {
                    reason: NeedsHuman::AgentFailed { error },
                } => Self::Failed { error: error.clone() },
                Outcome::NeedsHuman { reason } => Self::Stopped {
                    reason: reason.describe(),
                },
                Outcome::Stopped { reason } => match reason.strip_prefix(FAILED_PREFIX) {
                    Some(error) => Self::Failed { error: error.to_string() },
                    None => Self::Stopped { reason: reason.clone() },
                },
                Outcome::BudgetExhausted { limit } => Self::Stopped {
                    reason: match limit {
                        BudgetLimit::Sessions { used } => format!("budget spent: {used} sessions"),
                        BudgetLimit::Time { elapsed_secs } => format!(
                            "budget spent: {} of work",
                            format_elapsed(*elapsed_secs as i64 * 1000)
                        ),
                    },
                },
            },
            _ => Self::derive(lifecycle, waiting_since, conversation, run_since),
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// When the elapsed time counts from, for statuses that show one.
    pub fn since(&self) -> Option<i64> {
        match self {
            Self::Running { since, .. } => *since,
            Self::Waiting { since } => Some(*since),
            _ => None,
        }
    }
}

/// The node's local runner, as [`RunnerStatus::with_runner`] reads it.
#[derive(Debug, Clone, Copy)]
pub enum Runner<'a> {
    /// Working since `since` (ms since the epoch).
    Running { live: &'a Live, since: i64 },
    /// It stopped, with this outcome.
    Ended(&'a Outcome),
}

/// What a runner is doing when its agent reports no activity of its own.
fn runner_activity(live: &Live) -> String {
    use tod_store::conversation::ProtocolKind as P;
    match live.protocol {
        None => "deciding the next step",
        Some(P::Implementation) => "implementing",
        Some(P::Verification) => "verifying",
        Some(P::Review) => "reviewing",
        Some(P::Fix) => "fixing review findings",
        Some(P::GateCheck) => "checking the gate",
        Some(P::OnEntry) => "starting the new state",
        Some(P::Pr) => "working on the pull request",
        Some(_) => "working",
    }
    .to_string()
}

/// "45s", "6m", "2h 5m", "3d 4h": a short elapsed time.
pub fn format_elapsed(ms: i64) -> String {
    let secs = (ms.max(0)) / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        let (h, m) = (secs / 3600, (secs % 3600) / 60);
        if m == 0 { format!("{h}h") } else { format!("{h}h {m}m") }
    } else {
        let (d, h) = (secs / 86_400, (secs % 86_400) / 3600);
        if h == 0 { format!("{d}d") } else { format!("{d}d {h}h") }
    }
}

/// "850 tok", "23k tok", "1.2M tok".
pub fn format_tokens(tokens: u64) -> String {
    if tokens < 1000 {
        format!("{tokens} tok")
    } else if tokens < 1_000_000 {
        format!("{}k tok", tokens / 1000)
    } else {
        format!("{:.1}M tok", tokens as f64 / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_agent::TokenUsage;

    fn running() -> ConversationStatus {
        ConversationStatus {
            running: true,
            activity: Some("Reading src/lib.rs".into()),
            ..Default::default()
        }
    }

    #[test]
    fn running_wins_and_carries_activity_since_and_tokens() {
        let mut status = running();
        let mut usage = TokenUsage::default();
        usage.total.input = 1000;
        usage.total.output = 500;
        usage.total.cache_read = 20_000;
        status.live_usage = Some(usage);
        assert_eq!(
            RunnerStatus::derive("implementing", Some(5), Some(&status), Some(10)),
            RunnerStatus::Running {
                activity: Some("Reading src/lib.rs".into()),
                since: Some(10),
                tokens: Some(21_500),
            }
        );
    }

    #[test]
    fn a_permission_request_is_a_wait() {
        let mut status = running();
        status.permission = Some(tod_agent::PermissionRequest {
            run: tod_agent::RunId::new(),
            title: "Run tests".into(),
            options: Vec::new(),
        });
        assert_eq!(
            RunnerStatus::derive("implementing", None, Some(&status), Some(10)),
            RunnerStatus::Waiting { since: 10 }
        );
    }

    #[test]
    fn waiting_then_failed_then_done_then_idle() {
        let failed = ConversationStatus {
            last_error: Some("boom".into()),
            ..Default::default()
        };
        assert_eq!(
            RunnerStatus::derive("ready", Some(7), Some(&failed), None),
            RunnerStatus::Waiting { since: 7 }
        );
        assert_eq!(
            RunnerStatus::derive("ready", None, Some(&failed), None),
            RunnerStatus::Failed { error: "boom".into() }
        );
        assert_eq!(RunnerStatus::derive("done", None, None, None), RunnerStatus::Done);
        assert_eq!(
            RunnerStatus::derive("ready", None, Some(&ConversationStatus::default()), None),
            RunnerStatus::Idle
        );
    }

    #[test]
    fn a_local_runner_is_read_first() {
        let live = Live::default();
        assert_eq!(
            RunnerStatus::with_runner(Some(Runner::Running { live: &live, since: 3 }), "ready", Some(1), None, None),
            RunnerStatus::Running {
                activity: Some("deciding the next step".into()),
                since: Some(3),
                tokens: None,
            }
        );
        let paused = Outcome::Stopped {
            reason: crate::autopilot::local::PAUSED.into(),
        };
        assert_eq!(
            RunnerStatus::with_runner(Some(Runner::Ended(&paused)), "ready", None, None, None),
            RunnerStatus::Paused
        );
        // A conversation run by hand shows over a stopped runner.
        assert!(
            RunnerStatus::with_runner(Some(Runner::Ended(&paused)), "ready", None, Some(&running()), Some(4))
                .is_running()
        );
    }

    #[test]
    fn a_stopped_runner_says_why() {
        let decision = Outcome::NeedsHuman {
            reason: NeedsHuman::Decision { pending: 1 },
        };
        assert_eq!(
            RunnerStatus::with_runner(Some(Runner::Ended(&decision)), "ready", Some(9), None, None),
            RunnerStatus::Waiting { since: 9 }
        );
        // A criterion the app checks is not among the requests: the line
        // says what stopped it.
        let failing = Outcome::NeedsHuman {
            reason: NeedsHuman::FailingCriteria {
                criteria: vec!["PR approved?".into()],
            },
        };
        assert_eq!(
            RunnerStatus::with_runner(Some(Runner::Ended(&failing)), "pr", None, None, None),
            RunnerStatus::Stopped {
                reason: "gate criteria failing: PR approved?".into()
            }
        );
        let budget = Outcome::BudgetExhausted {
            limit: BudgetLimit::Sessions { used: 30 },
        };
        assert_eq!(
            RunnerStatus::with_runner(Some(Runner::Ended(&budget)), "ready", None, None, None),
            RunnerStatus::Stopped {
                reason: "budget spent: 30 sessions".into()
            }
        );
        let failed = Outcome::Stopped {
            reason: format!("{FAILED_PREFIX}no store"),
        };
        assert_eq!(
            RunnerStatus::with_runner(Some(Runner::Ended(&failed)), "ready", None, None, None),
            RunnerStatus::Failed { error: "no store".into() }
        );
        assert_eq!(
            RunnerStatus::with_runner(Some(Runner::Ended(&Outcome::Done)), "done", None, None, None),
            RunnerStatus::Done
        );
    }

    #[test]
    fn formats() {
        assert_eq!(format_elapsed(45_000), "45s");
        assert_eq!(format_elapsed(6 * 60_000 + 5_000), "6m");
        assert_eq!(format_elapsed(2 * 3_600_000 + 5 * 60_000), "2h 5m");
        assert_eq!(format_elapsed(-5), "0s");
        assert_eq!(format_tokens(850), "850 tok");
        assert_eq!(format_tokens(23_400), "23k tok");
        assert_eq!(format_tokens(1_200_000), "1.2M tok");
    }
}
