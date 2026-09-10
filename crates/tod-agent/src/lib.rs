//! Agent transport — how tod holds conversations and sessions with agents.
//!
//! Owns the provider interface and its implementations across agent platforms
//! (Cursor, Claude, mock) and, in future, environments (host shell, container,
//! cloud). This crate has no `tod-*` dependencies by design: it does not decide
//! where anything is stored or when — it is told what to say and reports back.

mod acp_host;
pub mod agent_launch;
pub mod agent_traffic;
mod answer_pool;
mod cursor_acp;
mod mock;
pub mod platform;
mod provider;
mod question_maker_pool;
mod routing;
pub mod util;

pub use agent_launch::{AgentLaunchOptions, effort_for_acp};
pub use platform::AgentPlatform;
pub use prompt::{AgentPrompt, SessionPoolConfig};
pub use question_maker_pool::RESEARCHER_SESSION_POOL_SIZE;

mod prompt {
    /// A prompt split so a pooled session can skip re-sending its preamble.
    ///
    /// Built by callers (`tod-core` assembles it from process docs); this crate
    /// only decides which half a given slot needs.
    #[derive(Debug, Clone, Default)]
    pub struct AgentPrompt {
        pub session_prefix: String,
        pub turn: String,
    }

    impl AgentPrompt {
        /// Full prompt for a brand-new session.
        pub fn full(&self) -> String {
            self.for_slot(0)
        }

        /// Prompt for a pooled slot that has already handled `responses_received` turns.
        pub fn for_slot(&self, responses_received: u32) -> String {
            if responses_received == 0 && !self.session_prefix.is_empty() {
                format!(
                    "{}

{}",
                    self.session_prefix.trim_end(),
                    self.turn
                )
            } else {
                self.turn.clone()
            }
        }
    }

    /// Session-reuse tuning for a provider pool.
    ///
    /// Transport-level only: how many concurrent sessions to hold and how many
    /// turns to reuse each for. Interview policy (when to replenish, when a
    /// second question maker is warranted) stays in `tod-core`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SessionPoolConfig {
        pub pool_size: u32,
        pub runs_per_session: u32,
    }

    impl SessionPoolConfig {
        pub fn new(pool_size: u32, runs_per_session: u32) -> Self {
            Self {
                pool_size,
                runs_per_session,
            }
        }
    }

    impl Default for SessionPoolConfig {
        fn default() -> Self {
            Self::new(4, 16)
        }
    }
}

#[allow(unused_imports)] // public API surface for agent backends
pub use acp_host::AcpHost;
use agent_traffic::SharedAgentTrafficLog;
#[allow(unused_imports)]
pub use cursor_acp::CursorAcpProvider;
pub use mock::MockAgentProvider;
pub use provider::{AgentProvider, AgentRunState, RunId};
pub use routing::RoutingAgentProvider;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Shared agent handle passed through interview views.
pub type SharedAgent = Arc<Mutex<Box<dyn AgentProvider + Send>>>;

/// True while the kickoff bootstrap ACP run is still owning the question maker slot.
pub type BootstrapGate = Arc<AtomicBool>;

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

    pub fn create(self, traffic_log: SharedAgentTrafficLog) -> (SharedAgent, BootstrapGate) {
        (
            Arc::new(Mutex::new(self.build_provider(traffic_log))),
            Arc::new(AtomicBool::new(false)),
        )
    }
}
