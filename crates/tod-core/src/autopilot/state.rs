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
    /// When the run started (ms since the epoch).
    pub started_at_ms: i64,
    /// Time spent working (ms), summed over every stretch the autopilot ran:
    /// the time budget counts this, not time asleep between wakes.
    #[serde(default)]
    pub active_ms: u64,
    /// When the stretch running now began; not saved (a restart begins a
    /// new one).
    #[serde(skip)]
    pub active_since_ms: Option<i64>,
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
            active_ms: 0,
            active_since_ms: None,
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

    /// Time spent working: every finished stretch plus the one running.
    pub fn elapsed(&self) -> Duration {
        let running = self.active_since_ms.map_or(0, |since| now_ms().saturating_sub(since).max(0) as u64);
        Duration::from_millis(self.active_ms + running)
    }

    /// A working stretch begins (ending any still open).
    pub fn begin_active(&mut self) {
        self.end_active();
        self.active_since_ms = Some(now_ms());
    }

    /// The working stretch ends: its time is added to `active_ms`.
    pub fn end_active(&mut self) {
        if let Some(since) = self.active_since_ms.take() {
            self.active_ms += now_ms().saturating_sub(since).max(0) as u64;
        }
    }

    /// Adds the running stretch's time so far to `active_ms` and keeps it
    /// running, so a save holds it.
    pub fn checkpoint_active(&mut self) {
        if self.active_since_ms.is_some() {
            self.begin_active();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_asleep_does_not_count() {
        let mut state = AutopilotState::fresh();
        // Started a day ago, worked ten minutes.
        state.started_at_ms -= 24 * 60 * 60 * 1000;
        state.active_ms = 10 * 60 * 1000;
        assert!(state.elapsed() < Duration::from_secs(11 * 60));
        state.active_since_ms = Some(now_ms() - 60_000);
        state.end_active();
        let elapsed = state.elapsed();
        assert!(elapsed >= Duration::from_secs(11 * 60) && elapsed < Duration::from_secs(12 * 60), "{elapsed:?}");
        // The sum is saved; a stretch in progress is not.
        let saved: AutopilotState = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(saved.active_ms, state.active_ms);
    }
}
