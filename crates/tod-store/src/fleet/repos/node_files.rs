//! Files capability repository: the location a node with Files picked.
//!
//! What a location holds (workspace directory, worktrees, where launches
//! run) is in [`crate::fleet::repos::files_place`]; the directory each node
//! works in (its own worktree or sandbox) is made from it when the node
//! first needs its files, and kept in [`crate::fleet::repos::files_location`].
//! The branch lives on `node_fields.branch`. The other columns of
//! `node_files` predate locations (schema v82 moved them to
//! `files_places`) and are no longer read.

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

/// A node's Files capability: the location it picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFiles {
    pub node_id: String,
    /// The location (`files_places.id`) this node's Files uses, if one is
    /// picked. The node's branch is its own, on `node_fields`.
    pub place_id: Option<String>,
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
                "SELECT node_id, place_id FROM node_files WHERE node_id = ?1",
                params![blob],
                |row| Ok(NodeFiles { node_id: node_id_column(row, 0)?, place_id: row.get(1)? }),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Pick the location (`Some`) or none (`None`); the location must exist.
    pub fn set_place(&self, node_id: &str, place_id: Option<&str>) -> Result<()> {
        let blob = node_id_blob(node_id)?;
        if let Some(id) = place_id
            && crate::fleet::repos::files_place::FilesPlaceRepo::new(self.conn).get(id)?.is_none()
        {
            anyhow::bail!("Location {id} no longer exists");
        }
        self.conn.execute(
            "INSERT INTO node_files (node_id, use_worktree, place_id, updated_at)
             VALUES (?1, 0, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET
               place_id = excluded.place_id, updated_at = excluded.updated_at",
            params![blob, place_id, now_ms()],
        )?;
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::files_place::{FilesPlace, FilesPlaceRepo};
    use crate::fleet::repos::{cleanup_test_dir, seed_node, test_writer_conn};

    #[test]
    fn picking_a_location_round_trips() {
        let (dir, conn) = test_writer_conn();
        let a = seed_node(&conn);
        let repo = NodeFilesRepo::new(&conn);
        assert!(repo.get(&a).unwrap().is_none());
        let place = FilesPlaceRepo::new(&conn)
            .save(&FilesPlace { name: "App".into(), ..Default::default() })
            .unwrap();
        repo.set_place(&a, Some(&place.id)).unwrap();
        assert_eq!(repo.get(&a).unwrap().unwrap().place_id.as_deref(), Some(place.id.as_str()));
        assert!(repo.set_place(&a, Some("gone")).is_err(), "an unknown location is refused");
        repo.set_place(&a, None).unwrap();
        assert_eq!(repo.get(&a).unwrap().unwrap().place_id, None);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn deleting_a_location_unpicks_it() {
        let (dir, conn) = test_writer_conn();
        let a = seed_node(&conn);
        let places = FilesPlaceRepo::new(&conn);
        let place = places.save(&FilesPlace { name: "App".into(), ..Default::default() }).unwrap();
        NodeFilesRepo::new(&conn).set_place(&a, Some(&place.id)).unwrap();
        assert_eq!(places.nodes_using(&place.id).unwrap(), vec![a.clone()]);
        places.delete(&place.id).unwrap();
        assert!(places.get(&place.id).unwrap().is_none());
        assert_eq!(NodeFilesRepo::new(&conn).get(&a).unwrap().unwrap().place_id, None);
        cleanup_test_dir(&dir);
    }
}
