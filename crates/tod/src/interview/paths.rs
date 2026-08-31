use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

static DATA_ROOT_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Env var for the persistent data root (see README “Data locations”).
pub const TOD_DATA_ROOT_ENV: &str = "TOD_DATA_ROOT";

/// Recommended repo-relative path for dogfooding durable state.
pub const DOGFOOD_DATA_ROOT: &str = ".local/data";

/// Recommended repo-relative prefix for isolated test sandboxes.
pub const TEST_DATA_ROOT: &str = ".local/test";

/// Resolve data root from CLI `--data-root` or `{TOD_DATA_ROOT}`.
pub fn resolve_data_root(cli: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = cli {
        return Some(root.to_path_buf());
    }
    std::env::var(TOD_DATA_ROOT_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
}

/// Resolve startup data root: CLI, env, then `install.toml` bootstrap.
pub fn resolve_startup_data_root(cli: Option<&Path>) -> Option<PathBuf> {
    resolve_data_root(cli).or_else(crate::install::load_data_root)
}

/// Pin all durable tod paths under `root`. Call once at process start before `TodPaths::discover()`.
pub fn set_data_root(root: PathBuf) {
    let mut guard = DATA_ROOT_OVERRIDE.write().expect("data root override lock");
    *guard = Some(root);
}

pub fn is_data_root_configured() -> bool {
    DATA_ROOT_OVERRIDE
        .read()
        .expect("data root override lock")
        .is_some()
}

#[cfg(test)]
pub fn clear_data_root_override() {
    *DATA_ROOT_OVERRIDE.write().expect("data root override lock") = None;
}

/// Resolved paths for durable tod data and git checkout discovery.
#[derive(Debug, Clone)]
pub struct TodPaths {
    /// Git checkout root (`assets/process/`, source tree). Independent of data root.
    git_repo_root: PathBuf,
    /// Durable state root (`tod.db`, `tod.yml`, …).
    data_root: PathBuf,
    config_dir: PathBuf,
}

impl TodPaths {
    /// Resolve git checkout and configured data root.
    pub fn discover() -> Result<Self> {
        let start = std::env::current_dir().context("failed to read current directory")?;
        let git_repo_root = find_repo_root(&start).unwrap_or(start);
        let data_root = DATA_ROOT_OVERRIDE
            .read()
            .expect("data root override lock")
            .clone()
            .context("data root is not configured")?;
        Ok(Self::new(git_repo_root, data_root))
    }

    pub fn from_repo_root(git_repo_root: PathBuf) -> Result<Self> {
        let data_root = DATA_ROOT_OVERRIDE
            .read()
            .expect("data root override lock")
            .clone()
            .context("data root is not configured")?;
        Ok(Self::new(git_repo_root, data_root))
    }

    fn new(git_repo_root: PathBuf, data_root: PathBuf) -> Self {
        let config_dir = data_root.clone();
        Self {
            git_repo_root,
            data_root,
            config_dir,
        }
    }

    /// Git checkout root (for `assets/process/` and other repo files).
    pub fn repo_root(&self) -> &Path {
        &self.git_repo_root
    }

    /// Durable state root (`tod.db`, logs, scratchpads).
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn local_home(&self) -> PathBuf {
        self.data_root.clone()
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Legacy per-repo interview SQLite (migrated into fleet on open).
    pub fn sqlite_path(&self) -> PathBuf {
        self.data_root
            .join(".local")
            .join(".config")
            .join("tod")
            .join("tod.db")
    }

    pub fn settings_path(&self) -> PathBuf {
        self.config_dir.join("tod.yml")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.data_root.join("log")
    }

    /// Fleet persistence root (same as data root).
    pub fn fleet_storage_root(&self) -> PathBuf {
        self.data_root.clone()
    }

    pub fn ensure_config_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("failed to create config dir {}", self.config_dir.display()))
    }

    pub fn ensure_log_dir(&self) -> Result<()> {
        let dir = self.log_dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create log dir {}", dir.display()))
    }
}

fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(".git").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_flat_under_data_root() {
        let root = std::env::temp_dir().join(format!("tod-paths-{}", uuid::Uuid::new_v4()));
        set_data_root(root.clone());
        let paths = TodPaths::discover().unwrap();
        assert_eq!(paths.data_root(), root.as_path());
        assert_eq!(paths.config_dir(), root.as_path());
        assert_eq!(paths.settings_path(), root.join("tod.yml"));
        assert_eq!(paths.log_dir(), root.join("log"));
        assert_eq!(paths.fleet_storage_root(), root);
        clear_data_root_override();
    }

    #[test]
    fn discover_honors_data_root_override() {
        let root = std::env::temp_dir().join(format!("tod-paths-{}", uuid::Uuid::new_v4()));
        set_data_root(root.clone());
        let paths = TodPaths::discover().unwrap();
        assert_eq!(paths.data_root(), root.as_path());
        clear_data_root_override();
    }

    #[test]
    fn repo_root_stays_checkout_when_data_root_overridden() {
        let root = std::env::temp_dir().join(format!("tod-paths-{}", uuid::Uuid::new_v4()));
        set_data_root(root);
        let paths = TodPaths::discover().unwrap();
        assert!(
            paths.repo_root().join("doc").join("process").is_dir()
                || paths.repo_root().join("assets").join("process").is_dir(),
            "repo_root should remain the git checkout, not data root"
        );
        clear_data_root_override();
    }

    #[test]
    fn discover_fails_without_data_root() {
        clear_data_root_override();
        assert!(TodPaths::discover().is_err());
    }

    #[test]
    fn resolve_data_root_prefers_cli() {
        let cli = PathBuf::from("/cli/root");
        let prev = std::env::var(TOD_DATA_ROOT_ENV).ok();
        unsafe {
            std::env::set_var(TOD_DATA_ROOT_ENV, "/env/root");
        }
        let resolved = resolve_data_root(Some(cli.as_path()));
        unsafe {
            if let Some(v) = prev {
                std::env::set_var(TOD_DATA_ROOT_ENV, v);
            } else {
                std::env::remove_var(TOD_DATA_ROOT_ENV);
            }
        }
        assert_eq!(resolved, Some(cli));
    }

    #[test]
    fn resolve_data_root_uses_env_when_cli_absent() {
        let prev = std::env::var(TOD_DATA_ROOT_ENV).ok();
        unsafe {
            std::env::set_var(TOD_DATA_ROOT_ENV, ".local/data");
        }
        let resolved = resolve_data_root(None);
        unsafe {
            if let Some(v) = prev {
                std::env::set_var(TOD_DATA_ROOT_ENV, v);
            } else {
                std::env::remove_var(TOD_DATA_ROOT_ENV);
            }
        }
        assert_eq!(resolved, Some(PathBuf::from(".local/data")));
    }

    #[test]
    fn resolve_data_root_none_without_cli_or_env() {
        let prev = std::env::var(TOD_DATA_ROOT_ENV).ok();
        unsafe {
            std::env::remove_var(TOD_DATA_ROOT_ENV);
        }
        assert_eq!(resolve_data_root(None), None);
        unsafe {
            if let Some(v) = prev {
                std::env::set_var(TOD_DATA_ROOT_ENV, v);
            }
        }
    }
}
