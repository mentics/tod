//! What the autopilot keeps of its own between restarts. Everything it decides
//! from is in the store; this is only its run's bookkeeping, one JSON file
//! per node under the data root, replaced atomically on every save.

use super::Outcome;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

/// The file the app puts in a cloud node's supervisor state directory to ask
/// for a new session; the supervisor takes it (and deletes it) at its next
/// stopping point, or when it wakes.
pub const FRESH_SESSION_REQUEST_FILE: &str = "fresh-session";

/// The file the app puts in a cloud node's supervisor state directory to
/// cancel the turn in flight ("Stop now"): the supervisor sees it while the
/// turn runs, cancels it and deletes the file.
pub const STOP_NOW_REQUEST_FILE: &str = "stop-now";

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
    /// The pull request conversation's agent turns when rounds on it began
    /// to count (`doc/lifecycle/pr-readiness.md`); `None` until the first.
    #[serde(default)]
    pub pr_turns_base: Option<u32>,
    /// The user asked for the next run's agent to start a new session
    /// instead of continuing or reopening one; the run clears it as it
    /// starts ([`super::Autopilot::new`]).
    #[serde(default)]
    pub fresh_session: bool,
    /// The run's loop counters, where the app reads them to show them.
    #[serde(default)]
    pub loops: LoopCounters,
    /// Later groups of the phase's skills still owed after the step that
    /// just finished; run before anything else, so an interruption resumes
    /// at the group it stopped in instead of moving on without it.
    #[serde(default)]
    pub pending_groups: Option<PendingGroups>,
}

/// The later skill groups (`lifecycle_config::groups`, 1 and up) a finished
/// step still owes, and where it is among them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingGroups {
    /// `ProtocolKind::as_str` of the conversations they run as.
    pub protocol: String,
    /// The lifecycle config's phase key.
    pub phase_key: String,
    /// The node's state when they became owed: they are dropped if it moved.
    pub lifecycle: String,
    /// The next group to run (1-based among the later ones: group index).
    pub next: usize,
}

/// How many times the same problem may come back in the same state before
/// the run stops and asks the user ([`LoopCounters::note`]).
pub const CYCLE_LIMIT: u32 = 3;

/// How many reviews a node may have in one stay in `review` before the run
/// stops and asks the user: review, fix, review again, and so on, until a
/// review finds nothing. Reaching it means the two are not converging. This
/// is the default; `lifecycle.review_round_limit` in the settings overrides it.
pub const REVIEW_ROUND_LIMIT: u32 = tod_store::settings::DEFAULT_REVIEW_ROUND_LIMIT;

/// Everything that counts a loop the run is in. Sessions
/// ([`AutopilotState::sessions`]) and the pull request's start
/// ([`AutopilotState::pr_turns_base`]) stay beside it; this holds the rest,
/// so one place says how far each loop has gone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopCounters {
    /// Rounds of work on the pull request since they began to count.
    #[serde(default)]
    pub pr_rounds: u32,
    /// Evaluator rejections in the current stay in the state being judged.
    #[serde(default)]
    pub evaluator_rejections: u32,
    /// Reviews started in the current stay in `review`. Each is a round of
    /// review, then fix; the run stops for the user at [`REVIEW_ROUND_LIMIT`].
    #[serde(default)]
    pub review_rounds: u32,
    /// Times a failed verification sent the node back to be fixed.
    #[serde(default)]
    pub fix_cycles: u32,
    /// How often each problem has come back, keyed `<state>|<what>`
    /// (a failing obligation, or what an evaluator asked for).
    #[serde(default)]
    pub cycles: BTreeMap<String, u32>,
}

impl LoopCounters {
    /// `what` failed again in `state`: its count so far, this time included.
    pub fn note(&mut self, state: &str, what: &str) -> u32 {
        let count = self.cycles.entry(format!("{state}|{what}")).or_insert(0);
        *count += 1;
        *count
    }

    /// Forgets what `state` had counted that is not in `still`: those
    /// problems are gone, so a different one failing next is progress.
    pub fn keep_only(&mut self, state: &str, still: &[String]) {
        let prefix = format!("{state}|");
        self.cycles.retain(|key, _| {
            key.strip_prefix(&prefix)
                .is_none_or(|what| still.iter().any(|s| s == what))
        });
    }

    /// Starts counting again: the user has seen why the run stopped.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentStep {
    /// `ProtocolKind::as_str`.
    pub protocol: String,
    /// The node's state when it started; a conversation is only reopened in
    /// the same state.
    pub lifecycle: String,
    pub conversation_id: Uuid,
    /// The group of the phase's skills the conversation works with (0: the
    /// phase agent's own), so a restart reopens the right session.
    #[serde(default)]
    pub skill_group: usize,
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
            pr_turns_base: None,
            fresh_session: false,
            loops: LoopCounters::default(),
            pending_groups: None,
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

    /// Ask that the next run of `node` start its agent in a new session.
    pub fn request_fresh_session(data_root: &Path, node: Uuid) -> Result<()> {
        let mut state = Self::load(data_root, node)?;
        state.fresh_session = true;
        state.save(data_root, node)
    }

    /// Ask that the next run of `node` redo its state from the top, as if it
    /// had just entered it: a new session, and none of the earlier run's
    /// conversation, owed skill groups or loop counts carried over.
    pub fn request_rerun_phase(data_root: &Path, node: Uuid) -> Result<()> {
        let mut state = Self::load(data_root, node)?;
        state.fresh_session = true;
        state.current = None;
        state.pending_groups = None;
        state.loops.clear();
        state.outcome = None;
        state.save(data_root, node)
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
    fn fresh_session_request_is_saved_and_old_files_load() {
        let dir = std::env::temp_dir().join(format!("tod-fresh-{}", Uuid::new_v4()));
        let node = Uuid::new_v4();
        assert!(!AutopilotState::load(&dir, node).unwrap().fresh_session);
        AutopilotState::request_fresh_session(&dir, node).unwrap();
        assert!(AutopilotState::load(&dir, node).unwrap().fresh_session);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_problem_that_comes_back_counts_and_a_fixed_one_is_forgotten() {
        let mut loops = LoopCounters::default();
        assert_eq!(loops.note("verifying", "a"), 1);
        assert_eq!(loops.note("verifying", "a"), 2);
        assert_eq!(loops.note("verifying", "b"), 1);
        loops.keep_only("verifying", &["b".to_string()]);
        assert_eq!(loops.note("verifying", "a"), 1);
        assert_eq!(loops.note("verifying", "b"), 2);
        // Other states' counts are left alone.
        loops.note("review", "x");
        loops.keep_only("verifying", &[]);
        assert_eq!(loops.cycles.len(), 1);
        // Saved states from before the counters load.
        let old: AutopilotState = serde_json::from_str(
            r#"{"started_at_ms":1,"sessions":2,"current":null,"steps":[],"outcome":null}"#,
        )
        .unwrap();
        assert_eq!(old.loops, LoopCounters::default());
    }

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
