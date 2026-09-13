//! Routes launches to Cursor or Claude ACP hosts based on [`AgentLaunchOptions::platform`].

use super::acp_host::AcpHost;
use super::cursor_acp::CursorAcpProvider;
use super::provider::{AgentProvider, AgentRunHandle, RunId, SessionTurn};
use crate::agent_launch::AgentLaunchOptions;
use crate::agent_traffic::{InterviewAgentCounts, SharedAgentTrafficLog};
use crate::platform::AgentPlatform;
use anyhow::Result;
use std::path::PathBuf;

/// Dual-host provider: interview/fleet launch options pick Cursor vs Claude per start.
pub struct RoutingAgentProvider {
    cursor: CursorAcpProvider,
    claude: CursorAcpProvider,
}

impl RoutingAgentProvider {
    pub fn new(traffic_log: SharedAgentTrafficLog) -> Self {
        Self {
            cursor: build_host_provider(AcpHost::Cursor, traffic_log.clone()),
            claude: build_host_provider(AcpHost::Claude, traffic_log),
        }
    }

    fn for_platform(&mut self, platform: AgentPlatform) -> &mut CursorAcpProvider {
        match platform {
            AgentPlatform::Cursor => &mut self.cursor,
            AgentPlatform::Claude => &mut self.claude,
        }
    }
}

fn build_host_provider(host: AcpHost, traffic_log: SharedAgentTrafficLog) -> CursorAcpProvider {
    CursorAcpProvider::for_host(host)
        .unwrap_or_else(|err| {
            let placeholder = match host {
                AcpHost::Cursor => PathBuf::from("agent"),
                AcpHost::Claude => PathBuf::from("claude-code-acp"),
            };
            eprintln!(
                "{} ACP provider init failed: {err}; using placeholder path {}",
                host.label(),
                placeholder.display()
            );
            CursorAcpProvider::with_agent_bin(host, placeholder)
        })
        .with_traffic_log(traffic_log)
}

fn sum_interview_counts(a: InterviewAgentCounts, b: InterviewAgentCounts) -> InterviewAgentCounts {
    InterviewAgentCounts {
        question_maker_in_flight: a.question_maker_in_flight + b.question_maker_in_flight,
        answer_active: a.answer_active + b.answer_active,
        answer_pool: a.answer_pool + b.answer_pool,
        answer_max: a.answer_max.max(b.answer_max),
    }
}

impl AgentProvider for RoutingAgentProvider {
    fn start_fleet_agent(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: String,
        options: AgentLaunchOptions,
        session_title: String,
    ) -> Result<AgentRunHandle> {
        self.for_platform(options.platform)
            .start_fleet_agent(agent_config_id, cwd, prompt, options, session_title)
    }

    fn send_session_turn(&mut self, turn: SessionTurn) -> Result<AgentRunHandle> {
        self.for_platform(turn.options.platform)
            .send_session_turn(turn)
    }

    fn session_id(&self, key: &str) -> Option<String> {
        self.cursor
            .session_id(key)
            .or_else(|| self.claude.session_id(key))
    }

    fn session_context_chars(&self, key: &str) -> Option<u64> {
        self.cursor
            .session_context_chars(key)
            .or_else(|| self.claude.session_context_chars(key))
    }

    fn close_session(&mut self, key: &str) {
        self.cursor.close_session(key);
        self.claude.close_session(key);
    }

    fn poll_run(&mut self, id: RunId) -> Option<super::provider::AgentRunState> {
        self.cursor.poll_run(id).or_else(|| self.claude.poll_run(id))
    }

    fn respond_to_permission(&mut self, id: RunId, option_id: &str) -> Result<()> {
        if self.cursor.respond_to_permission(id, option_id).is_ok() {
            return Ok(());
        }
        self.claude.respond_to_permission(id, option_id)
    }

    fn cancel_run(&mut self, id: RunId) -> Result<()> {
        let _ = self.cursor.cancel_run(id);
        let _ = self.claude.cancel_run(id);
        Ok(())
    }

    fn interview_status_counts(&self) -> InterviewAgentCounts {
        sum_interview_counts(
            self.cursor.interview_status_counts(),
            self.claude.interview_status_counts(),
        )
    }
}
