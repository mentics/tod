//! What a node's runner or a conversation run by hand is doing right now, as
//! the task panel's runner line shows beside "who is next" (which is
//! `crate::next_actor`'s, never this module's).
//!
//! The runner is the node's autopilot run (`crate::autopilot::local`), when
//! there is one: [`LiveTurn::of`] reads it first, else a conversation running
//! on the node.

use crate::autopilot::Outcome;
use crate::autopilot::local::Live;
use crate::conversation::ConversationStatus;

/// The lifecycle state that means the task is finished.
pub const DONE_STATE: &str = "done";

/// A turn that is live right now: what it is doing, since when, and the
/// tokens it has spent so far (when the provider reports them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTurn {
    pub activity: Option<String>,
    pub since: Option<i64>,
    pub tokens: Option<u64>,
    /// The run is polling GitHub for a change and reads it next at this time.
    pub next_poll_ms: Option<i64>,
}

impl LiveTurn {
    /// The live turn of the node's local runner, else of a conversation
    /// running on it (`run_since`: when the caller first saw it running).
    /// `None` when nothing is live, or when the agent is blocked on a
    /// permission: that is the user's, and is the node's actor to say.
    pub fn of(
        runner: Option<Runner<'_>>,
        conversation: Option<&ConversationStatus>,
        run_since: Option<i64>,
    ) -> Option<Self> {
        let tokens = |status: &ConversationStatus| {
            status.live_usage.as_ref().map(|u| {
                let t = &u.total;
                t.input + t.output + t.cache_read + t.cache_write
            })
        };
        if let Some(Runner::Running { live, since }) = runner {
            let status = &live.status;
            if status.permission.is_some() {
                return None;
            }
            return Some(Self {
                activity: status
                    .activity
                    .clone()
                    .filter(|a| !a.trim().is_empty())
                    .or_else(|| Some(runner_activity(live))),
                since: Some(since),
                tokens: tokens(status),
                next_poll_ms: live.next_poll_ms,
            });
        }
        let status = conversation.filter(|s| s.running)?;
        if status.permission.is_some() {
            return None;
        }
        Some(Self {
            activity: status.activity.clone().filter(|a| !a.trim().is_empty()),
            since: run_since,
            tokens: tokens(status),
            next_poll_ms: None,
        })
    }
}

/// What the node's pending GitHub waits say it is waiting for, and when it
/// looks next (ms): the soonest `github:` wait. The run that recorded it may
/// not be in this app (it ended, the app restarted, or it runs in the
/// cloud), so the wait itself is the record of the schedule.
pub fn github_wait(waits: &[tod_store::waits::Wait]) -> Option<(String, i64)> {
    let wait = waits
        .iter()
        .filter(|w| w.match_spec.starts_with("github:"))
        .min_by_key(|w| w.due_at)?;
    if let Some(detail) = wait.detail.as_deref().filter(|d| !d.trim().is_empty()) {
        return Some((detail.to_string(), wait.due_at));
    }
    let spec = wait.match_spec.as_str();
    let what = if spec == "github:release:published" || spec.starts_with("github:release:") {
        "a release that names the pull request".to_string()
    } else {
        let number = spec.strip_prefix("github:pr:").and_then(|r| r.split(':').next());
        let event = spec.rsplit(':').next().unwrap_or_default();
        let thing = if crate::next_actor::is_review_spec(spec) {
            "a review"
        } else {
            match event {
                "merged" => "the merge",
                _ => "GitHub",
            }
        };
        match number.filter(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty()) {
            Some(n) => format!("{thing} of pull request #{n}"),
            None => thing.to_string(),
        }
    };
    Some((what, wait.due_at))
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
        Some(P::Phase) => "working on the phase",
        Some(P::Evaluate) => "evaluating the phase",
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
    fn a_live_conversation_carries_activity_since_and_tokens() {
        let mut status = running();
        let mut usage = TokenUsage::default();
        usage.total.input = 1000;
        usage.total.output = 500;
        usage.total.cache_read = 20_000;
        status.live_usage = Some(usage);
        assert_eq!(
            LiveTurn::of(None, Some(&status), Some(10)),
            Some(LiveTurn {
                activity: Some("Reading src/lib.rs".into()),
                since: Some(10),
                tokens: Some(21_500),
                next_poll_ms: None,
            })
        );
    }

    #[test]
    fn a_permission_request_is_not_a_live_turn() {
        let mut status = running();
        status.permission = Some(tod_agent::PermissionRequest {
            run: tod_agent::RunId::new(),
            title: "Run tests".into(),
            options: Vec::new(),
        });
        assert_eq!(LiveTurn::of(None, Some(&status), Some(10)), None);
    }

    #[test]
    fn nothing_running_is_no_live_turn() {
        assert_eq!(LiveTurn::of(None, None, None), None);
        assert_eq!(LiveTurn::of(None, Some(&ConversationStatus::default()), None), None);
    }

    #[test]
    fn a_local_runner_is_read_first_and_a_stopped_one_lets_a_hand_run_show() {
        let live = Live::default();
        assert_eq!(
            LiveTurn::of(Some(Runner::Running { live: &live, since: 3 }), None, None),
            Some(LiveTurn {
                activity: Some("deciding the next step".into()),
                since: Some(3),
                tokens: None,
                next_poll_ms: None,
            })
        );
        let paused = Outcome::Stopped { reason: crate::autopilot::local::PAUSED.into() };
        assert_eq!(LiveTurn::of(Some(Runner::Ended(&paused)), None, None), None);
        assert!(LiveTurn::of(Some(Runner::Ended(&paused)), Some(&running()), Some(4)).is_some());
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

    #[test]
    fn a_scheduled_github_look_is_named_by_its_wait() {
        let wait = |spec: &str, due_at| tod_store::waits::Wait {
            id: uuid::Uuid::nil(),
            node_id: uuid::Uuid::nil(),
            kind: "event".into(),
            match_spec: spec.into(),
            every_secs: None,
            due_at,
            state: "pending".into(),
            created_at: 0,
            updated_at: 0,
            detail: None,
            attempt: 0,
            max_attempts: None,
            last_error: None,
        };
        let waits = [wait("github:pr:12:merged", 500), wait("notes", 1), wait("github:pr:13:review", 900)];
        assert_eq!(github_wait(&waits), Some(("the merge of pull request #12".to_string(), 500)));
        assert_eq!(github_wait(&[wait("notes", 1)]), None);
        // Both forms of a review's spec read as a review.
        for spec in ["github:pr:13:review", "github:pr:13:review:maria,team:core"] {
            assert_eq!(github_wait(&[wait(spec, 900)]), Some(("a review of pull request #13".to_string(), 900)));
        }
    }
}
