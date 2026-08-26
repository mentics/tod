use crate::interview::settings::AnswerProcessorSettings;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub use super::answer_pool::AnswerProcessorPoolStats;

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
    ResearcherReplenishment,
    AnswerProcessor,
    DeepDiveChat,
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
    pub kind: AgentRunKind,
    pub state: AgentRunState,
}

#[derive(Debug, Clone)]
pub struct DeepDiveContext {
    pub project: String,
    pub task: String,
    pub lifecycle_state: String,
    pub interview_purpose: String,
    pub interview_phase: String,
    pub question_id: String,
    pub question_body: String,
}

/// Swappable agent backend boundary (`--agent mock|cursor`).
pub trait AgentProvider {
    fn start_researcher_replenishment(
        &mut self,
        cwd: PathBuf,
        prompt: String,
    ) -> anyhow::Result<AgentRunHandle>;

    fn start_answer_processor(
        &mut self,
        cwd: PathBuf,
        prompt: String,
        pool: &AnswerProcessorSettings,
    ) -> anyhow::Result<AgentRunHandle>;

    /// Pool visibility for the interview workspace status footer.
    fn answer_processor_pool_stats(
        &self,
        cwd: &Path,
        pool: &AnswerProcessorSettings,
    ) -> AnswerProcessorPoolStats;

    fn start_deep_dive_chat(
        &mut self,
        cwd: PathBuf,
        context: DeepDiveContext,
        initial_message: Option<String>,
    ) -> anyhow::Result<AgentRunHandle>;

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState>;

    fn cancel_run(&mut self, id: RunId) -> anyhow::Result<()>;
}
