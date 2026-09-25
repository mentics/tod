//! Agent transport — how tod holds conversations and sessions with agents.
//!
//! Owns the provider interface and its implementations across agent platforms
//! (Cursor, Claude, mock) and, in future, environments (host shell, container,
//! cloud). This crate has no `tod-*` dependencies by design: it does not decide
//! where anything is stored or when — it is told what to say and reports back.

mod acp_host;
pub mod agent_launch;
pub mod agent_traffic;
mod cursor_acp;
pub mod devcontainer;
mod mock;
pub mod platform;
mod process_tree;
mod provider;
mod reply;
mod routing;
pub mod run_state;
pub mod sandbox;
mod transcript;
mod usage;
pub mod util;

pub use agent_launch::{AgentLaunchOptions, effort_for_acp};
pub use devcontainer::AgentEnvironment;
pub use platform::AgentPlatform;
pub use run_state::{
    EngagementState, LivenessResult, RunHandle, RunLocation, RunLocationOps,
    SharedEngagementRegistry, claude_transcript_fingerprint, shared_engagement_registry,
};

#[allow(unused_imports)] // public API surface for agent backends
pub use acp_host::AcpHost;
use agent_traffic::SharedAgentTrafficLog;
#[allow(unused_imports)]
pub use cursor_acp::CursorAcpProvider;
pub use mock::{
    MockAgentProvider, MockInterviewHandler, MockInterviewTurn, MockReply, mock_gate_check_reply,
    set_mock_interview_handler,
};
pub use provider::{
    AgentProvider, AgentRunHandle, AgentRunState, PermissionOption, PermissionRequest, RunId,
    SessionObserver, SessionOpening, SessionPurpose, SessionStarted, SessionTurn,
};
pub use reply::ReplyPart;
pub use routing::RoutingAgentProvider;
pub use transcript::{
    FormatProblem, Transcript, TranscriptRead, TranscriptTurn, read_transcript,
    transcript_fingerprint,
};
pub use usage::{Cost, TokenCounts, TokenUsage};

use std::sync::{Arc, Mutex};

/// Shared agent handle passed through interview views.
pub type SharedAgent = Arc<Mutex<Box<dyn AgentProvider + Send>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentBackend {
    /// In-process mock — default for automated UI verification.
    Mock,
    /// Real Cursor Agent CLI over ACP.
    #[default]
    Cursor,
    /// Claude Agent CLI over ACP.
    Claude,
}

impl AgentBackend {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "cursor" | "acp" | "real" => Ok(Self::Cursor),
            "claude" | "anthropic" => Ok(Self::Claude),
            other => {
                anyhow::bail!("unknown --agent backend `{other}` (expected mock|cursor|claude)")
            }
        }
    }

    pub fn from_platform(platform: AgentPlatform) -> Self {
        match platform {
            AgentPlatform::Cursor => Self::Cursor,
            AgentPlatform::Claude => Self::Claude,
        }
    }

    pub fn build_provider(
        self,
        traffic_log: SharedAgentTrafficLog,
    ) -> Box<dyn AgentProvider + Send> {
        match self {
            Self::Mock => Box::new(MockAgentProvider::new().with_traffic_log(traffic_log)),
            // Non-mock: always route so per-config / settings platform can pick either host.
            Self::Cursor | Self::Claude => Box::new(RoutingAgentProvider::new(traffic_log)),
        }
    }

    pub fn create(self, traffic_log: SharedAgentTrafficLog) -> SharedAgent {
        Arc::new(Mutex::new(self.build_provider(traffic_log)))
    }
}
