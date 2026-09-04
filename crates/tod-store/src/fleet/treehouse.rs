//! Treehouse CLI integration: isolated config and worktree roots under the data directory.

use crate::fleet::paths::normalize_absolute;
use crate::paths::TodPaths;
use crate::settings::TodSettings;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Replaces `~/.treehouse` as Treehouse's durable home; config lives at `$TREEHOUSE_HOME/config.toml`.
pub const TREEHOUSE_HOME_ENV: &str = "TREEHOUSE_HOME";

/// Parent directory for all worktree pools (`$TREEHOUSE_WORKTREES/{repo}-{hash}/...`).
pub const TREEHOUSE_WORKTREES_ENV: &str = "TREEHOUSE_WORKTREES";

/// Skip background update checks when Tod invokes Treehouse programmatically.
pub const TREEHOUSE_NO_UPDATE_CHECK_ENV: &str = "TREEHOUSE_NO_UPDATE_CHECK";

/// Tod-owned Treehouse home (`TREEHOUSE_HOME`) lives under `{data_root}/treehouse`.
pub fn treehouse_home(paths: &TodPaths) -> PathBuf {
    paths.data_root().join("treehouse")
}

/// User config path when `TREEHOUSE_HOME` is set (`$TREEHOUSE_HOME/config.toml`).
pub fn treehouse_config_path(paths: &TodPaths) -> PathBuf {
    treehouse_home(paths).join("config.toml")
}

/// Resolved `TREEHOUSE_WORKTREES` value from settings. When unset, Treehouse pools under `TREEHOUSE_HOME`.
pub fn resolve_worktrees_parent(settings: &TodSettings) -> Result<Option<PathBuf>> {
    let Some(path) = settings.treehouse_worktrees_root.as_ref() else {
        return Ok(None);
    };
    Ok(Some(normalize_absolute(path.as_path())?))
}

/// Env overrides applied to every Treehouse subprocess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreehouseInvocation {
    pub treehouse_home: PathBuf,
    pub worktrees_parent: Option<PathBuf>,
}

impl TreehouseInvocation {
    pub fn resolve(settings: &TodSettings, paths: &TodPaths) -> Result<Self> {
        let treehouse_home = normalize_absolute(treehouse_home(paths).as_path())?;
        let worktrees_parent = resolve_worktrees_parent(settings)?;
        ensure_user_config(&treehouse_home)?;
        Ok(Self {
            treehouse_home,
            worktrees_parent,
        })
    }

    pub fn apply_to(&self, command: &mut Command) {
        command.env(TREEHOUSE_HOME_ENV, &self.treehouse_home);
        if let Some(worktrees) = &self.worktrees_parent {
            command.env(TREEHOUSE_WORKTREES_ENV, worktrees);
        }
        command.env(TREEHOUSE_NO_UPDATE_CHECK_ENV, "1");
    }
}

/// Sync Treehouse user config on disk after settings change.
pub fn sync_user_config(settings: &TodSettings, paths: &TodPaths) -> Result<()> {
    let _ = TreehouseInvocation::resolve(settings, paths)?;
    Ok(())
}

fn ensure_user_config(treehouse_home: &Path) -> Result<()> {
    std::fs::create_dir_all(treehouse_home)
        .with_context(|| format!("create treehouse home dir {}", treehouse_home.display()))?;
    let config_path = treehouse_home.join("config.toml");
    let contents = "# Managed by tod — do not edit while tod is running.\nmax_trees = 16\n";
    std::fs::write(&config_path, contents)
        .with_context(|| format!("write treehouse config {}", config_path.display()))?;
    Ok(())
}

/// Returns true when the `treehouse` CLI is on PATH and responds.
pub fn treehouse_available() -> bool {
    if Command::new("treehouse")
        .arg("env")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return true;
    }
    Command::new("treehouse")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{clear_data_root_override, set_data_root};
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    fn test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn resolve_worktrees_parent_none_when_unset() {
        let _guard = test_lock().lock().unwrap();
        let sandbox = std::env::temp_dir().join(format!("tod-treehouse-{}", uuid::Uuid::new_v4()));
        set_data_root(sandbox.clone());
        let settings = TodSettings::default();
        assert_eq!(resolve_worktrees_parent(&settings).unwrap(), None);
        clear_data_root_override();
        let _ = fs::remove_dir_all(sandbox);
    }

    #[test]
    fn sync_writes_config_at_treehouse_home() {
        let _guard = test_lock().lock().unwrap();
        let sandbox = std::env::temp_dir().join(format!("tod-treehouse-{}", uuid::Uuid::new_v4()));
        set_data_root(sandbox.clone());
        let paths = TodPaths::discover().unwrap();
        let settings = TodSettings::default();
        sync_user_config(&settings, &paths).unwrap();
        let config_path = treehouse_config_path(&paths);
        assert!(config_path.is_file());
        let contents = fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("max_trees = 16"));
        assert!(!contents.contains("root ="));
        clear_data_root_override();
        let _ = fs::remove_dir_all(sandbox);
    }

    #[test]
    fn invocation_sets_treehouse_home_and_worktrees_env() {
        let _guard = test_lock().lock().unwrap();
        let sandbox = std::env::temp_dir().join(format!("tod-treehouse-{}", uuid::Uuid::new_v4()));
        set_data_root(sandbox.clone());
        let paths = TodPaths::discover().unwrap();
        let custom = sandbox.join("custom-worktrees");
        let settings = TodSettings {
            treehouse_worktrees_root: Some(custom.clone()),
            ..TodSettings::default()
        };
        let invocation = TreehouseInvocation::resolve(&settings, &paths).unwrap();
        let expected_home = normalize_absolute(treehouse_home(&paths).as_path()).unwrap();
        let actual_home = invocation
            .treehouse_home
            .canonicalize()
            .unwrap_or(invocation.treehouse_home);
        let expected_home = expected_home.canonicalize().unwrap_or(expected_home);
        assert_eq!(actual_home, expected_home);
        let worktrees = invocation.worktrees_parent.unwrap();
        assert_eq!(worktrees, custom.canonicalize().unwrap_or(custom));
        clear_data_root_override();
        let _ = fs::remove_dir_all(sandbox);
    }
}
