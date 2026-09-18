//! Agent platform — which agent CLI a session talks to.

use serde::{Deserialize, Serialize};

/// Interview agent platform — persisted in `tod.yml` and shown in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPlatform {
    Cursor,
    #[serde(alias = "anthropic")]
    Claude,
}

impl Default for AgentPlatform {
    fn default() -> Self {
        Self::Claude
    }
}

impl AgentPlatform {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cursor => "Cursor",
            Self::Claude => "Claude",
        }
    }
}
