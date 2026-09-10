use crate::agent_launch::AgentLaunchOptions;
use crate::agent_traffic::InterviewAgentCounts;
use crate::prompt::{AgentPrompt, SessionPoolConfig};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunId(Uuid);

impl RunId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRunKind {
    QuestionMakerReplenishment,
    AnswerProcessor,
    DeepDiveChat,
    FleetAgent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunState {
    InFlight,
    Success(Option<String>),
    Failure(String),
}

#[derive(Debug, Clone)]
pub struct AgentRunHandle {
    pub id: RunId,
}

/// Swappable agent backend boundary (`--agent mock|cursor|claude`).
pub trait AgentProvider {
    fn start_question_maker_replenishment(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: AgentPrompt,
        pool: &SessionPoolConfig,
        options: AgentLaunchOptions,
    ) -> anyhow::Result<AgentRunHandle>;

    fn start_answer_processor(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: AgentPrompt,
        pool: &SessionPoolConfig,
        options: AgentLaunchOptions,
    ) -> anyhow::Result<AgentRunHandle>;

    fn start_deep_dive_chat(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: String,
        options: AgentLaunchOptions,
    ) -> anyhow::Result<AgentRunHandle>;

    /// Start an autonomous fleet agent run for a saved agent config.
    fn start_fleet_agent(
        &mut self,
        agent_config_id: &str,
        cwd: PathBuf,
        prompt: String,
        options: AgentLaunchOptions,
    ) -> anyhow::Result<AgentRunHandle>;

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState>;

    fn cancel_run(&mut self, id: RunId) -> anyhow::Result<()>;

    /// Live in-flight counts for the global agent status bar.
    fn interview_status_counts(&self) -> InterviewAgentCounts;
}
