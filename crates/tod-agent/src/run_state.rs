//! Where an agent run executes, and what it's doing right now.
//!
//! `RunLocation` replaces the old stringly-typed `run_kind` column: it is the
//! one place that knows the four ways tod can run an agent, and
//! `RunLocationOps` is the contract each location implements to answer "is
//! this run still alive" — the mechanism differs per location (OS pid+birth
//! token for a process tod spawned directly, a PID recorded in a state file
//! for a terminal window, and so on), but callers only ever need `is_alive`.
//!
//! `EngagementState` is deliberately never persisted (see CLAUDE.md /
//! agent-tracking-transcripts plan): it only exists while something is
//! actively watching a run's live stream, so it is always derived, never
//! stored.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The four ways tod can be running an agent process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunLocation {
    /// The agent process is a child of tod's own window process.
    LocalWindow,
    /// The agent runs in a terminal window tod launched but does not own.
    Terminal,
    /// The agent runs inside a dev container reachable from this host.
    DevContainer,
    /// The agent runs on a remote cloud VM.
    CloudVm,
}

impl RunLocation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalWindow => "local_window",
            Self::Terminal => "terminal",
            Self::DevContainer => "dev_container",
            Self::CloudVm => "cloud_vm",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "local_window" => Some(Self::LocalWindow),
            "terminal" => Some(Self::Terminal),
            "dev_container" => Some(Self::DevContainer),
            "cloud_vm" => Some(Self::CloudVm),
            _ => None,
        }
    }
}

/// What a live-watched run is doing right now. Never persisted — computed
/// fresh by whatever is actively following the run's stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngagementState {
    WaitingOnAgent,
    WaitingOnUser,
    WaitingOnOther(String),
    Done,
}

/// Shared home for every run's current `EngagementState`, keyed by the fleet
/// agent-run id. A run only has an entry while something is actively polling
/// it — nothing here is persisted; a poll loop that stops running (window
/// closed, turn finished) removes its entries rather than leaving them stale.
///
/// This is the single producer both the status bar and any per-run label
/// should read from, instead of each re-deriving status from `AgentRunState`
/// (or worse, from the durable `runtime_status` column, which only knows
/// active/done).
pub type SharedEngagementRegistry = Arc<Mutex<HashMap<String, EngagementState>>>;

pub fn shared_engagement_registry() -> SharedEngagementRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

/// A handle with enough identity for a `RunLocationOps` impl to check
/// liveness — deliberately minimal so it stays free of `tod-store` types.
#[derive(Debug, Clone)]
pub struct RunHandle {
    pub pid: Option<u32>,
    pub birth_token: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivenessResult {
    Alive,
    NotAlive,
    /// This location can't answer the question at all (e.g. no reconnect
    /// identity was ever recorded for this run).
    Unknown,
}

/// Per-location liveness check. One impl per `RunLocation` variant.
pub trait RunLocationOps {
    fn is_alive(&self, run: &RunHandle) -> LivenessResult;
}

/// Claude Code's config directory, resolved the way the CLI resolves it.
pub(crate) fn claude_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    } else {
        std::env::var_os("HOME")
    };
    home.map(|home| PathBuf::from(home).join(".claude"))
}

/// Claude Code keeps one `<session-id>.jsonl` per session, under a directory per project.
pub(crate) fn find_claude_session_log(config_dir: &Path, session_id: &str) -> Option<PathBuf> {
    let file_name = format!("{session_id}.jsonl");
    std::fs::read_dir(config_dir.join("projects"))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(&file_name))
        .find(|path| path.is_file())
}

/// How much of the tail of a transcript file to read looking for the last
/// complete line. Claude Code's jsonl lines are small; this comfortably
/// covers the last line without reading the whole (potentially huge) file.
const TAIL_READ_BYTES: u64 = 8192;

/// Read just the last complete line of `path` without reading the rest of
/// the file — the whole point of the fingerprint check is to avoid paying
/// for a full transcript read just to learn "has this changed since we last
/// looked."
fn read_last_line(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_READ_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    buf.lines().rev().find(|line| !line.trim().is_empty()).map(str::to_string)
}

/// A cheap fingerprint for "has this Claude session's transcript changed
/// since we last cached it" — the last jsonl line's `uuid` field if present,
/// else the raw line itself. Never reads more than the tail of the file (see
/// `read_last_line`), so this is safe to call opportunistically even for
/// large transcripts.
pub fn claude_transcript_fingerprint(session_id: &str) -> Option<String> {
    let config_dir = claude_config_dir()?;
    let path = find_claude_session_log(&config_dir, session_id)?;
    let last_line = read_last_line(&path)?;
    let uuid = serde_json::from_str::<serde_json::Value>(&last_line)
        .ok()
        .and_then(|value| value.get("uuid").and_then(|v| v.as_str()).map(str::to_string));
    Some(uuid.unwrap_or(last_line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_location_round_trips_through_its_string_form() {
        for location in [
            RunLocation::LocalWindow,
            RunLocation::Terminal,
            RunLocation::DevContainer,
            RunLocation::CloudVm,
        ] {
            assert_eq!(RunLocation::parse(location.as_str()), Some(location));
        }
        assert_eq!(RunLocation::parse("nonsense"), None);
    }

    #[test]
    fn fingerprint_prefers_the_last_lines_uuid() {
        let dir = std::env::temp_dir().join(format!("tod-claude-fp-{}", uuid_like()));
        let project_dir = dir.join("projects").join("some-project");
        std::fs::create_dir_all(&project_dir).unwrap();
        // SAFETY: single-threaded test process section, restored immediately after.
        unsafe {
            std::env::set_var("CLAUDE_CONFIG_DIR", &dir);
        }
        let session_id = "session-123";
        let log_path = project_dir.join(format!("{session_id}.jsonl"));
        std::fs::write(
            &log_path,
            "{\"type\":\"user\",\"uuid\":\"first\"}\n{\"type\":\"assistant\",\"uuid\":\"second\"}\n",
        )
        .unwrap();

        let fingerprint = claude_transcript_fingerprint(session_id);
        unsafe {
            std::env::remove_var("CLAUDE_CONFIG_DIR");
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(fingerprint.as_deref(), Some("second"));
    }

    fn uuid_like() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }
}
