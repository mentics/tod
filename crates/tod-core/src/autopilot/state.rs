//! What the autopilot keeps of its own between restarts. Everything it decides
//! from is in the store; this is only its run's bookkeeping, one JSON file
//! per node under the data root, replaced atomically on every save.

use super::Outcome;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Where `node`'s autopilot state is saved.
pub fn state_path(data_root: &Path, node: Uuid) -> PathBuf {
    data_root.join("autopilot").join(format!("{node}.json"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotState {
    /// When the run started (ms since the epoch): the time budget counts
    /// from here.
    pub started_at_ms: i64,
    /// Conversations started or reopened.
    pub sessions: u32,
    /// The conversation in progress, reopened by a restart.
    pub current: Option<CurrentStep>,
    /// Every finished step, in order.
    pub steps: Vec<StepRecord>,
    /// How the last run stopped; `None` while one runs.
    pub outcome: Option<Outcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentStep {
    /// `ProtocolKind::as_str`.
    pub protocol: String,
    /// The node's state when it started; a conversation is only reopened in
    /// the same state.
    pub lifecycle: String,
    pub conversation_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    /// A protocol (`implementation`, `gate_check`, …), `advance`, or
    /// `fix_failed`.
    pub step: String,
    pub from: String,
    pub to: String,
    pub conversation_id: Option<Uuid>,
    pub at_ms: i64,
}

impl AutopilotState {
    pub fn fresh() -> Self {
        Self {
            started_at_ms: now_ms(),
            sessions: 0,
            current: None,
            steps: Vec::new(),
            outcome: None,
        }
    }

    /// `node`'s saved state, or a fresh one.
    pub fn load(data_root: &Path, node: Uuid) -> Result<Self> {
        let path = state_path(data_root, node);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("read autopilot state {}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::fresh()),
            Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self, data_root: &Path, node: Uuid) -> Result<()> {
        let path = state_path(data_root, node);
        let dir = path.parent().expect("state path has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replace {}", path.display()))?;
        Ok(())
    }

    /// Time since the run started.
    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(now_ms().saturating_sub(self.started_at_ms).max(0) as u64)
    }
}
