use crate::agent_launch::AgentLaunchOptions;
use crate::agent_traffic::{AgentCategory, InterviewAgentCounts};
use crate::devcontainer::AgentEnvironment;
use crate::platform::AgentPlatform;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

/// An agent session a provider started or resumed, reported as soon as the
/// agent gives its id — before its first turn ends, so a turn that never
/// ends still leaves its session findable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStarted {
    pub platform: AgentPlatform,
    pub agent_session_id: String,
    /// The caller's key for the session: a [`SessionTurn::key`], or the owner
    /// id of a fleet-agent run. Its traffic is filed under the same key.
    pub key: String,
    pub title: String,
    pub cwd: PathBuf,
}

/// Hears of every session a provider starts or resumes, on the thread that
/// learned of it. Keep it quick: that thread is driving the agent.
pub type SessionObserver = Arc<dyn Fn(SessionStarted) + Send + Sync>;

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
    FleetAgent,
}

impl AgentRunKind {
    pub(crate) fn traffic_category(self) -> AgentCategory {
        match self {
            Self::QuestionMakerReplenishment => AgentCategory::QuestionMaker,
            Self::AnswerProcessor => AgentCategory::AnswerProcessor,
            Self::FleetAgent => AgentCategory::Fleet,
        }
    }

    /// How a run's traffic is listed when its session has no name.
    pub(crate) fn traffic_label(self) -> &'static str {
        match self {
            Self::QuestionMakerReplenishment => "question-maker",
            Self::AnswerProcessor => "answer-processor",
            Self::FleetAgent => "fleet-agent",
        }
    }
}

/// What a long-lived conversation is for. Only affects how its traffic is
/// labeled and counted; the transport treats every conversation the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionPurpose {
    /// A user-facing chat.
    #[default]
    Chat,
    /// An interview question maker session.
    QuestionMaker,
    /// An interview answer processor session.
    AnswerProcessor,
    /// Legacy: the removed drafter. Kept because the interview still maps
    /// its legacy `Drafter` role here.
    Drafter,
    /// The conversation view's agent: talks with the user, and acts on the
    /// project through `tod-cli` as the conversation.
    Conversation,
}

impl SessionPurpose {
    pub(crate) fn run_kind(self) -> AgentRunKind {
        match self {
            Self::Chat | Self::Conversation => AgentRunKind::FleetAgent,
            Self::QuestionMaker | Self::Drafter => {
                AgentRunKind::QuestionMakerReplenishment
            }
            Self::AnswerProcessor => AgentRunKind::AnswerProcessor,
        }
    }
}

/// One choice offered by an agent's permission request (e.g. "Allow once",
/// "Deny").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
}

/// An agent is blocked waiting for the user to allow or deny an action. The
/// run stays `NeedsPermission` until [`AgentProvider::respond_to_permission`]
/// is called with one of `options`' ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    pub run: RunId,
    /// Human-readable description of the action the agent wants to take.
    pub title: String,
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunState {
    /// Still running. The payload is a short, human-readable description of
    /// what the agent is doing right now (a tool call, a permission request,
    /// …), when the provider can report one.
    InFlight(Option<String>),
    /// The agent is paused waiting on a permission decision. Callers should
    /// surface `request` to the user and answer it with
    /// [`AgentProvider::respond_to_permission`]; the run stays in this state
    /// (or reports a fresh request) until then.
    NeedsPermission(PermissionRequest),
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
    pub owner_id: String,
    /// Human-readable session name, the same on every turn. A session this
    /// turn creates is given it (where the platform lets a client name one),
    /// and the session's traffic is listed under it. Empty means unnamed.
    pub title: String,
    pub cwd: PathBuf,
    pub options: AgentLaunchOptions,
    /// Agent-side session id the caller recorded from an earlier process.
    pub resume_session_id: Option<String>,
    /// Set on the conversation's first message only.
    pub opening: Option<SessionOpening>,
    pub message: String,
    pub purpose: SessionPurpose,
    /// Extra environment for the agent process. Applied when the process for
    /// this key is started, so it must stay the same for the life of the key.
    pub env: Vec<(String, String)>,
    /// Where the agent process runs. With a dev container, `cwd` is the host
    /// directory it maps from; like `env`, it applies when the process starts.
    pub environment: AgentEnvironment,
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
    /// Start an autonomous fleet agent run for a saved agent config.
    ///
    /// `session_title` names the agent-side session the same way
    /// [`SessionTurn::title`] does for chat turns — callers build both with
    /// the same naming convention so a session looks the same no matter how it
    /// was started; empty means don't name it. Platforms that expose no rename
    /// mechanism (Cursor) ignore it. `environment` is where it runs, as for
    /// [`SessionTurn::environment`].
    fn start_fleet_agent(
        &mut self,
        owner_id: &str,
        cwd: PathBuf,
        prompt: String,
        options: AgentLaunchOptions,
        session_title: String,
        environment: AgentEnvironment,
    ) -> anyhow::Result<AgentRunHandle>;

    /// Send one message in the long-lived conversation `turn.key`, starting or
    /// resuming its agent session as needed. Poll the returned run for the reply.
    fn send_session_turn(&mut self, turn: SessionTurn) -> anyhow::Result<AgentRunHandle>;

    /// Agent-side session id for conversation `key`, once the agent assigned
    /// one. Callers persist it so a later process can resume the session.
    fn session_id(&self, key: &str) -> Option<String>;

    /// Agent-side session id for the one-shot fleet-agent run `id` (see
    /// [`AgentProvider::start_fleet_agent`]), once the agent assigned one.
    /// Unlike `session_id`, this is keyed by `RunId` rather than a caller
    /// key, because fleet-agent runs have no long-lived conversation entry.
    /// Callers persist it the same way they persist `session_id`.
    fn fleet_run_session_id(&self, id: RunId) -> Option<String>;


    /// Characters that have entered conversation `key`'s context since this
    /// provider started holding it: prompts sent, replies, and tool output the
    /// agent reported. A size estimate for callers deciding when to rotate.
    fn session_context_chars(&self, key: &str) -> Option<u64>;

    /// The latest turn of conversation `key`, part by part — text, thoughts,
    /// and tool calls in the order the agent streamed them. `None` when the
    /// provider does not report parts; callers then have only the reply text.
    fn session_reply_parts(&self, _key: &str) -> Option<Vec<crate::ReplyPart>> {
        None
    }

    /// The tokens conversation `key`'s agent reported spending while this
    /// provider held it — each turn's totals, the context's size, the cost
    /// — when it reports any. The platform's own record of the session
    /// (`read_transcript`) says more, where it keeps usage at all.
    fn session_token_usage(&self, _key: &str) -> Option<crate::TokenUsage> {
        None
    }

    /// Stop the live process behind conversation `key`. The agent-side session
    /// is left intact, so a later turn can resume it.
    fn close_session(&mut self, key: &str);

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState>;

    /// Answer a pending [`AgentRunState::NeedsPermission`] for `id` by
    /// selecting one of its options. Errors if the run has no pending
    /// permission request.
    fn respond_to_permission(&mut self, id: RunId, option_id: &str) -> anyhow::Result<()>;

    fn cancel_run(&mut self, id: RunId) -> anyhow::Result<()>;

    /// Live in-flight counts for the global agent status bar.
    fn interview_status_counts(&self) -> InterviewAgentCounts;

    /// Report every session started or resumed from now on to `observer`.
    /// Providers with no agent-side sessions (the mock) ignore it.
    fn set_session_observer(&mut self, _observer: SessionObserver) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::AgentPlatform;

    fn turn(opening: Option<SessionOpening>) -> SessionTurn {
        SessionTurn {
            key: "run-1".into(),
            owner_id: "config".into(),
            title: "Chat".into(),
            cwd: PathBuf::from("."),
            options: AgentLaunchOptions::for_platform(AgentPlatform::Claude),
            resume_session_id: None,
            opening,
            message: "hello".into(),
            purpose: SessionPurpose::Chat,
            env: Vec::new(),
            environment: AgentEnvironment::Host,
        }
    }

    #[test]
    fn opening_context_precedes_the_first_message() {
        let first = turn(Some(SessionOpening {
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
            context: None,
        }));
        assert_eq!(first.prompt_blocks(), vec!["hello"]);
    }
}
