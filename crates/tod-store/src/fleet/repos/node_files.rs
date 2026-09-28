//! Files capability repository: the settings on the node that has Files —
//! the worktree flag and where launches run.
//!
//! The workspace directory and branch live on `node_fields.repo` / `branch`.
//! These settings are a recipe: the directory each node works in (its own
//! worktree or sandbox) is made from them when the node first needs its
//! files, and kept in [`crate::fleet::repos::files_location`]. The
//! `worktree_*` columns here predate that and are no longer read (schema v75
//! moved them there).

use crate::fleet::repos::{node_id_blob, node_id_column};
use crate::outline::uuid_blob::now_ms;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// A node's launches run inside a running dev container rather than on this
/// machine.
///
/// Usually the repository lives in the container: the workspace directory
/// and worktree are container paths, and git runs there. With
/// `repo_on_host`, the repository is on this machine and mounted into the
/// container: the workspace directory stays a host path, git runs here, and
/// only the launches go into the container.
///
/// With `sandbox`, every node works in a cloud sandbox of its own (see
/// [`crate::fleet::sandbox`]), made from `sandbox_from` when the node first
/// needs its files; `container` is unused. The repository always lives in
/// the sandbox: the image or the forked sandbox must already hold it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevContainerSetting {
    /// Container name or id; `None` until one is chosen.
    #[serde(default)]
    pub container: Option<String>,
    /// The repository is on this machine, mounted into the container.
    #[serde(default)]
    pub repo_on_host: bool,
    /// Each node works in a cloud sandbox of its own, not a Docker container.
    #[serde(default)]
    pub sandbox: bool,
    /// What each node's sandbox is made from.
    #[serde(default)]
    pub sandbox_from: SandboxFrom,
}

/// What a node's new sandbox starts from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxFrom {
    /// An image; empty is the account's default (Settings → Cloud sandboxes).
    Image(String),
    /// A copy of this sandbox's current state.
    Fork(String),
}

impl Default for SandboxFrom {
    fn default() -> Self {
        Self::Image(String::new())
    }
}

impl SandboxFrom {
    /// `(sandbox_source, sandbox_from)` as stored.
    pub(crate) fn columns(&self) -> (&'static str, &str) {
        match self {
            Self::Image(image) => ("image", image.trim()),
            Self::Fork(name) => ("fork", name.trim()),
        }
    }

    pub(crate) fn from_columns(source: &str, from: String) -> Self {
        if source == "fork" { Self::Fork(from) } else { Self::Image(from) }
    }

    /// Why a sandbox can't be made from this yet (user-facing).
    pub fn incomplete(&self) -> Option<&'static str> {
        matches!(self, Self::Fork(name) if name.trim().is_empty())
            .then_some("Choose the sandbox to fork")
    }

    pub fn source(&self) -> crate::fleet::sandbox::NewSandboxSource {
        use crate::fleet::sandbox::NewSandboxSource;
        match self {
            Self::Image(image) => NewSandboxSource::Image(image.trim().into()),
            Self::Fork(name) => NewSandboxSource::Fork(name.trim().into()),
        }
    }

    /// For the user: "the default image", "image X", "a fork of Y".
    pub fn describe(&self) -> String {
        match self {
            Self::Image(image) if image.trim().is_empty() => "the default image".into(),
            Self::Image(image) => format!("image {}", image.trim()),
            Self::Fork(name) if name.trim().is_empty() => "a fork (none chosen)".into(),
            Self::Fork(name) => format!("a fork of {}", name.trim()),
        }
    }
}

impl DevContainerSetting {
    pub fn container(&self) -> Option<&str> {
        self.container.as_deref().map(str::trim).filter(|c| !c.is_empty())
    }

    /// The Docker container the repository lives in, when it lives in one.
    pub fn repo_container(&self) -> Option<&str> {
        if self.repo_on_host || self.sandbox {
            return None;
        }
        self.container()
    }

    /// The Docker container a repository on this machine is mounted into.
    pub fn mounted_container(&self) -> Option<&str> {
        if !self.repo_on_host || self.sandbox {
            return None;
        }
        self.container()
    }

    /// The repository is not on this machine (a container or a sandbox holds it).
    pub fn repo_is_remote(&self) -> bool {
        self.sandbox || self.repo_container().is_some()
    }

    /// `container_kind` as stored.
    pub fn kind(&self) -> &'static str {
        if self.sandbox { "sandbox" } else { "docker" }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFiles {
    pub node_id: String,
    /// Each node works in a worktree of its own.
    pub use_worktree: bool,
    /// Set when the node's launches run in a dev container or sandboxes.
    pub dev_container: Option<DevContainerSetting>,
}

pub struct NodeFilesRepo<'a> {
    conn: &'a Connection,
}

impl<'a> NodeFilesRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn get(&self, node_id: &str) -> Result<Option<NodeFiles>> {
        let blob = node_id_blob(node_id)?;
        self.conn
            .query_row(
                "SELECT node_id, use_worktree, dev_container, container, container_repo_on_host,
                        container_kind, sandbox_source, sandbox_from
                 FROM node_files WHERE node_id = ?1",
                params![blob],
                row_to_files,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_use_worktree(&self, node_id: &str, use_worktree: bool) -> Result<()> {
        let blob = node_id_blob(node_id)?;
        self.conn.execute(
            "INSERT INTO node_files (node_id, use_worktree, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET
               use_worktree = excluded.use_worktree, updated_at = excluded.updated_at",
            params![blob, i32::from(use_worktree), now_ms()],
        )?;
        Ok(())
    }

    /// Run the node's launches in a dev container (`Some`), or on this machine.
    pub fn set_dev_container(
        &self,
        node_id: &str,
        dev_container: Option<&DevContainerSetting>,
    ) -> Result<()> {
        let blob = node_id_blob(node_id)?;
        let default_from = SandboxFrom::default();
        let (on, container, on_host, kind, from) = match dev_container {
            Some(setting) => (
                1,
                setting.container().filter(|_| !setting.sandbox).map(str::to_string),
                i32::from(setting.repo_on_host && !setting.sandbox),
                setting.kind(),
                &setting.sandbox_from,
            ),
            None => (0, None, 0, "docker", &default_from),
        };
        let (source, from) = from.columns();
        self.conn.execute(
            "INSERT INTO node_files
               (node_id, use_worktree, dev_container, container, container_repo_on_host,
                container_kind, sandbox_source, sandbox_from, updated_at)
             VALUES (?1, 0, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(node_id) DO UPDATE SET
               dev_container = excluded.dev_container,
               container = excluded.container,
               container_repo_on_host = excluded.container_repo_on_host,
               container_kind = excluded.container_kind,
               sandbox_source = excluded.sandbox_source,
               sandbox_from = excluded.sandbox_from,
               updated_at = excluded.updated_at",
            params![blob, on, container, on_host, kind, source, from, now_ms()],
        )?;
        Ok(())
    }
}

fn row_to_files(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeFiles> {
    Ok(NodeFiles {
        node_id: node_id_column(row, 0)?,
        use_worktree: row.get::<_, i64>(1)? != 0,
        dev_container: if row.get::<_, i64>(2)? != 0 {
            Some(DevContainerSetting {
                container: row.get(3)?,
                repo_on_host: row.get::<_, i64>(4)? != 0,
                sandbox: row.get::<_, String>(5)? == "sandbox",
                sandbox_from: SandboxFrom::from_columns(&row.get::<_, String>(6)?, row.get(7)?),
            })
        } else {
            None
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::{cleanup_test_dir, seed_node, test_writer_conn};

    #[test]
    fn worktree_flag_round_trip() {
        let (dir, conn) = test_writer_conn();
        let a = seed_node(&conn);
        let repo = NodeFilesRepo::new(&conn);
        assert!(repo.get(&a).unwrap().is_none());
        repo.set_use_worktree(&a, true).unwrap();
        assert!(repo.get(&a).unwrap().unwrap().use_worktree);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn dev_container_round_trip() {
        let (dir, conn) = test_writer_conn();
        let a = seed_node(&conn);
        let repo = NodeFilesRepo::new(&conn);
        repo.set_use_worktree(&a, true).unwrap();

        let setting = DevContainerSetting {
            container: Some(" my-dev ".into()),
            repo_on_host: true,
            ..Default::default()
        };
        repo.set_dev_container(&a, Some(&setting)).unwrap();
        let files = repo.get(&a).unwrap().unwrap();
        assert!(files.use_worktree, "the worktree flag is kept");
        let dev = files.dev_container.expect("dev container");
        assert_eq!(dev.container.as_deref(), Some("my-dev"));
        assert!(dev.repo_on_host);
        assert_eq!(dev.repo_container(), None);

        // Chosen, but no container picked yet.
        repo.set_dev_container(&a, Some(&DevContainerSetting::default()))
            .unwrap();
        let dev = repo.get(&a).unwrap().unwrap().dev_container.unwrap();
        assert_eq!(dev.container(), None);
        assert!(!dev.repo_on_host, "the repository lives in the container by default");

        repo.set_dev_container(&a, None).unwrap();
        assert_eq!(repo.get(&a).unwrap().unwrap().dev_container, None);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn sandbox_recipe_round_trip() {
        let (dir, conn) = test_writer_conn();
        let a = seed_node(&conn);
        let repo = NodeFilesRepo::new(&conn);
        let setting = DevContainerSetting {
            sandbox: true,
            sandbox_from: SandboxFrom::Fork("template".into()),
            ..Default::default()
        };
        repo.set_dev_container(&a, Some(&setting)).unwrap();
        let dev = repo.get(&a).unwrap().unwrap().dev_container.unwrap();
        assert_eq!(dev, setting);
        assert!(dev.repo_is_remote());
        cleanup_test_dir(&dir);
    }
}
