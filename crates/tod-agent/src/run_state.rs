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
