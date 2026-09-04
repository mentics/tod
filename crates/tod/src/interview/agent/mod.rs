mod acp_host;
mod answer_pool;
mod cursor_acp;
mod mock;
mod provider;
mod question_maker_pool;
mod routing;

#[allow(unused_imports)] // public API surface for agent backends
pub use acp_host::{AcpHost, AgentPlatform};
#[allow(unused_imports)]
pub use cursor_acp::CursorAcpProvider;
pub use mock::MockAgentProvider;
pub use provider::{AgentProvider, AgentRunState, DeepDiveContext, RunId};
pub use routing::RoutingAgentProvider;
#[allow(unused_imports)]
pub use tod_store::AgentLaunchOptions;

use tod_store::agent_traffic::SharedAgentTrafficLog;

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
