//! Resolve a node's Files and Agent capability values.
//!
//! Both inherit from the nearest ancestor (or the node itself) that has the
//! capability enabled; nodes between them without the capability are skipped.

use crate::agent_launch::AgentLaunchOptions;
use crate::fleet::repos::node_agent::{NodeAgent, NodeAgentRepo};
use crate::fleet::repos::node_files::{DevContainerSetting, NodeFilesRepo};
use crate::fleet::workdir::Workdir;
use crate::outline::repos::NodeRepo;
use crate::outline::types::Capability;
use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use crate::settings::{AgentRole, TodSettings};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Files capability values for a node, possibly inherited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFiles {
    /// Node that owns the Files capability (the node itself unless inherited).
    pub source_node_id: String,
    pub source_title: String,
    pub inherited: bool,
    /// Workspace directory (`node_fields.repo`).
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub use_worktree: bool,
    pub worktree_path: Option<String>,
    pub worktree_lease_id: Option<String>,
    pub worktree_lease_holder: Option<String>,
    /// Set when launches run in a dev container. When the repository lives
    /// in it, `repo` and `worktree_path` are container paths; when it is
    /// mounted from this machine they stay host paths, and the container's
    /// own is resolved against the running container at launch (see
    /// [`crate::fleet::dev_container`]).
    pub dev_container: Option<DevContainerSetting>,
}

/// Where launches from a node run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesDirectory {
    Ready(Workdir),
    /// Worktree flag on, but no worktree has been set up (or it's gone).
    NeedsWorktreeSetup,
    /// No usable directory; the reason is user-facing.
    Missing(String),
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

impl ResolvedFiles {
    pub fn repo(&self) -> Option<&str> {
        non_empty(&self.repo)
    }

    pub fn branch(&self) -> Option<&str> {
        non_empty(&self.branch)
    }

    /// A worktree has been set up and recorded.
    pub fn worktree_path(&self) -> Option<&str> {
        non_empty(&self.worktree_path)
    }

    /// The dev container the repository lives in, when it lives in one.
    pub fn repo_container(&self) -> Option<&str> {
        self.dev_container
            .as_ref()
            .and_then(DevContainerSetting::repo_container)
    }

    /// `path` (the workspace directory or worktree) where it is: in the
    /// repository's container, else on this machine.
    pub fn workdir(&self, path: &str) -> Workdir {
        match self.repo_container() {
            Some(container) => Workdir::container(container, path),
            None => Workdir::host(path),
        }
    }

    /// The workspace directory, where it is.
    pub fn repo_dir(&self) -> Option<Workdir> {
        self.repo().map(|repo| self.workdir(repo))
    }

    /// The set-up worktree, where it is.
    pub fn worktree_dir(&self) -> Option<Workdir> {
        self.worktree_path().map(|path| self.workdir(path))
    }

    /// The resolved directory: the worktree when enabled, else the workspace
    /// directory. With the repository mounted into a dev container, the host
    /// side of it. One inside a container is not checked here (that takes
    /// Docker); a launch into a missing one fails.
    pub fn directory(&self) -> FilesDirectory {
        if self
            .dev_container
            .as_ref()
            .is_some_and(|dev| dev.container().is_none())
        {
            return FilesDirectory::Missing("Choose a dev container".into());
        }
        let Some(repo) = self.repo() else {
            return FilesDirectory::Missing("Set a workspace directory".into());
        };
        if let Some(container) = self.repo_container() {
            if !repo.starts_with('/') {
                return FilesDirectory::Missing(format!(
                    "The workspace directory is inside dev container {container}: \
                     give its path there, like /workspaces/app"
                ));
            }
            if self.use_worktree {
                return match self.worktree_dir() {
                    Some(dir) => FilesDirectory::Ready(dir),
                    None => FilesDirectory::NeedsWorktreeSetup,
                };
            }
            return FilesDirectory::Ready(self.workdir(repo));
        }
        if self.use_worktree {
            return match self.worktree_path() {
                Some(path) if Path::new(path).is_dir() => {
                    FilesDirectory::Ready(Workdir::host(path))
                }
                _ => FilesDirectory::NeedsWorktreeSetup,
            };
        }
        let path = PathBuf::from(repo);
        if path.is_dir() {
            FilesDirectory::Ready(Workdir::Host(path))
        } else {
            FilesDirectory::Missing(format!("Workspace directory does not exist: {repo}"))
        }
    }

    pub fn ready_directory(&self) -> Option<Workdir> {
        match self.directory() {
            FilesDirectory::Ready(dir) => Some(dir),
            _ => None,
        }
    }
}

/// Agent capability values for a node, possibly inherited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgent {
    pub source_node_id: String,
    pub source_title: String,
    pub inherited: bool,
    pub agent: NodeAgent,
}

impl ResolvedAgent {
    /// Launch options with unset values following the settings for `role`.
    pub fn launch_options(&self, settings: &TodSettings, role: AgentRole) -> AgentLaunchOptions {
        self.agent
            .launch_options(&settings.launch_options_for(role))
    }
}

/// Nearest node (starting at `node_id`, walking up) with `cap` enabled.
fn nearest_with_capability(
    conn: &Connection,
    node_id: &str,
    cap: Capability,
) -> Result<Option<(Uuid, String, bool)>> {
    let Ok(uuid) = Uuid::parse_str(node_id) else {
        return Ok(None);
    };
    let chain = crate::outline::ancestor_chain(conn, uuid)?;
    let node_repo = NodeRepo::new(conn);
    for id in chain.into_iter().rev() {
        if node_repo.list_capabilities(id)?.contains(&cap) {
            let title = node_repo.get(id)?.map(|n| n.title).unwrap_or_default();
            return Ok(Some((id, title, id != uuid)));
        }
    }
    Ok(None)
}

pub fn resolve_files_for_node(conn: &Connection, node_id: &str) -> Result<Option<ResolvedFiles>> {
    let Some((source, source_title, inherited)) =
        nearest_with_capability(conn, node_id, Capability::Files)?
    else {
        return Ok(None);
    };
    let source_node_id = source.to_string();
    let (repo, branch): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT repo, branch FROM node_fields WHERE node_id = ?1",
            params![uuid_to_blob(source)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or_default();
    let files = NodeFilesRepo::new(conn).get(&source_node_id)?;
    Ok(Some(ResolvedFiles {
        source_title,
        inherited,
        repo,
        branch,
        use_worktree: files.as_ref().is_some_and(|f| f.use_worktree),
        worktree_path: files.as_ref().and_then(|f| f.worktree_path.clone()),
        worktree_lease_id: files.as_ref().and_then(|f| f.worktree_lease_id.clone()),
        worktree_lease_holder: files.as_ref().and_then(|f| f.worktree_lease_holder.clone()),
        dev_container: files.and_then(|f| f.dev_container),
        source_node_id,
    }))
}

pub fn resolve_agent_for_node(conn: &Connection, node_id: &str) -> Result<Option<ResolvedAgent>> {
    let Some((source, source_title, inherited)) =
        nearest_with_capability(conn, node_id, Capability::Agent)?
    else {
        return Ok(None);
    };
    let source_node_id = source.to_string();
    let agent = NodeAgentRepo::new(conn)
        .get(&source_node_id)?
        .unwrap_or_else(|| NodeAgent {
            node_id: source_node_id.clone(),
            ..NodeAgent::default()
        });
    Ok(Some(ResolvedAgent {
        source_node_id,
        source_title,
        inherited,
        agent,
    }))
}

/// For every node in `list_id`, the node that owns `cap` for it — itself when
/// it has the capability, else the nearest ancestor that does. Nodes with no
/// owner in their chain are absent from the map.
///
/// The list-scoped counterpart of [`nearest_with_capability`]: two queries for
/// the whole list instead of an ancestor walk per node.
pub fn capability_sources_for_list(
    conn: &Connection,
    list_id: Uuid,
    cap: Capability,
) -> Result<HashMap<Uuid, Uuid>> {
    let mut parents: HashMap<Uuid, Option<Uuid>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT node_id, parent_id FROM outline_entries WHERE list_id = ?1",
        )?;
        let rows = stmt.query_map(params![uuid_to_blob(list_id)], |row| {
            let node: Vec<u8> = row.get(0)?;
            let parent: Option<Vec<u8>> = row.get(1)?;
            Ok((
                blob_to_uuid_sql(&node)?,
                parent.as_deref().map(blob_to_uuid_sql).transpose()?,
            ))
        })?;
        for row in rows {
            let (node, parent) = row?;
            parents.insert(node, parent);
        }
    }

    let mut owners: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    {
        let mut stmt = conn.prepare(
            "SELECT c.node_id
             FROM node_capabilities c
             INNER JOIN outline_entries e ON e.node_id = c.node_id
             WHERE e.list_id = ?1 AND c.capability = ?2",
        )?;
        let rows = stmt.query_map(params![uuid_to_blob(list_id), cap.as_str()], |row| {
            let node: Vec<u8> = row.get(0)?;
            blob_to_uuid_sql(&node)
        })?;
        for row in rows {
            owners.insert(row?);
        }
    }

    let mut sources: HashMap<Uuid, Uuid> = HashMap::new();
    for &node in parents.keys() {
        // Walk up to the nearest owner, memoizing every node on the way so a
        // deep tree still costs one pass.
        let mut chain = Vec::new();
        let mut cursor = Some(node);
        let mut source = None;
        while let Some(id) = cursor {
            if let Some(hit) = sources.get(&id) {
                source = Some(*hit);
                break;
            }
            if owners.contains(&id) {
                source = Some(id);
                break;
            }
            chain.push(id);
            cursor = parents.get(&id).copied().flatten();
        }
        if let Some(source) = source {
            for id in chain {
                sources.insert(id, source);
            }
            sources.entry(node).or_insert(source);
        }
    }
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::store::FleetStore;
    use crate::fleet::test_util::{cleanup_fleet_root, temp_fleet_root};
    use crate::fleet::writer::FleetMutation;
    use crate::outline::{CreatePosition, OutlineMutation};

    struct Tree {
        root: PathBuf,
        store: FleetStore,
        grandparent: Uuid,
        parent: Uuid,
        child: Uuid,
    }

    fn setup_tree() -> Tree {
        let root = temp_fleet_root();
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "t".into(),
                title: "T".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        let list_id = store.list_outline_lists().unwrap()[0].id;
        let mut ids = Vec::new();
        let mut parent: Option<Uuid> = None;
        for title in ["Grandparent", "Parent", "Child"] {
            let node_id = Uuid::new_v4();
            store
                .enqueue_outline(OutlineMutation::CreateNode {
                    node_id: Some(node_id),
                    list_id,
                    parent_id: parent,
                    anchor_id: parent,
                    position: if parent.is_some() {
                        CreatePosition::Child
                    } else {
                        CreatePosition::Below
                    },
                    title: title.into(),
                })
                .unwrap();
            store.writer().flush().unwrap();
            ids.push(node_id);
            parent = Some(node_id);
        }
        store.reload_if_stale().ok();
        Tree {
            root,
            store,
            grandparent: ids[0],
            parent: ids[1],
            child: ids[2],
        }
    }

    fn enable(store: &FleetStore, node: Uuid, caps: Vec<Capability>) {
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: caps,
            })
            .unwrap();
        store.writer().flush().unwrap();
    }

    fn set_repo(store: &FleetStore, node: Uuid, repo: &str) {
        store
            .enqueue(FleetMutation::UpdateTaskRepo {
                id: node.to_string(),
                repo: Some(repo.into()),
            })
            .unwrap();
        store.writer().flush().unwrap();
    }

    #[test]
    fn child_inherits_parent_files() {
        let tree = setup_tree();
        enable(&tree.store, tree.parent, vec![Capability::Files]);
        set_repo(&tree.store, tree.parent, "/parent/repo");
        let files = tree
            .store
            .resolve_files_for_node(&tree.child.to_string())
            .unwrap()
            .expect("inherited files");
        assert!(files.inherited);
        assert_eq!(files.source_node_id, tree.parent.to_string());
        assert_eq!(files.source_title, "Parent");
        assert_eq!(files.repo(), Some("/parent/repo"));
        drop(tree.store);
        cleanup_fleet_root(&tree.root);
    }

    #[test]
    fn child_local_capability_overrides_parent() {
        let tree = setup_tree();
        enable(&tree.store, tree.parent, vec![Capability::Files]);
        set_repo(&tree.store, tree.parent, "/parent/repo");
        enable(&tree.store, tree.child, vec![Capability::Files]);
        set_repo(&tree.store, tree.child, "/child/repo");
        let files = tree
            .store
            .resolve_files_for_node(&tree.child.to_string())
            .unwrap()
            .unwrap();
        assert!(!files.inherited);
        assert_eq!(files.repo(), Some("/child/repo"));
        drop(tree.store);
        cleanup_fleet_root(&tree.root);
    }

    #[test]
    fn skips_parent_without_capability_uses_grandparent() {
        let tree = setup_tree();
        enable(&tree.store, tree.grandparent, vec![Capability::Agent]);
        tree.store
            .enqueue(FleetMutation::UpsertNodeAgent {
                node_id: tree.grandparent.to_string(),
                platform: Some("cursor".into()),
                model: None,
                effort: None,
            })
            .unwrap();
        enable(&tree.store, tree.parent, vec![Capability::Files]);
        let agent = tree
            .store
            .resolve_agent_for_node(&tree.child.to_string())
            .unwrap()
            .expect("grandparent agent");
        assert!(agent.inherited);
        assert_eq!(agent.source_node_id, tree.grandparent.to_string());
        assert_eq!(agent.agent.platform.as_deref(), Some("cursor"));
        drop(tree.store);
        cleanup_fleet_root(&tree.root);
    }

    #[test]
    fn no_capability_in_chain_resolves_nothing() {
        let tree = setup_tree();
        assert!(
            tree.store
                .resolve_files_for_node(&tree.child.to_string())
                .unwrap()
                .is_none()
        );
        assert!(
            tree.store
                .resolve_agent_for_node(&tree.child.to_string())
                .unwrap()
                .is_none()
        );
        drop(tree.store);
        cleanup_fleet_root(&tree.root);
    }

    #[test]
    fn list_scoped_sources_match_per_node_resolution() {
        let tree = setup_tree();
        // Agent on the grandparent, Files on the parent: the child inherits
        // Agent across the parent (which lacks it) and Files from the parent.
        enable(&tree.store, tree.grandparent, vec![Capability::Agent]);
        enable(&tree.store, tree.parent, vec![Capability::Files]);
        tree.store.reload_if_stale().ok();
        let list_id = tree.store.list_outline_lists().unwrap()[0].id;

        let agents = tree
            .store
            .capability_sources_for_list(list_id, Capability::Agent)
            .unwrap();
        assert_eq!(agents.get(&tree.grandparent), Some(&tree.grandparent));
        assert_eq!(agents.get(&tree.parent), Some(&tree.grandparent));
        assert_eq!(agents.get(&tree.child), Some(&tree.grandparent));

        let files = tree
            .store
            .capability_sources_for_list(list_id, Capability::Files)
            .unwrap();
        // The grandparent is above the owner, so Files resolve to nothing there.
        assert_eq!(files.get(&tree.grandparent), None);
        assert_eq!(files.get(&tree.parent), Some(&tree.parent));
        assert_eq!(files.get(&tree.child), Some(&tree.parent));

        // A node's own capability wins over the ancestor's.
        enable(&tree.store, tree.child, vec![Capability::Files]);
        tree.store.reload_if_stale().ok();
        let files = tree
            .store
            .capability_sources_for_list(list_id, Capability::Files)
            .unwrap();
        assert_eq!(files.get(&tree.child), Some(&tree.child));

        // And it agrees with the per-node resolution it replaces.
        for node in [tree.grandparent, tree.parent, tree.child] {
            let id = node.to_string();
            assert_eq!(
                files.get(&node).copied().map(|id| id.to_string()),
                tree.store
                    .resolve_files_for_node(&id)
                    .unwrap()
                    .map(|f| f.source_node_id),
            );
            assert_eq!(
                agents.get(&node).copied().map(|id| id.to_string()),
                tree.store
                    .resolve_agent_for_node(&id)
                    .unwrap()
                    .map(|a| a.source_node_id),
            );
        }
        drop(tree.store);
        cleanup_fleet_root(&tree.root);
    }

    #[test]
    fn no_capability_anywhere_yields_an_empty_source_map() {
        let tree = setup_tree();
        let list_id = tree.store.list_outline_lists().unwrap()[0].id;
        assert!(
            tree.store
                .capability_sources_for_list(list_id, Capability::Agent)
                .unwrap()
                .is_empty()
        );
        drop(tree.store);
        cleanup_fleet_root(&tree.root);
    }

    #[test]
    fn directory_reflects_worktree_state() {
        let dir = std::env::temp_dir();
        let mut files = ResolvedFiles {
            source_node_id: Uuid::new_v4().to_string(),
            source_title: "N".into(),
            inherited: false,
            repo: None,
            branch: None,
            use_worktree: false,
            worktree_path: None,
            worktree_lease_id: None,
            worktree_lease_holder: None,
            dev_container: None,
        };
        let host = Workdir::host(&dir);
        assert!(matches!(files.directory(), FilesDirectory::Missing(_)));
        files.repo = Some(dir.display().to_string());
        assert_eq!(files.directory(), FilesDirectory::Ready(host.clone()));
        files.use_worktree = true;
        assert_eq!(files.directory(), FilesDirectory::NeedsWorktreeSetup);
        files.worktree_path = Some(dir.display().to_string());
        assert_eq!(files.directory(), FilesDirectory::Ready(host.clone()));

        // A dev container needs one chosen; mounted, the directory stays the host's.
        files.dev_container = Some(DevContainerSetting::default());
        assert_eq!(
            files.directory(),
            FilesDirectory::Missing("Choose a dev container".into())
        );
        files.dev_container = Some(DevContainerSetting {
            container: Some("dev".into()),
            directory: None,
            repo_on_host: true,
        });
        assert_eq!(files.directory(), FilesDirectory::Ready(host));
    }

    #[test]
    fn a_repository_in_a_dev_container_is_a_container_path() {
        let mut files = ResolvedFiles {
            source_node_id: Uuid::new_v4().to_string(),
            source_title: "N".into(),
            inherited: false,
            repo: Some(r"C:\not\there".into()),
            branch: None,
            use_worktree: false,
            worktree_path: None,
            worktree_lease_id: None,
            worktree_lease_holder: None,
            dev_container: Some(DevContainerSetting {
                container: Some("dev".into()),
                ..Default::default()
            }),
        };
        assert!(matches!(files.directory(), FilesDirectory::Missing(_)));
        files.repo = Some("/workspaces/app".into());
        assert_eq!(
            files.directory(),
            FilesDirectory::Ready(Workdir::container("dev", "/workspaces/app"))
        );
        files.use_worktree = true;
        assert_eq!(files.directory(), FilesDirectory::NeedsWorktreeSetup);
        files.worktree_path = Some("/workspaces/app/.worktrees/x".into());
        assert_eq!(
            files.directory(),
            FilesDirectory::Ready(Workdir::container("dev", "/workspaces/app/.worktrees/x"))
        );
    }
}
