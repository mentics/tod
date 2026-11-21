//! Bundled state agent doc paths.

use super::install::TodInstallPaths;
use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ProcessManifest {
    process_root: PathBuf,
}

impl ProcessManifest {
    pub fn load(install: &TodInstallPaths) -> Result<Self> {
        let root = install.process_root();
        let base = root.join("agents/state/base.md");
        if !base.is_file() {
            anyhow::bail!("missing bundled process doc {}", base.display());
        }
        Ok(Self { process_root: root.to_path_buf() })
    }

    /// Lifecycle state agent doc when present (`agents/state/{lifecycle}.md`).
    pub fn state_doc(&self, lifecycle: &str) -> Option<PathBuf> {
        let path = self.resolve(&format!("agents/state/{lifecycle}.md"));
        if path.is_file() { Some(path) } else { None }
    }

    /// Shared state-agent conventions.
    pub fn state_base_doc(&self) -> PathBuf {
        self.resolve("agents/state/base.md")
    }

    fn resolve(&self, rel: &str) -> PathBuf {
        self.process_root.join(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_bundled_defaults() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("assets")
            .join("process");
        if !root.join("README.md").is_file() {
            return;
        }
        let install = TodInstallPaths::from_process_root(root).unwrap();
        let manifest = ProcessManifest::load(&install).unwrap();
        assert!(manifest.state_base_doc().is_file());
    }
}
