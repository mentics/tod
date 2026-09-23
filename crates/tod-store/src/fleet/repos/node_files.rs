//! Files capability repository — worktree flag, set-up worktree, and dev
//! container per node.
//!
//! The workspace directory and branch live on `node_fields.repo` / `branch`.

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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevContainerSetting {
    /// Container name or id; `None` until one is chosen.
    #[serde(default)]
    pub container: Option<String>,
    /// The repository is on this machine, mounted into the container.
    #[serde(default)]
    pub repo_on_host: bool,
}

impl DevContainerSetting {
    pub fn container(&self) -> Option<&str> {
        self.container.as_deref().map(str::trim).filter(|c| !c.is_empty())
    }

    /// The container the repository lives in, when it lives in one.
    pub fn repo_container(&self) -> Option<&str> {
        if self.repo_on_host {
            return None;
        }
        self.container()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFiles {
    pub node_id: String,
    pub use_worktree: bool,
    pub worktree_path: Option<String>,
    pub worktree_lease_id: Option<String>,
    pub worktree_lease_holder: Option<String>,
    /// Set when the node's launches run in a dev container.
    pub dev_container: Option<DevContainerSetting>,
}

impl NodeFiles {
    /// A worktree has been set up and recorded for this node.
    pub fn worktree_path(&self) -> Option<&str> {
        self.worktree_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
    }
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
                "SELECT node_id, use_worktree, worktree_path, worktree_lease_id, worktree_lease_holder,
                        dev_container, container, container_repo_on_host
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
        let (on, container, on_host) = match dev_container {
            Some(setting) => (
                1,
                setting.container().map(str::to_string),
                i32::from(setting.repo_on_host),
            ),
            None => (0, None, 0),
        };
        self.conn.execute(
            "INSERT INTO node_files
               (node_id, use_worktree, dev_container, container, container_repo_on_host, updated_at)
             VALUES (?1, 0, ?2, ?3, ?4, ?5)
             ON CONFLICT(node_id) DO UPDATE SET
               dev_container = excluded.dev_container,
               container = excluded.container,
               container_repo_on_host = excluded.container_repo_on_host,
               updated_at = excluded.updated_at",
            params![blob, on, container, on_host, now_ms()],
        )?;
        Ok(())
    }

    /// Record (or clear, with `None`s) the node's set-up worktree.
    pub fn update_worktree(
        &self,
        node_id: &str,
        worktree_path: Option<&str>,
        worktree_lease_id: Option<&str>,
        worktree_lease_holder: Option<&str>,
    ) -> Result<()> {
        let blob = node_id_blob(node_id)?;
        self.conn.execute(
            "INSERT INTO node_files
               (node_id, use_worktree, worktree_path, worktree_lease_id, worktree_lease_holder, updated_at)
             VALUES (?1, 1, ?2, ?3, ?4, ?5)
             ON CONFLICT(node_id) DO UPDATE SET
               worktree_path = excluded.worktree_path,
               worktree_lease_id = excluded.worktree_lease_id,
               worktree_lease_holder = excluded.worktree_lease_holder,
               updated_at = excluded.updated_at",
            params![
                blob,
                worktree_path,
                worktree_lease_id,
                worktree_lease_holder,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Nodes with a recorded worktree path.
    pub fn list_with_worktree(&self) -> Result<Vec<NodeFiles>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, use_worktree, worktree_path, worktree_lease_id, worktree_lease_holder,
                    dev_container, container, container_repo_on_host
             FROM node_files WHERE worktree_path IS NOT NULL AND worktree_path != ''",
        )?;
        let rows = stmt
            .query_map([], row_to_files)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Existing worktree path for a node with the same repo + branch.
    pub fn resolve_shared_worktree_path(&self, repo: &str, branch: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT f.worktree_path FROM node_files f
                 INNER JOIN node_fields nf ON nf.node_id = f.node_id
                 WHERE f.use_worktree = 1
                   AND f.worktree_path IS NOT NULL AND f.worktree_path != ''
                   AND nf.repo = ?1
                   AND COALESCE(nf.branch, '') = ?2
                 LIMIT 1",
                params![repo, branch],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Nodes other than `node_id` whose recorded worktree is `path`.
    pub fn other_nodes_using_worktree(&self, node_id: &str, path: &str) -> Result<Vec<String>> {
        let blob = node_id_blob(node_id)?;
        let mut stmt = self
            .conn
            .prepare("SELECT node_id FROM node_files WHERE worktree_path = ?1 AND node_id != ?2")?;
        let rows = stmt
            .query_map(params![path, blob], |row| node_id_column(row, 0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn row_to_files(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeFiles> {
    Ok(NodeFiles {
        node_id: node_id_column(row, 0)?,
        use_worktree: row.get::<_, i64>(1)? != 0,
        worktree_path: row.get(2)?,
        worktree_lease_id: row.get(3)?,
        worktree_lease_holder: row.get(4)?,
        dev_container: if row.get::<_, i64>(5)? != 0 {
            Some(DevContainerSetting {
                container: row.get(6)?,
                repo_on_host: row.get::<_, i64>(7)? != 0,
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
    fn worktree_round_trip_and_sharing() {
        let (dir, conn) = test_writer_conn();
        let a = seed_node(&conn);
        let b = seed_node(&conn);
        let repo = NodeFilesRepo::new(&conn);
        assert!(repo.get(&a).unwrap().is_none());

        repo.set_use_worktree(&a, true).unwrap();
        let files = repo.get(&a).unwrap().unwrap();
        assert!(files.use_worktree);
        assert!(files.worktree_path().is_none());

        repo.update_worktree(&a, Some("/wt/a"), Some("lease"), Some("tod-a"))
            .unwrap();
        repo.update_worktree(&b, Some("/wt/a"), None, None).unwrap();
        let files = repo.get(&a).unwrap().unwrap();
        assert_eq!(files.worktree_path(), Some("/wt/a"));
        assert_eq!(files.worktree_lease_id.as_deref(), Some("lease"));
        assert_eq!(
            repo.other_nodes_using_worktree(&a, "/wt/a").unwrap(),
            vec![b.clone()]
        );

        repo.update_worktree(&b, None, None, None).unwrap();
        assert!(
            repo.other_nodes_using_worktree(&a, "/wt/a")
                .unwrap()
                .is_empty()
        );
        cleanup_test_dir(&dir);
    }

    #[test]
    fn dev_container_round_trip_keeps_the_worktree() {
        let (dir, conn) = test_writer_conn();
        let a = seed_node(&conn);
        let repo = NodeFilesRepo::new(&conn);
        repo.update_worktree(&a, Some("/wt/a"), None, None).unwrap();

        let setting = DevContainerSetting {
            container: Some(" my-dev ".into()),
            repo_on_host: true,
        };
        repo.set_dev_container(&a, Some(&setting)).unwrap();
        let files = repo.get(&a).unwrap().unwrap();
        assert_eq!(files.worktree_path(), Some("/wt/a"));
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
}
