use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Normalized absolute paths for fleet persistence files under a storage root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetPaths {
    root: PathBuf,
    db: PathBuf,
    lock: PathBuf,
    migration_intent: PathBuf,
    migrating: PathBuf,
    stale_copy: PathBuf,
    pre_upgrade_bak: PathBuf,
    held_writes: PathBuf,
}

impl FleetPaths {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = normalize_absolute(root.as_ref())?;
        Ok(Self {
            db: root.join("tod.db"),
            lock: root.join("tod.lock"),
            migration_intent: root.join("tod.migration-intent"),
            migrating: root.join("tod.migrating"),
            stale_copy: root.join("tod.stale-copy"),
            pre_upgrade_bak: root.join("tod.pre-upgrade.bak"),
            held_writes: root.join("tod.held-writes"),
            root,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn db(&self) -> &Path {
        &self.db
    }

    pub fn lock(&self) -> &Path {
        &self.lock
    }

    pub fn migration_intent(&self) -> &Path {
        &self.migration_intent
    }

    pub fn migrating(&self) -> &Path {
        &self.migrating
    }

    pub fn stale_copy(&self) -> &Path {
        &self.stale_copy
    }

    pub fn pre_upgrade_bak(&self) -> &Path {
        &self.pre_upgrade_bak
    }

    pub fn held_writes(&self) -> &Path {
        &self.held_writes
    }

    /// Staging path for atomic pre-upgrade backup creation.
    pub fn pre_upgrade_bak_tmp(&self) -> PathBuf {
        self.root.join("tod.pre-upgrade.bak.tmp")
    }

    /// Returns true when `tod.db` exists under this storage root.
    pub fn has_store(&self) -> bool {
        self.db.exists()
    }

    pub fn ensure_root(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root).with_context(|| {
            format!(
                "failed to create fleet storage root {}",
                self.root.display()
            )
        })?;
        std::fs::create_dir_all(self.media())
            .with_context(|| format!("failed to create media dir {}", self.media().display()))?;
        Ok(())
    }

    /// Immutable media artifacts directory (`{root}/media/`).
    pub fn media(&self) -> PathBuf {
        self.root.join("media")
    }
}

/// Resolve to an absolute path, canonicalizing when the path already exists.
pub fn normalize_absolute(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)
        .with_context(|| format!("failed to resolve absolute path for {}", path.display()))?;
    if absolute.exists() {
        absolute
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", absolute.display()))
    } else {
        Ok(absolute)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn path_shapes_under_root() {
        let root = std::env::temp_dir().join(format!("tod-fleet-paths-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let paths = FleetPaths::new(&root).unwrap();

        assert!(paths.root().is_absolute());
        assert_eq!(paths.db(), paths.root().join("tod.db"));
        assert_eq!(paths.lock(), paths.root().join("tod.lock"));
        assert_eq!(
            paths.migration_intent(),
            paths.root().join("tod.migration-intent")
        );
        assert_eq!(paths.migrating(), paths.root().join("tod.migrating"));
        assert_eq!(paths.stale_copy(), paths.root().join("tod.stale-copy"));
        assert_eq!(
            paths.pre_upgrade_bak(),
            paths.root().join("tod.pre-upgrade.bak")
        );
        assert_eq!(paths.held_writes(), paths.root().join("tod.held-writes"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn relative_root_becomes_absolute() {
        let root = std::env::temp_dir().join(format!("tod-fleet-rel-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let rel = Path::new(".").join(root.file_name().unwrap());
        // Use parent + relative segment when cwd is temp parent.
        let parent = root.parent().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(parent).unwrap();
        let paths = FleetPaths::new(&rel).unwrap();
        assert!(paths.root().is_absolute());
        std::env::set_current_dir(original).unwrap();
        let _ = fs::remove_dir_all(root);
    }
}
