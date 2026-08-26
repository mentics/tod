mod answer_pool;
mod cursor_acp;
mod mock;
mod provider;

pub use answer_pool::AnswerProcessorPoolStats;

pub use cursor_acp::CursorAcpProvider;
pub use mock::MockAgentProvider;
pub use provider::{AgentProvider, AgentRunState, DeepDiveContext, RunId};

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Shared agent handle passed through interview views.
pub type SharedAgent = Arc<Mutex<Box<dyn AgentProvider + Send>>>;

/// True while the kickoff bootstrap ACP run is still owning the researcher slot.
pub type BootstrapGate = Arc<AtomicBool>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentBackend {
    /// In-process mock — default for automated UI verification.
    Mock,
    /// Real Cursor Agent CLI over ACP.
    #[default]
    Cursor,
}

impl AgentBackend {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "cursor" | "acp" | "real" => Ok(Self::Cursor),
            other => anyhow::bail!("unknown --agent backend `{other}` (expected mock|cursor)"),
        }
    }

    pub fn create(self) -> (SharedAgent, BootstrapGate) {
        let boxed: Box<dyn AgentProvider + Send> = match self {
            Self::Mock => Box::new(MockAgentProvider::new()),
            Self::Cursor => Box::new(CursorAcpProvider::default()),
        };
        (
            Arc::new(Mutex::new(boxed)),
            Arc::new(AtomicBool::new(false)),
        )
    }
}
