//! Routes launches to Cursor or Claude ACP hosts based on [`AgentLaunchOptions::platform`].

use super::acp_host::AcpHost;
use super::cursor_acp::CursorAcpProvider;
use super::provider::{AgentProvider, AgentRunHandle, DeepDiveContext, RunId};
use crate::interview::settings::{AnswerProcessorSettings, QuestionMakerSettings};
use crate::process_bundle::InterviewAgentPrompt;
use anyhow::Result;
use std::path::PathBuf;
use tod_store::agent_traffic::{InterviewAgentCounts, SharedAgentTrafficLog};
use tod_store::{AgentLaunchOptions, AgentPlatform};

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
        answer_max: a.answer_max + b.answer_max,
        deep_dive_in_flight: a.deep_dive_in_flight + b.deep_dive_in_flight,
    }
}

impl AgentProvider for RoutingAgentProvider {
    fn start_question_maker_replenishment(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: InterviewAgentPrompt,
        pool: &QuestionMakerSettings,
        options: AgentLaunchOptions,
    ) -> Result<AgentRunHandle> {
        self.for_platform(options.platform)
            .start_question_maker_replenishment(agent_config_id, cwd, prompt, pool, options)
    }

    fn start_answer_processor(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: InterviewAgentPrompt,
        pool: &AnswerProcessorSettings,
        options: AgentLaunchOptions,
    ) -> Result<AgentRunHandle> {
        self.for_platform(options.platform).start_answer_processor(
            agent_config_id,
            cwd,
            prompt,
            pool,
            options,
        )
    }

    fn start_deep_dive_chat(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        context: DeepDiveContext,
        initial_message: Option<String>,
        options: AgentLaunchOptions,
    ) -> Result<AgentRunHandle> {
        self.for_platform(options.platform).start_deep_dive_chat(
            agent_config_id,
            cwd,
            context,
            initial_message,
            options,
        )
    }

    fn start_fleet_agent(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: String,
        options: AgentLaunchOptions,
    ) -> Result<AgentRunHandle> {
        self.for_platform(options.platform)
            .start_fleet_agent(agent_config_id, cwd, prompt, options)
    }

    fn poll_run(&mut self, id: RunId) -> Option<super::provider::AgentRunState> {
        // Drain both hosts so pool completions advance even when the run lives on the other.
        let cursor = self.cursor.poll_run(id);
        let claude = self.claude.poll_run(id);
        cursor.or(claude)
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
