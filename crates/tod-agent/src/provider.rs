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

/// What opens a long-lived conversation: sent once, with its first message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionOpening {
    /// Human-readable name for the agent-side session, where the platform lets
    /// a client set one.
    pub title: String,
    /// Context delivered ahead of the first message.
    pub context: Option<String>,
}

/// One user message in a long-lived conversation.
///
/// Conversations are addressed by a caller-chosen `key`. The provider keeps the
/// agent process behind a key alive between turns, so only the new message is
/// sent; when no live process holds the key it resumes `resume_session_id`
/// rather than replaying history.
#[derive(Debug, Clone)]
pub struct SessionTurn {
    pub key: String,
    pub agent_config_id: String,
    pub cwd: PathBuf,
    pub options: AgentLaunchOptions,
    /// Agent-side session id the caller recorded from an earlier process.
    pub resume_session_id: Option<String>,
    /// Set on the conversation's first message only.
    pub opening: Option<SessionOpening>,
    pub message: String,
}

impl SessionTurn {
    /// Prompt content blocks in send order: the opening context (first message
    /// only), then the message.
    pub fn prompt_blocks(&self) -> Vec<String> {
        let context = self
            .opening
            .as_ref()
            .and_then(|opening| opening.context.as_deref())
            .filter(|context| !context.trim().is_empty());
        context
            .map(str::to_string)
            .into_iter()
            .chain(std::iter::once(self.message.clone()))
            .collect()
    }
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

    /// Send one message in the long-lived conversation `turn.key`, starting or
    /// resuming its agent session as needed. Poll the returned run for the reply.
    fn send_session_turn(&mut self, turn: SessionTurn) -> anyhow::Result<AgentRunHandle>;

    /// Agent-side session id for conversation `key`, once the agent assigned
    /// one. Callers persist it so a later process can resume the session.
    fn session_id(&self, key: &str) -> Option<String>;

    /// Stop the live process behind conversation `key`. The agent-side session
    /// is left intact, so a later turn can resume it.
    fn close_session(&mut self, key: &str);

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState>;

    fn cancel_run(&mut self, id: RunId) -> anyhow::Result<()>;

    /// Live in-flight counts for the global agent status bar.
    fn interview_status_counts(&self) -> InterviewAgentCounts;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::AgentPlatform;

    fn turn(opening: Option<SessionOpening>) -> SessionTurn {
        SessionTurn {
            key: "run-1".into(),
            agent_config_id: "config".into(),
            cwd: PathBuf::from("."),
            options: AgentLaunchOptions::for_platform(AgentPlatform::Claude),
            resume_session_id: None,
            opening,
            message: "hello".into(),
        }
    }

    #[test]
    fn opening_context_precedes_the_first_message() {
        let first = turn(Some(SessionOpening {
            title: "Chat".into(),
            context: Some("CONTEXT".into()),
        }));
        assert_eq!(first.prompt_blocks(), vec!["CONTEXT", "hello"]);
    }

    #[test]
    fn later_messages_carry_only_the_message() {
        assert_eq!(turn(None).prompt_blocks(), vec!["hello"]);
    }

    #[test]
    fn opening_without_context_sends_only_the_message() {
        let first = turn(Some(SessionOpening {
            title: "Chat".into(),
            context: None,
        }));
        assert_eq!(first.prompt_blocks(), vec!["hello"]);
    }
}
