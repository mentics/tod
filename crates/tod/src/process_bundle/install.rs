//! Resolve install-bundled process documentation paths.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Paths to versioned process/agent documentation shipped with the app.
#[derive(Debug, Clone)]
pub struct TodInstallPaths {
    process_root: PathBuf,
}

impl TodInstallPaths {
    /// Discover bundled `process/` directory.
    ///
    /// Resolution order:
    /// 1. `{TOD_PROCESS_ROOT}` env override
    /// 2. `{executable_dir}/process` (installed layout)
    /// 3. Walk up from cwd for `assets/process/README.md` (dev repo checkout)
    pub fn discover() -> Result<Self> {
        if let Ok(raw) = std::env::var("TOD_PROCESS_ROOT") {
            let root = PathBuf::from(raw);
            return Self::from_process_root(root);
        }

        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let candidate = dir.join("process");
                if candidate.join("README.md").is_file() {
                    return Self::from_process_root(candidate);
                }
            }
        }

        let start = std::env::current_dir().context("failed to read current directory")?;
        if let Some(root) = find_dev_process_root(&start) {
            return Self::from_process_root(root);
        }

        anyhow::bail!(
            "bundled process documentation not found; set TOD_PROCESS_ROOT or install process/ next to the binary"
        )
    }

    pub fn from_process_root(process_root: PathBuf) -> Result<Self> {
        if !process_root.join("README.md").is_file() {
            anyhow::bail!(
                "invalid process root {} (missing README.md)",
                process_root.display()
            );
        }
        Ok(Self { process_root })
    }

    pub fn process_root(&self) -> &Path {
        &self.process_root
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn resolve(&self, rel: &str) -> PathBuf {
        self.process_root.join(rel)
    }
}

/// Dev checkout: `assets/process/` in the repo tree.
fn find_dev_process_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("assets").join("process");
        if candidate.join("README.md").is_file() {
            return Some(candidate);
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
    fn discovers_repo_assets_process_root() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("assets")
            .join("process");
        if root.join("README.md").is_file() {
            let paths = TodInstallPaths::from_process_root(root).unwrap();
            assert!(paths
                .resolve("agents/interview/base.md")
                .is_file());
        }
    }
}
