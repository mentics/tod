//! What a task's runner is doing right now, as the task panel's runner line
//! shows it (`doc/ui/task-panel.md`, "Runner").
//!
//! Today there is no runner process: the status is derived from what exists
//! — the node's lifecycle state, what it is waiting on the user for
//! (`crate::attention`), and the [`ConversationStatus`] of any conversation
//! running on it. When the runner exists it becomes the source of
//! [`RunnerStatus`] instead, and the panel does not change.

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
    /// The last agent turn failed.
    Failed { error: String },
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
