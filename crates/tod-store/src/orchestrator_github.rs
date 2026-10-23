//! The GitHub tokens the user gives the orchestrator, and which repositories
//! each one reads (Settings → Orchestrator GitHub access).
//!
//! The orchestrator reads GitHub through its sandbox's proxy, which holds
//! every token under a name and matches only on host, so the orchestrator
//! chooses per repository (`crate::github_routes`). Here is the user's side
//! of that: a list of tokens, each with a label and the repositories it is
//! for (`owner/repo`, `owner/*`, or `*`). The list (labels and patterns) is
//! `orchestrator-github.json` in the data root; each token itself is in the
//! credential store (keyring, else the encrypted file), never in that file.
//! `tod_core::orchestrator_github::apply` sends them to the orchestrator.

use crate::credentials::{CredentialError, CredentialStore};
use crate::github_routes::{GithubRoutes, Route};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const FILE_NAME: &str = "orchestrator-github.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Letters, digits, `-`; names the proxy secret and the stored token.
    pub id: String,
    /// What the user calls it ("Acme org").
    pub label: String,
    /// `owner/repo`, `owner/*` or `*`: the repositories this token reads.
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub tokens: Vec<Entry>,
}

/// The proxy secret holding `entry`'s token.
pub fn secret_name(id: &str) -> String {
    format!("gh_{}", tod_sandbox::node::secret_name(id))
}

fn account(id: &str) -> String {
    format!("orchestrator-github-{id}")
}

/// A well-formed repository pattern, or why not.
pub fn check_pattern(pattern: &str) -> Result<(), String> {
    let pattern = pattern.trim();
    if pattern == "*" {
        return Ok(());
    }
    let ok_name = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    match pattern.split_once('/') {
        Some((owner, "*")) if ok_name(owner) => Ok(()),
        Some((owner, repo)) if ok_name(owner) && ok_name(repo) => Ok(()),
        _ => Err(format!("{pattern:?} is not owner/repo, owner/*, or *")),
    }
}

/// An id for a new entry labelled `label` that no entry in `existing` has.
pub fn new_id(label: &str, existing: &[Entry]) -> String {
    let slug: String = label
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base = if slug.is_empty() { "token".to_string() } else { slug };
    let mut id = base.clone();
    let mut n = 2;
    while existing.iter().any(|e| e.id == id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

impl Config {
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = root.join(FILE_NAME);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| anyhow::anyhow!("{} is not readable: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow::anyhow!("read {}: {e}", path.display())),
        }
    }

    /// Checks, then writes. A token's repositories must be well formed and
    /// every id unique.
    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        self.check()?;
        std::fs::create_dir_all(root)?;
        let path = root.join(FILE_NAME);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn check(&self) -> anyhow::Result<()> {
        for (i, entry) in self.tokens.iter().enumerate() {
            if entry.id.is_empty() || !entry.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                anyhow::bail!("token {:?} has no usable id", entry.label);
            }
            if self.tokens[..i].iter().any(|e| e.id == entry.id) {
                anyhow::bail!("two tokens are called {:?}", entry.id);
            }
            if entry.repos.is_empty() {
                anyhow::bail!("{:?} names no repositories", entry.label);
            }
            for pattern in &entry.repos {
                check_pattern(pattern).map_err(|e| anyhow::anyhow!("{:?}: {e}", entry.label))?;
            }
        }
        Ok(())
    }

    /// Which secret reads which repositories. Holds no token.
    pub fn routes(&self) -> GithubRoutes {
        GithubRoutes {
            routes: self
                .tokens
                .iter()
                .flat_map(|e| e.repos.iter().map(|p| Route { pattern: p.trim().to_string(), secret: secret_name(&e.id) }))
                .collect(),
        }
    }
}

/// Stores `entry`'s token (replacing any).
pub fn set_token(root: &Path, id: &str, token: &str) -> Result<(), CredentialError> {
    CredentialStore::from_data_root(root).set_named(&account(id), token).map(|_| ())
}

pub fn token(root: &Path, id: &str) -> Option<String> {
    CredentialStore::from_data_root(root).get_named(&account(id))
}

pub fn has_token(root: &Path, id: &str) -> bool {
    CredentialStore::from_data_root(root).has_named(&account(id))
}

pub fn delete_token(root: &Path, id: &str) {
    CredentialStore::from_data_root(root).delete_named(&account(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, repos: &[&str]) -> Entry {
        Entry { id: id.into(), label: id.into(), repos: repos.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn patterns_are_checked() {
        for ok in ["*", "acme/*", "acme/app", " Acme/app.js "] {
            assert!(check_pattern(ok).is_ok(), "{ok}");
        }
        for bad in ["", "acme", "acme/", "/app", "a/b/c", "*/app", "ac me/app"] {
            assert!(check_pattern(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ids_come_from_labels_and_do_not_collide() {
        let taken = vec![entry("acme-org", &["*"])];
        assert_eq!(new_id("Acme Org", &taken), "acme-org-2");
        assert_eq!(new_id("Joel's repos!", &taken), "joel-s-repos");
        assert_eq!(new_id("???", &[]), "token");
    }

    #[test]
    fn a_config_checks_saves_and_makes_routes_without_tokens() {
        let dir = std::env::temp_dir().join(format!("tod-orch-github-{}", uuid::Uuid::new_v4()));
        let config = Config { tokens: vec![entry("acme", &["acme/*", "joel/x"]), entry("joel", &["joel/*"])] };
        config.save(&dir).unwrap();
        assert_eq!(Config::load(&dir).unwrap(), config);
        let routes = config.routes();
        assert_eq!(routes.secret_for("joel", "x"), Some("gh_acme"));
        assert_eq!(routes.secret_for("joel", "y"), Some("gh_joel"));
        assert_eq!(routes.secret_for("acme", "z"), Some("gh_acme"));
        assert!(!std::fs::read_to_string(dir.join(FILE_NAME)).unwrap().contains("ghp_"));
        for bad in [
            Config { tokens: vec![entry("a", &[])] },
            Config { tokens: vec![entry("a", &["x"])] },
            Config { tokens: vec![entry("a", &["*"]), entry("a", &["*"])] },
            Config { tokens: vec![entry("a b", &["*"])] },
        ] {
            assert!(bad.save(&dir).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
