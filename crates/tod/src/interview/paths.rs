use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Repo-local XDG-style paths for durable tod data.
#[derive(Debug, Clone)]
pub struct TodPaths {
    repo_root: PathBuf,
    config_dir: PathBuf,
}

impl TodPaths {
    /// Resolve repo root by walking up from the current directory.
    /// A directory qualifies if it contains `.local` or `.git`.
    pub fn discover() -> Result<Self> {
        let start = std::env::current_dir().context("failed to read current directory")?;
        let repo_root = find_repo_root(&start).unwrap_or(start);
        Ok(Self::from_repo_root(repo_root))
    }

    pub fn from_repo_root(repo_root: PathBuf) -> Self {
        let config_dir = repo_root.join(".local").join(".config").join("tod");
        Self {
            repo_root,
            config_dir,
        }
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn local_home(&self) -> PathBuf {
        self.repo_root.join(".local")
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn sqlite_path(&self) -> PathBuf {
        self.config_dir.join("tod.db")
    }

    pub fn settings_path(&self) -> PathBuf {
        self.config_dir.join("tod.yml")
    }

    /// Ensure `.local/.config/tod/` exists.
    pub fn ensure_config_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("failed to create config dir {}", self.config_dir.display()))
    }
}

fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(".local").is_dir() || dir.join(".git").is_dir() {
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
    fn config_paths_under_local_config_tod() {
        let paths = TodPaths::from_repo_root(PathBuf::from("/repo"));
        assert_eq!(paths.config_dir(), Path::new("/repo/.local/.config/tod"));
        assert_eq!(paths.sqlite_path(), Path::new("/repo/.local/.config/tod/tod.db"));
        assert_eq!(paths.settings_path(), Path::new("/repo/.local/.config/tod/tod.yml"));
    }
}
