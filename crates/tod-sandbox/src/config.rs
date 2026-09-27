//! `sandboxes.toml`: the Blaxel account tod uses and the sandboxes it knows.
//!
//! The caller decides where the file lives (tod keeps it in the data root).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const FILE_NAME: &str = "sandboxes.toml";

/// Hosts ending in this are sandboxes (`ssh://root@<name>.tod/...` in Zed);
/// any other host goes to the real `ssh`.
pub const HOST_SUFFIX: &str = ".tod";

pub fn host_for(name: &str) -> String {
    format!("{name}{HOST_SUFFIX}")
}

/// The sandbox name for an ssh destination (`[user@]<name>.tod`), if it is one.
pub fn name_for_host(dest: &str) -> Option<&str> {
    let host = dest.rsplit('@').next().unwrap_or(dest);
    host.strip_suffix(HOST_SUFFIX).filter(|n| !n.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMode {
    /// The user's own `bl login` session (`bl token`).
    #[default]
    Bl,
    /// A workspace API key kept in tod's credential store.
    ApiKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub workspace: String,
    #[serde(default = "default_region")]
    pub region: String,
    #[serde(default)]
    pub auth: AuthMode,
    #[serde(default = "default_image")]
    pub default_image: String,
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
    /// Put in each new sandbox's `tod-owner` label, so a shared workspace
    /// shows whose sandbox is whose.
    #[serde(default)]
    pub owner: Option<String>,
    /// How a node's Claude Code gets the user's subscription token
    /// (`claude_token_via = "proxy" | "env"`); see [`ClaudeTokenVia`].
    #[serde(default)]
    pub claude_token_via: ClaudeTokenVia,
    /// The orchestrator sandbox's name (`orchestrator = "..."`): the one
    /// `tod-sandbox orchestrator` deploys and nodes reach. Defaults to
    /// [`crate::orchestrator::NAME`].
    #[serde(default = "default_orchestrator", skip_serializing_if = "is_default_orchestrator")]
    pub orchestrator: String,
    /// The Blaxel volume that holds the orchestrator's data (`/data`), so it
    /// outlives the orchestrator's sandbox (`orchestrator_volume = "..."`).
    /// Unset: the sandbox's own disk, lost with it (an account without
    /// volumes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_volume: Option<String>,
    /// Node sandboxes are forked from a prepared base sandbox of this name
    /// (`node_base = "..."`), made on first use; unset, or where forking is
    /// refused, each is created from the image. See `tod_core::cloud_sync`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_base: Option<String>,
}

fn default_orchestrator() -> String {
    crate::orchestrator::NAME.to_string()
}

fn is_default_orchestrator(name: &String) -> bool {
    name == crate::orchestrator::NAME
}

/// How the Claude subscription token (`claude setup-token`) reaches the
/// Claude Code agent in an autonomous node's sandbox
/// (`doc/cloud-sandboxes/autonomous-nodes.md`, Credentials).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaudeTokenVia {
    /// The sandbox's proxy adds it to requests for `api.anthropic.com`;
    /// Claude Code sees only a placeholder. The token never enters the
    /// sandbox.
    #[default]
    Proxy,
    /// It is put in the supervisor's environment only (so Claude Code, its
    /// child, has it), never in the sandbox-wide one.
    Env,
}

/// What a new sandbox starts from when neither the account nor the request
/// names an image.
pub const DEFAULT_IMAGE: &str = "blaxel/base-image:latest";

/// Where new sandboxes are created unless the account says otherwise. Agent
/// Drive is only available here.
pub const DEFAULT_REGION: &str = "us-was-1";

impl Account {
    /// An account signed in with `bl login`, with the defaults.
    pub fn new(workspace: impl Into<String>) -> Self {
        Self {
            workspace: workspace.into(),
            region: default_region(),
            auth: AuthMode::Bl,
            default_image: default_image(),
            memory_mb: default_memory(),
            owner: std::env::var("USERNAME").or_else(|_| std::env::var("USER")).ok(),
            claude_token_via: ClaudeTokenVia::default(),
            orchestrator: default_orchestrator(),
            orchestrator_volume: None,
            node_base: None,
        }
    }
}

fn default_region() -> String {
    DEFAULT_REGION.into()
}
fn default_image() -> String {
    DEFAULT_IMAGE.into()
}
fn default_memory() -> u32 {
    4096
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sandbox {
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub agents: bool,
}

/// Who keeps a waiting node's wake timer (`tod_core::scheduler`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SchedulerKind {
    /// The orchestrator's own timer (`POST /wakes`): the development account,
    /// where Blaxel schedules are unverified.
    #[default]
    Orchestrator,
    /// A Blaxel schedule on the node's own sandbox, run as process `wait-<id>`.
    Blaxel,
}

impl SchedulerKind {
    /// Its name in `sandboxes.toml` and in a node's `TOD_SCHEDULER`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Orchestrator => "orchestrator",
            Self::Blaxel => "blaxel",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "orchestrator" => Some(Self::Orchestrator),
            "blaxel" => Some(Self::Blaxel),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub blaxel: Option<Account>,
    /// `scheduler = "orchestrator" | "blaxel"`; defaults to the orchestrator.
    #[serde(default)]
    pub scheduler: SchedulerKind,
    #[serde(default, rename = "sandbox")]
    pub sandboxes: Vec<Sandbox>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = format!(
            "# tod's cloud sandboxes; managed by `tod-sandbox`.\n{}",
            toml::to_string_pretty(self)?
        );
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))
    }

    pub fn sandbox(&self, name: &str) -> Option<&Sandbox> {
        self.sandboxes.iter().find(|s| s.name == name)
    }

    pub fn upsert(&mut self, sandbox: Sandbox) {
        match self.sandboxes.iter_mut().find(|s| s.name == sandbox.name) {
            Some(existing) => *existing = sandbox,
            None => self.sandboxes.push(sandbox),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_sandbox_hosts() {
        assert_eq!(name_for_host("root@dev.tod"), Some("dev"));
        assert_eq!(name_for_host("dev.tod"), Some("dev"));
        assert_eq!(name_for_host("github.com"), None);
        assert_eq!(name_for_host(".tod"), None);
    }

    #[test]
    fn claude_token_via_defaults_to_the_proxy() {
        let c: Config = toml::from_str("[blaxel]
workspace = \"w\"
").unwrap();
        assert_eq!(c.blaxel.unwrap().claude_token_via, ClaudeTokenVia::Proxy);
        let c: Config = toml::from_str("[blaxel]
workspace = \"w\"
claude_token_via = \"env\"
").unwrap();
        assert_eq!(c.blaxel.unwrap().claude_token_via, ClaudeTokenVia::Env);
    }

    #[test]
    fn the_orchestrator_volume_and_node_base_are_off_by_default() {
        let c: Config = toml::from_str("scheduler = \"blaxel\"\n[blaxel]\nworkspace = \"w\"\n").unwrap();
        assert_eq!(c.scheduler, SchedulerKind::Blaxel);
        let acct = c.blaxel.unwrap();
        assert_eq!(acct.orchestrator, crate::orchestrator::NAME);
        assert_eq!(acct.orchestrator_volume, None);
        assert_eq!(acct.node_base, None);
        // The default name is not written back.
        assert!(!toml::to_string(&acct).unwrap().contains("orchestrator"));
        assert_eq!(SchedulerKind::parse(SchedulerKind::Blaxel.as_str()), Some(SchedulerKind::Blaxel));
        assert_eq!(SchedulerKind::parse("nope"), None);
    }

    #[test]
    fn round_trips() {
        let dir = std::env::temp_dir().join(format!("tod-sandbox-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        assert!(Config::load(&path).unwrap().blaxel.is_none());
        let mut c = Config::default();
        c.blaxel = Some(Account {
            workspace: "w".into(),
            region: default_region(),
            auth: AuthMode::ApiKey,
            default_image: default_image(),
            memory_mb: 4096,
            owner: Some("me".into()),
            claude_token_via: ClaudeTokenVia::Env,
            orchestrator: "orch-2".into(),
            orchestrator_volume: Some("orch-data".into()),
            node_base: Some("base".into()),
        });
        c.upsert(Sandbox { name: "a".into(), image: "i".into(), url: None, agents: true });
        c.upsert(Sandbox { name: "a".into(), image: "j".into(), url: Some("u".into()), agents: true });
        c.save(&path).unwrap();
        let back = Config::load(&path).unwrap();
        assert_eq!(back.sandboxes.len(), 1);
        assert_eq!(back.sandbox("a").unwrap().image, "j");
        let acct = back.blaxel.unwrap();
        assert_eq!(acct.auth, AuthMode::ApiKey);
        assert_eq!(acct.claude_token_via, ClaudeTokenVia::Env);
        assert_eq!(acct.orchestrator, "orch-2");
        assert_eq!(acct.orchestrator_volume.as_deref(), Some("orch-data"));
        assert_eq!(acct.node_base.as_deref(), Some("base"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
