//! The agent the supervisor drives, and the wrapper that keeps the local
//! copy in step with it.
//!
//! The agent's `tod-cli` writes go to the orchestrator, while the protocol
//! that judges a finished turn reads the local copy. So [`Syncing`] pushes
//! the copy before every turn it sends (the conversation row the agent's
//! writes are attributed to must exist there first) and pulls as soon as a
//! turn is seen to end, before the driver reads it.

use crate::replica::Replica;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tod_agent::agent_traffic::InterviewAgentCounts;
use tod_agent::{
    AgentBackend, AgentEnvironment, AgentLaunchOptions, AgentProvider, AgentRunHandle, AgentRunState, RunId,
    SessionTurn,
};

/// Which agent runs the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Claude Code through `claude-code-acp`, with the subscription token in
    /// the environment (`CLAUDE_CODE_OAUTH_TOKEN`).
    Claude,
    /// The app's `--agent mock`, writing to the local copy.
    Mock,
}

impl AgentKind {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw {
            "claude" => Ok(Self::Claude),
            "mock" => Ok(Self::Mock),
            other => anyhow::bail!("unknown agent {other:?} (expected claude or mock)"),
        }
    }

    /// The provider. The mock acts through `tod-cli`'s client against
    /// `data_root` (the local copy, reached through its mutation socket).
    pub fn provider(self, data_root: &Path) -> Box<dyn AgentProvider + Send> {
        let log = tod_agent::agent_traffic::shared_log();
        match self {
            Self::Mock => {
                tod_core::interview::mock::install_mock_interview_handler(data_root.to_path_buf());
                AgentBackend::Mock.build_provider(log)
            }
            Self::Claude => AgentBackend::Claude.build_provider(log),
        }
    }
}

/// An [`AgentProvider`] that syncs the local copy around each turn.
pub struct Syncing {
    inner: Box<dyn AgentProvider + Send>,
    replica: Arc<Mutex<Replica>>,
}

impl Syncing {
    pub fn new(inner: Box<dyn AgentProvider + Send>, replica: Arc<Mutex<Replica>>) -> Self {
        Self { inner, replica }
    }

    fn push(&self) {
        let mut replica = self.replica.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = replica.push() {
            tracing::warn!("pushing before a turn: {err:#}");
        }
    }

    fn pull(&self) {
        let mut replica = self.replica.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = replica.pull() {
            tracing::warn!("pulling after a turn: {err:#}");
        }
    }
}

impl AgentProvider for Syncing {
    fn start_fleet_agent(
        &mut self,
        owner_id: &str,
        cwd: PathBuf,
        prompt: String,
        options: AgentLaunchOptions,
        session_title: String,
        environment: AgentEnvironment,
    ) -> anyhow::Result<AgentRunHandle> {
        self.push();
        self.inner.start_fleet_agent(owner_id, cwd, prompt, options, session_title, environment)
    }

    fn send_session_turn(&mut self, turn: SessionTurn) -> anyhow::Result<AgentRunHandle> {
        self.push();
        self.inner.send_session_turn(turn)
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

    fn session_reply_parts(&self, key: &str) -> Option<Vec<tod_agent::ReplyPart>> {
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
        if matches!(state, Some(AgentRunState::Success(_) | AgentRunState::Failure(_))) {
            self.pull();
        }
        state
    }

    fn respond_to_permission(&mut self, id: RunId, option_id: &str) -> anyhow::Result<()> {
        self.inner.respond_to_permission(id, option_id)
    }

    fn cancel_run(&mut self, id: RunId) -> anyhow::Result<()> {
        self.inner.cancel_run(id)
    }

    fn interview_status_counts(&self) -> InterviewAgentCounts {
        self.inner.interview_status_counts()
    }

    fn set_session_observer(&mut self, observer: tod_agent::SessionObserver) {
        self.inner.set_session_observer(observer)
    }
}
