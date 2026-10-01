//! `tod-agentd`: the resident daemon, one per data root (`doc/agentd.md`).
//!
//! `tod` and `tod-cli` link this library to find the daemon, start it when
//! none is running, and talk to it; the `tod-agentd` binary is [`server::run`].
//! Nothing here depends on the app or the agent transport.
//!
//! Everything lives in `<data root>/daemon/`: `agentd.json` ([`Info`]: pid,
//! loopback port, token, build), `agentd.lock` (held by the running daemon),
//! `start.lock` (held by whoever is starting one), `agentd.log`, and the
//! copies of the executable the daemon runs from.
//!
//! The wire protocol is one JSON object per line each way, over loopback
//! TCP. Every request carries the token from `agentd.json`, which only a
//! process that can read the data root can know.

pub mod client;
pub mod server;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A hash of the source this build is made from.
pub const BUILD_STAMP: &str = env!("TOD_AGENTD_BUILD_STAMP");
/// When the source of this build last changed (seconds since the epoch):
/// of two different builds, the larger is newer.
pub const BUILT_AT: u64 = parse_u64(env!("TOD_AGENTD_BUILT_AT"));
/// The wire protocol's version. A change that an older client or daemon could
/// misread bumps it.
pub const PROTOCOL: u32 = 1;

const fn parse_u64(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let mut n = 0u64;
    let mut i = 0;
    while i < bytes.len() {
        n = n * 10 + (bytes[i] - b'0') as u64;
        i += 1;
    }
    n
}

/// Where the daemon's files are.
#[derive(Debug, Clone)]
pub struct Paths {
    dir: PathBuf,
}

impl Paths {
    pub fn new(data_root: &Path) -> Self {
        Self { dir: data_root.join("daemon") }
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn info(&self) -> PathBuf {
        self.dir.join("agentd.json")
    }
    pub fn daemon_lock(&self) -> PathBuf {
        self.dir.join("agentd.lock")
    }
    pub fn start_lock(&self) -> PathBuf {
        self.dir.join("start.lock")
    }
    pub fn log(&self) -> PathBuf {
        self.dir.join("agentd.log")
    }
    /// The copy of the executable for `stamp` that the daemon runs from.
    pub fn executable(&self, stamp: &str) -> PathBuf {
        let name = format!("tod-agentd-{stamp}{}", std::env::consts::EXE_SUFFIX);
        self.dir.join(name)
    }
}

/// How to reach a running daemon, and which build it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    pub pid: u32,
    pub port: u16,
    pub token: String,
    pub stamp: String,
    pub built_at: u64,
    pub protocol: u32,
}

impl Info {
    pub fn read(paths: &Paths) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(paths.info()).ok()?).ok()
    }
}

/// A request: the token, then the command.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub token: String,
    #[serde(flatten)]
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Who are you? Answers with the daemon's [`Info`].
    Hello,
    Ping,
    /// Drain, then exit. The reply is sent first.
    Quit,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<Info>,
}

impl Response {
    pub fn ok() -> Self {
        Self { ok: true, error: None, info: None }
    }
    pub fn err(message: impl Into<String>) -> Self {
        Self { ok: false, error: Some(message.into()), info: None }
    }
}
