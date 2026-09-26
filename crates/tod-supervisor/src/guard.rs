//! Crash guards around the agent (design: "Crash guards").
//!
//! [`Guarded`] wraps the provider and watches every turn in flight. Any
//! change in what it reports — the activity line, the reply's text, a tool
//! call starting or updating — counts as activity. A turn with none for
//! [`Guards::hang_after`] is hung: its run is cancelled, its session closed
//! (the next turn resumes it by id), and the turn reported failed with a
//! message starting [`HUNG`], so the autopilot stops with `AgentFailed` and
//! the supervisor retries it.
//!
//! A turn that "succeeds" with a usage-limit message as its whole reply is
//! reported failed with that message, so the supervisor sees the limit the
//! same way whichever way Claude Code surfaced it.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tod_agent::agent_traffic::InterviewAgentCounts;
use tod_agent::{
    AgentEnvironment, AgentLaunchOptions, AgentProvider, AgentRunHandle, AgentRunState, ReplyPart, RunId,
    SessionTurn,
};

/// How a failure a hang caused begins.
pub const HUNG: &str = "agent hung:";

/// The supervisor's crash-guard settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guards {
    /// A turn with no activity this long is hung.
    pub hang_after: Duration,
    /// Consecutive failed turns before the user is asked.
    pub max_failures: u32,
}

impl Default for Guards {
    fn default() -> Self {
        Self { hang_after: Duration::from_secs(15 * 60), max_failures: 3 }
    }
}

/// Now, for the hang timer; tests step it by hand.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

struct Watch {
    /// The session the run is a turn of (`None` for a fleet run).
    key: Option<String>,
    fingerprint: u64,
    last_activity: Instant,
}

pub struct Guarded<P> {
    inner: P,
    hang_after: Duration,
    clock: Arc<dyn Clock>,
    runs: HashMap<RunId, Watch>,
}

impl<P: AgentProvider> Guarded<P> {
    pub fn new(inner: P, hang_after: Duration, clock: Arc<dyn Clock>) -> Self {
        Self { inner, hang_after, clock, runs: HashMap::new() }
    }

    pub fn inner_mut(&mut self) -> &mut P {
        &mut self.inner
    }

    fn watch(&mut self, id: RunId, key: Option<String>) {
        let now = self.clock.now();
        self.runs.insert(id, Watch { key, fingerprint: 0, last_activity: now });
    }

    fn fingerprint(&self, key: Option<&str>, activity: &Option<String>) -> u64 {
        let mut h = DefaultHasher::new();
        activity.hash(&mut h);
        if let Some(parts) = key.and_then(|k| self.inner.session_reply_parts(k)) {
            for part in parts {
                match part {
                    ReplyPart::Text { text } | ReplyPart::Thought { text } => text.len().hash(&mut h),
                    ReplyPart::Tool { id, title, status } => (id, title, status).hash(&mut h),
                    #[allow(unreachable_patterns)]
                    _ => format!("{part:?}").hash(&mut h),
                }
            }
        }
        h.finish()
    }

    fn reply_text(&self, key: Option<&str>) -> String {
        key.and_then(|k| self.inner.session_reply_parts(k))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| match p {
                ReplyPart::Text { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

impl<P: AgentProvider> AgentProvider for Guarded<P> {
    fn start_fleet_agent(
        &mut self,
        owner_id: &str,
        cwd: PathBuf,
        prompt: String,
        options: AgentLaunchOptions,
        session_title: String,
        environment: AgentEnvironment,
    ) -> anyhow::Result<AgentRunHandle> {
        let handle = self.inner.start_fleet_agent(owner_id, cwd, prompt, options, session_title, environment)?;
        self.watch(handle.id, None);
        Ok(handle)
    }

    fn send_session_turn(&mut self, turn: SessionTurn) -> anyhow::Result<AgentRunHandle> {
        let key = turn.key.clone();
        let handle = self.inner.send_session_turn(turn)?;
        self.watch(handle.id, Some(key));
        Ok(handle)
    }

    fn session_id(&self, key: &str) -> Option<String> {
        self.inner.session_id(key)
    }

    fn fleet_run_session_id(&self, id: RunId) -> Option<String> {
        self.inner.fleet_run_session_id(id)
    }

    fn session_context_chars(&self, key: &str) -> Option<u64> {
        self.inner.session_context_chars(key)
    }

    fn session_reply_parts(&self, key: &str) -> Option<Vec<ReplyPart>> {
        self.inner.session_reply_parts(key)
    }

    fn session_token_usage(&self, key: &str) -> Option<tod_agent::TokenUsage> {
        self.inner.session_token_usage(key)
    }

    fn close_session(&mut self, key: &str) {
        self.inner.close_session(key)
    }

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState> {
        let state = self.inner.poll_run(id);
        let Some(watch) = self.runs.get(&id) else {
            return state;
        };
        let key = watch.key.clone();
        match &state {
            Some(AgentRunState::InFlight(activity)) => {
                let fingerprint = self.fingerprint(key.as_deref(), activity);
                let now = self.clock.now();
                let watch = self.runs.get_mut(&id).expect("watched");
                if fingerprint != watch.fingerprint {
                    watch.fingerprint = fingerprint;
                    watch.last_activity = now;
                    return state;
                }
                let idle = now.saturating_duration_since(watch.last_activity);
                if idle < self.hang_after {
                    return state;
                }
                tracing::warn!(?idle, session = ?key, "agent hung; ending its session");
                self.runs.remove(&id);
                if let Err(err) = self.inner.cancel_run(id) {
                    tracing::warn!("cancelling a hung run: {err:#}");
                }
                if let Some(key) = &key {
                    self.inner.close_session(key);
                }
                Some(AgentRunState::Failure(format!("{HUNG} no activity for {} minutes", idle.as_secs() / 60)))
            }
            Some(AgentRunState::Success(reply)) => {
                self.runs.remove(&id);
                let text = reply.clone().unwrap_or_else(|| self.reply_text(key.as_deref()));
                // Only a reply that is nothing but the limit: work that
                // mentions one is still work.
                if text.len() < 400 && crate::usage_limit::detect(&text, 0).is_some() {
                    return Some(AgentRunState::Failure(text.trim().to_string()));
                }
                state
            }
            Some(AgentRunState::Failure(_)) | None => {
                self.runs.remove(&id);
                state
            }
            Some(AgentRunState::NeedsPermission(_)) => {
                // Waiting on a person is not a hang.
                let now = self.clock.now();
                if let Some(watch) = self.runs.get_mut(&id) {
                    watch.last_activity = now;
                }
                state
            }
        }
    }

    fn respond_to_permission(&mut self, id: RunId, option_id: &str) -> anyhow::Result<()> {
        self.inner.respond_to_permission(id, option_id)
    }

    fn cancel_run(&mut self, id: RunId) -> anyhow::Result<()> {
        self.runs.remove(&id);
        self.inner.cancel_run(id)
    }

    fn interview_status_counts(&self) -> InterviewAgentCounts {
        self.inner.interview_status_counts()
    }

    fn set_session_observer(&mut self, observer: tod_agent::SessionObserver) {
        self.inner.set_session_observer(observer)
    }
}

/// What to do about a run that stopped with the agent failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnFailure {
    /// A usage limit: wait for its reset.
    UsageLimit(crate::usage_limit::UsageLimit),
    /// Run the step again.
    Retry,
    /// Too many in a row: ask the user.
    Ask,
}

/// Decides [`OnFailure`] for the `failures`-th consecutive failure.
pub fn on_failure(error: &str, failures: u32, guards: &Guards, now_ms: i64) -> OnFailure {
    if let Some(limit) = crate::usage_limit::detect(error, now_ms) {
        return OnFailure::UsageLimit(limit);
    }
    if failures >= guards.max_failures { OnFailure::Ask } else { OnFailure::Retry }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct ManualClock(Mutex<Instant>);

    impl ManualClock {
        fn advance(&self, by: Duration) {
            *self.0.lock().unwrap() += by;
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            *self.0.lock().unwrap()
        }
    }

    /// A provider whose turns report `states` in order, then the last forever.
    #[derive(Default)]
    struct Scripted {
        states: Vec<AgentRunState>,
        polls: usize,
        cancelled: Vec<RunId>,
        closed: Vec<String>,
    }

    impl AgentProvider for Scripted {
        fn start_fleet_agent(
            &mut self,
            _: &str,
            _: PathBuf,
            _: String,
            _: AgentLaunchOptions,
            _: String,
            _: AgentEnvironment,
        ) -> anyhow::Result<AgentRunHandle> {
            anyhow::bail!("not used")
        }
        fn send_session_turn(&mut self, _: SessionTurn) -> anyhow::Result<AgentRunHandle> {
            Ok(AgentRunHandle { id: RunId::default() })
        }
        fn session_id(&self, _: &str) -> Option<String> {
            None
        }
        fn fleet_run_session_id(&self, _: RunId) -> Option<String> {
            None
        }
        fn session_context_chars(&self, _: &str) -> Option<u64> {
            None
        }
        fn close_session(&mut self, key: &str) {
            self.closed.push(key.to_string());
        }
        fn poll_run(&mut self, _: RunId) -> Option<AgentRunState> {
            let i = self.polls.min(self.states.len() - 1);
            self.polls += 1;
            Some(self.states[i].clone())
        }
        fn respond_to_permission(&mut self, _: RunId, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn cancel_run(&mut self, id: RunId) -> anyhow::Result<()> {
            self.cancelled.push(id);
            Ok(())
        }
        fn interview_status_counts(&self) -> InterviewAgentCounts {
            InterviewAgentCounts::default()
        }
    }

    fn turn() -> SessionTurn {
        SessionTurn {
            key: "conversation-x".into(),
            owner_id: String::new(),
            title: String::new(),
            cwd: PathBuf::new(),
            options: AgentLaunchOptions::for_platform(tod_agent::AgentPlatform::Claude),
            resume_session_id: None,
            opening: None,
            message: "go".into(),
            purpose: Default::default(),
            env: Vec::new(),
            environment: AgentEnvironment::Host,
        }
    }

    fn guarded(states: Vec<AgentRunState>) -> (Guarded<Scripted>, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock(Mutex::new(Instant::now())));
        let provider = Scripted { states, ..Default::default() };
        (Guarded::new(provider, Duration::from_secs(15 * 60), clock.clone()), clock)
    }

    #[test]
    fn a_turn_with_no_activity_is_ended_as_hung() {
        let (mut agent, clock) = guarded(vec![AgentRunState::InFlight(Some("thinking".into()))]);
        let run = agent.send_session_turn(turn()).unwrap().id;
        assert!(matches!(agent.poll_run(run), Some(AgentRunState::InFlight(_))));
        clock.advance(Duration::from_secs(14 * 60));
        assert!(matches!(agent.poll_run(run), Some(AgentRunState::InFlight(_))));
        clock.advance(Duration::from_secs(2 * 60));
        match agent.poll_run(run) {
            Some(AgentRunState::Failure(error)) => assert!(error.starts_with(HUNG), "{error}"),
            other => panic!("expected a hang, got {other:?}"),
        }
        assert_eq!(agent.inner.cancelled, vec![run]);
        assert_eq!(agent.inner.closed, vec!["conversation-x".to_string()]);
    }

    #[test]
    fn activity_resets_the_timer() {
        let states = (0..10).map(|i| AgentRunState::InFlight(Some(format!("tool {i}")))).collect();
        let (mut agent, clock) = guarded(states);
        let run = agent.send_session_turn(turn()).unwrap().id;
        for _ in 0..10 {
            clock.advance(Duration::from_secs(10 * 60));
            assert!(matches!(agent.poll_run(run), Some(AgentRunState::InFlight(_))));
        }
    }

    #[test]
    fn a_permission_prompt_is_not_a_hang() {
        let request = tod_agent::PermissionRequest { run: RunId::default(), title: "t".into(), options: Vec::new() };
        let (mut agent, clock) = guarded(vec![AgentRunState::NeedsPermission(request)]);
        let run = agent.send_session_turn(turn()).unwrap().id;
        for _ in 0..5 {
            clock.advance(Duration::from_secs(20 * 60));
            assert!(matches!(agent.poll_run(run), Some(AgentRunState::NeedsPermission(_))));
        }
    }

    #[test]
    fn a_reply_that_is_only_a_usage_limit_fails_the_turn() {
        let (mut agent, _) =
            guarded(vec![AgentRunState::Success(Some("5-hour limit reached ∙ resets 2am".into()))]);
        let run = agent.send_session_turn(turn()).unwrap().id;
        assert_eq!(
            agent.poll_run(run),
            Some(AgentRunState::Failure("5-hour limit reached ∙ resets 2am".into()))
        );
    }

    #[test]
    fn failures_retry_until_the_limit_then_ask() {
        let guards = Guards::default();
        assert_eq!(on_failure("agent hung: x", 1, &guards, 0), OnFailure::Retry);
        assert_eq!(on_failure("boom", 2, &guards, 0), OnFailure::Retry);
        assert_eq!(on_failure("boom", 3, &guards, 0), OnFailure::Ask);
        assert!(matches!(
            on_failure("Claude usage limit reached|1800000000", 3, &guards, 0),
            OnFailure::UsageLimit(_)
        ));
    }
}
