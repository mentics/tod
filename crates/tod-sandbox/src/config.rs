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
}

fn default_region() -> String {
    "us-pdx-1".into()
}
fn default_image() -> String {
    "blaxel/base-image:latest".into()
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub blaxel: Option<Account>,
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
        });
        c.upsert(Sandbox { name: "a".into(), image: "i".into(), url: None, agents: true });
        c.upsert(Sandbox { name: "a".into(), image: "j".into(), url: Some("u".into()), agents: true });
        c.save(&path).unwrap();
        let back = Config::load(&path).unwrap();
        assert_eq!(back.sandboxes.len(), 1);
        assert_eq!(back.sandbox("a").unwrap().image, "j");
        assert_eq!(back.blaxel.unwrap().auth, AuthMode::ApiKey);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
