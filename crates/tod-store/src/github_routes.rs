//! Which of the orchestrator's GitHub tokens reads which repository.
//!
//! The orchestrator reads GitHub through its sandbox's proxy, which holds
//! each token under a name (`tod_sandbox::node::keyed_secrets_spec`) and
//! swaps a placeholder in the request's own `Authorization` header for it
//! (`secret_placeholder`; Blaxel's proxy matches only on host, so the client
//! has to choose). This file is the choice: patterns of repositories, each
//! with the name of the secret to use. It holds no token. It is kept in the
//! user's data directory on the orchestrator (`github-routes.json`), put
//! there by the app, and read by the process that has `TOD_GITHUB_AUTH=routed`.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const FILE_NAME: &str = "github-routes.json";

/// `pattern` is `owner/repo`, `owner/*` or `*`; case does not matter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub pattern: String,
    /// The proxy secret's name.
    pub secret: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubRoutes {
    pub routes: Vec<Route>,
}

/// How specifically `pattern` names the repository, or `None` if it does not.
fn specificity(pattern: &str, owner: &str, repo: &str) -> Option<u8> {
    let pattern = pattern.trim();
    if pattern == "*" {
        return Some(0);
    }
    let (o, r) = pattern.split_once('/')?;
    if !o.eq_ignore_ascii_case(owner) {
        return None;
    }
    if r == "*" {
        Some(1)
    } else if r.eq_ignore_ascii_case(repo) {
        Some(2)
    } else {
        None
    }
}

impl GithubRoutes {
    /// The secret to read `owner/repo` with: the most specific pattern
    /// that names it, the earlier one of equals.
    pub fn secret_for(&self, owner: &str, repo: &str) -> Option<&str> {
        self.routes
            .iter()
            .filter_map(|route| specificity(&route.pattern, owner, repo).map(|s| (s, route)))
            .rev()
            .max_by_key(|(s, _)| *s)
            .map(|(_, route)| route.secret.as_str())
    }

    /// `<dir>/github-routes.json`; none yet is no routes.
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join(FILE_NAME);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| anyhow::anyhow!("{} is not readable: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow::anyhow!("read {}: {e}", path.display())),
        }
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(FILE_NAME);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

/// `owner` and `repo` of a REST path (`/repos/<owner>/<repo>/...`).
pub fn repo_of_path(path: &str) -> Option<(&str, &str)> {
    let mut parts = path.strip_prefix("/repos/")?.split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().map(|r| r.split('?').next().unwrap_or(r)).filter(|s| !s.is_empty())?;
    Some((owner, repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routes(list: &[(&str, &str)]) -> GithubRoutes {
        GithubRoutes { routes: list.iter().map(|(p, s)| Route { pattern: p.to_string(), secret: s.to_string() }).collect() }
    }

    #[test]
    fn the_most_specific_pattern_wins_and_case_does_not_matter() {
        let r = routes(&[("*", "any"), ("Acme/*", "acme"), ("acme/app", "app")]);
        assert_eq!(r.secret_for("ACME", "App"), Some("app"));
        assert_eq!(r.secret_for("acme", "other"), Some("acme"));
        assert_eq!(r.secret_for("joel-cofactr", "product-ingest"), Some("any"));
        assert_eq!(routes(&[("acme/*", "acme")]).secret_for("other", "x"), None);
        assert_eq!(routes(&[("acme/app", "a"), ("acme/app", "b")]).secret_for("acme", "app"), Some("a"));
    }

    #[test]
    fn a_repository_is_read_from_a_rest_path() {
        assert_eq!(repo_of_path("/repos/acme/app/pulls/3"), Some(("acme", "app")));
        assert_eq!(repo_of_path("/repos/acme/app?x=1"), Some(("acme", "app")));
        assert_eq!(repo_of_path("/user"), None);
        assert_eq!(repo_of_path("/repos/acme"), None);
    }

    #[test]
    fn routes_round_trip_through_their_file() {
        let dir = std::env::temp_dir().join(format!("tod-routes-{}", uuid::Uuid::new_v4()));
        assert_eq!(GithubRoutes::load(&dir).unwrap(), GithubRoutes::default());
        let r = routes(&[("acme/*", "gh_acme")]);
        r.save(&dir).unwrap();
        assert_eq!(GithubRoutes::load(&dir).unwrap(), r);
        std::fs::write(dir.join(FILE_NAME), b"nope").unwrap();
        assert!(GithubRoutes::load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
