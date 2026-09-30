//! Resolve a node's Files and Agent capability values.
//!
//! Both inherit from the nearest ancestor (or the node itself) that has the
//! capability enabled; nodes between them without the capability are skipped.
//!
//! Files settings are a recipe. When they call for a worktree or a sandbox,
//! every node that resolves to them works in one of its own, made the first
//! time it needs its files ([`crate::fleet::provision::resolve_launch_cwd`])
//! and recorded as its location ([`crate::fleet::repos::files_location`]).
//! Otherwise every such node shares the workspace directory.

use crate::agent_launch::AgentLaunchOptions;
use crate::fleet::repos::files_location::{FilesLocation, FilesLocationRepo, recipe_key};
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
use std::path::PathBuf;
use uuid::Uuid;

/// Files capability values for a node, possibly inherited, and the location
/// made for that node from them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFiles {
    /// The node these were resolved for.
    pub node_id: String,
    /// Node that owns the Files capability (the node itself unless inherited).
    pub source_node_id: String,
    pub source_title: String,
    pub inherited: bool,
    /// Workspace directory (`node_fields.repo` of the source).
    pub repo: Option<String>,
    /// The branch this node's work is on: its own `node_fields.branch` when
    /// it gets a location of its own ([`Self::per_node`]), else the source's.
    pub branch: Option<String>,
    /// Each node works in a worktree of its own (not with a sandbox, which
    /// is already its own).
    pub use_worktree: bool,
    /// Set when launches run in a dev container or cloud sandboxes. When the
    /// repository lives in it, `repo` and worktrees are paths there; when it
    /// is mounted from this machine they stay host paths, and the
    /// container's own is resolved against the running container at launch
    /// (see [`crate::fleet::dev_container`]).
    pub dev_container: Option<DevContainerSetting>,
    /// This node's location, made from these settings.
    pub location: Option<FilesLocation>,
    /// A location this node has that was made from other settings (they
    /// changed since, or it now resolves to another node's): never used,
    /// only removed.
    pub stale_location: Option<FilesLocation>,
}

/// Where launches from a node run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesDirectory {
    Ready(Workdir),
    /// The node gets a worktree or sandbox of its own, not made yet: the
    /// first launch makes it.
    NotMade,
    /// No usable directory; the reason is user-facing.
    Missing(String),
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

/// A git remote URL (`https://…`, `ssh://…`, `git@host:owner/repo`) rather
/// than a path. A Windows path (`C:\src`, `C:/src`) has no `@` before its
/// colon, so it is not one.
fn is_remote_url(value: &str) -> bool {
    if value.contains("://") {
        return true;
    }
    match value.split_once(':') {
        Some((user_host, _)) => user_host.contains('@') && !user_host.contains(['/', '\\']),
        None => false,
    }
}

impl ResolvedFiles {
    pub fn repo(&self) -> Option<&str> {
        non_empty(&self.repo)
    }

    pub fn branch(&self) -> Option<&str> {
        non_empty(&self.branch)
    }

    /// The workspace directory when it is a git remote URL rather than a
    /// directory: an autonomous cloud node's repository, which is cloned in
    /// the node's sandbox and has no checkout on this machine.
    pub fn repo_url(&self) -> Option<&str> {
        self.repo().filter(|repo| is_remote_url(repo))
    }

    /// Each node works in a cloud sandbox of its own.
    pub fn runs_in_sandbox(&self) -> bool {
        self.dev_container.as_ref().is_some_and(|dev| dev.sandbox)
    }

    /// Whether each node gets a location of its own (a worktree or a
    /// sandbox), rather than sharing the workspace directory.
    pub fn per_node(&self) -> bool {
        self.runs_in_sandbox() || self.use_worktree
    }

    /// What each node gets from these settings, in a phrase
    /// ("a sandbox from image X", "a worktree in container Y").
    pub fn describe_recipe(&self) -> String {
        match &self.dev_container {
            Some(dev) if dev.sandbox => {
                format!("a cloud sandbox from {}", dev.sandbox_from.describe())
            }
            Some(dev) => {
                let container = dev.container().unwrap_or("(none chosen)");
                match (self.use_worktree, dev.repo_on_host) {
                    (true, true) => format!("a worktree, run in dev container {container}"),
                    (true, false) => format!("a worktree in dev container {container}"),
                    (false, _) => format!("the workspace directory in dev container {container}"),
                }
            }
            None if self.use_worktree => "a worktree on this machine".into(),
            None => "the workspace directory on this machine".into(),
        }
    }

    /// The fingerprint of these settings a location is made from.
    pub fn recipe_key(&self) -> String {
        recipe_key(self.repo(), self.use_worktree, self.dev_container.as_ref())
    }

    /// This node's worktree, when it has one.
    pub fn worktree_path(&self) -> Option<&str> {
        self.location
            .as_ref()
            .filter(|_| !self.runs_in_sandbox())
            .and_then(FilesLocation::worktree_path)
    }

    /// The dev container the repository lives in, when it lives in one.
    pub fn repo_container(&self) -> Option<&str> {
        self.dev_container
            .as_ref()
            .and_then(DevContainerSetting::repo_container)
    }

    /// This node's cloud sandbox, once it has been made.
    pub fn repo_sandbox(&self) -> Option<&str> {
        if !self.runs_in_sandbox() {
            return None;
        }
        self.location.as_ref().and_then(FilesLocation::sandbox)
    }

    /// The repository is not on this machine.
    pub fn repo_is_remote(&self) -> bool {
        self.dev_container
            .as_ref()
            .is_some_and(DevContainerSetting::repo_is_remote)
    }

    /// `path` (the workspace directory or worktree) where it is: in the
    /// repository's container or this node's sandbox, else on this machine.
    pub fn workdir(&self, path: &str) -> Workdir {
        if let Some(container) = self.repo_container() {
            return Workdir::container(container, path);
        }
        if let Some(sandbox) = self.repo_sandbox() {
            return Workdir::sandbox(sandbox, path);
        }
        Workdir::host(path)
    }

    /// The workspace directory, where it is. `None` for a sandbox not made
    /// yet: there is no machine to find it on.
    pub fn repo_dir(&self) -> Option<Workdir> {
        if self.runs_in_sandbox() && self.repo_sandbox().is_none() {
            return None;
        }
        self.repo().map(|repo| self.workdir(repo))
    }

    /// This node's worktree, where it is.
    pub fn worktree_dir(&self) -> Option<Workdir> {
        self.worktree_path().map(|path| self.workdir(path))
    }

    /// Why no location can be made from these settings yet (user-facing).
    pub fn incomplete(&self) -> Option<String> {
        if let Some(dev) = &self.dev_container {
            if dev.sandbox {
                if let Some(reason) = dev.sandbox_from.incomplete() {
                    return Some(reason.into());
                }
            } else if dev.container().is_none() {
                return Some("Choose a dev container".into());
            }
        }
        let Some(repo) = self.repo() else {
            return Some("Set a workspace directory".into());
        };
        let remote = if self.runs_in_sandbox() {
            Some(("the sandbox", "/root/app"))
        } else {
            self.repo_container().map(|_| ("the dev container", "/workspaces/app"))
        };
        if let Some((place, example)) = remote {
            if !repo.starts_with('/') {
                return Some(format!(
                    "The workspace directory is inside {place}: give its path there, like {example}"
                ));
            }
        }
        None
    }

    /// The resolved directory: this node's worktree or sandbox when it gets
    /// one, else the workspace directory. With the repository mounted into
    /// a dev container, the host side of it. One inside a container or
    /// sandbox is not checked here (that takes Docker, or the network); a
    /// launch into a missing one fails.
    pub fn directory(&self) -> FilesDirectory {
        if let Some(reason) = self.incomplete() {
            return FilesDirectory::Missing(reason);
        }
        let Some(repo) = self.repo() else {
            return FilesDirectory::Missing("Set a workspace directory".into());
        };
        if self.runs_in_sandbox() {
            return match self.repo_sandbox() {
                Some(_) => FilesDirectory::Ready(self.workdir(repo)),
                None => FilesDirectory::NotMade,
            };
        }
        if self.use_worktree {
            return match self.worktree_dir() {
                Some(dir) if self.repo_container().is_some() || dir.is_dir() => {
                    FilesDirectory::Ready(dir)
                }
                _ => FilesDirectory::NotMade,
            };
        }
        if self.repo_container().is_some() {
            return FilesDirectory::Ready(self.workdir(repo));
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

/// Whether `node_id` is a **task node** (`doc/glossary.md`): the Lifecycle
/// capability on the node itself, and Agent on it or inherited from an
/// ancestor. The unified view opens the task panel for these by default.
pub fn is_task_node(conn: &Connection, node_id: Uuid) -> Result<bool> {
    if !NodeRepo::new(conn)
        .list_capabilities(node_id)?
        .contains(&Capability::Lifecycle)
    {
        return Ok(false);
    }
    Ok(nearest_with_capability(conn, &node_id.to_string(), Capability::Agent)?.is_some())
}

pub fn resolve_files_for_node(conn: &Connection, node_id: &str) -> Result<Option<ResolvedFiles>> {
    let Some((source, source_title, inherited)) =
        nearest_with_capability(conn, node_id, Capability::Files)?
    else {
        return Ok(None);
    };
    let source_node_id = source.to_string();
    let fields = |id: Uuid| -> Result<(Option<String>, Option<String>)> {
        Ok(conn
            .query_row(
                "SELECT repo, branch FROM node_fields WHERE node_id = ?1",
                params![uuid_to_blob(id)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .unwrap_or_default())
    };
    let (repo, source_branch) = fields(source)?;
    let files = NodeFilesRepo::new(conn).get(&source_node_id)?;
    let mut resolved = ResolvedFiles {
        node_id: node_id.to_string(),
        source_title,
        inherited,
        repo,
        branch: source_branch,
        use_worktree: files.as_ref().is_some_and(|f| f.use_worktree),
        dev_container: files.and_then(|f| f.dev_container),
        location: None,
        stale_location: None,
        source_node_id,
    };
    if resolved.per_node() && inherited {
        resolved.branch = match Uuid::parse_str(node_id) {
            Ok(id) => fields(id)?.1,
            Err(_) => None,
        };
    }
    if let Some(location) = FilesLocationRepo::new(conn).get(node_id)? {
        let current = resolved.per_node()
            && location.source_node_id == resolved.source_node_id
            && location.recipe == resolved.recipe_key();
        if current {
            resolved.location = Some(location);
        } else {
            resolved.stale_location = Some(location);
        }
    }
    Ok(Some(resolved))
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
    fn environment_merges_down_the_tree_nearest_wins() {
        use crate::environment::{Entry, resolve};
        let tree = setup_tree();
        let set = |node: Uuid, entries: Vec<Entry>| {
            enable(&tree.store, node, vec![Capability::Environment]);
            tree.store.reload_if_stale().ok();
            tree.store
                .enqueue_outline(OutlineMutation::SetNodeEnvironment { node_id: node, entries })
                .unwrap();
            tree.store.writer().flush().unwrap();
        };
        let mut gb = Entry::secret("growthbook");
        gb.hosts = vec!["api.growthbook.io".into()];
        gb.description = Some("prod flags".into());
        set(tree.grandparent, vec![gb, Entry::variable("region", "eu")]);
        // The child redefines `region` and, with the parent lacking the
        // capability, still inherits `growthbook` across it.
        set(tree.child, vec![Entry::variable("region", "us")]);
        let env = tree.store.read(|conn| resolve(conn, tree.child)).unwrap();
        let names: Vec<_> = env.iter().map(|r| r.entry.name.as_str()).collect();
        assert_eq!(names, ["growthbook", "region"]);
        let region = env.iter().find(|r| r.entry.name == "region").unwrap();
        assert_eq!(region.entry.value.as_deref(), Some("us"));
        assert!(!region.inherited);
        let gb = env.iter().find(|r| r.entry.name == "growthbook").unwrap();
        assert!(gb.inherited);
        assert_eq!(gb.source_node, tree.grandparent);
        assert_eq!(gb.entry.description(), Some("prod flags"));
        // A mutation on a node without the capability is refused.
        assert!(
            tree.store
                .enqueue_outline(OutlineMutation::SetNodeEnvironment { node_id: tree.parent, entries: vec![] })
                .is_err()
                || tree.store.writer().flush().is_err()
                || tree.store.read(|c| crate::environment::entries(c, tree.parent)).unwrap().is_empty()
        );
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

    fn resolved(repo: &str, dev: Option<DevContainerSetting>) -> ResolvedFiles {
        ResolvedFiles {
            node_id: Uuid::new_v4().to_string(),
            source_node_id: Uuid::new_v4().to_string(),
            source_title: "N".into(),
            inherited: false,
            repo: Some(repo.into()),
            branch: None,
            use_worktree: false,
            dev_container: dev,
            location: None,
            stale_location: None,
        }
    }

    fn location_at(files: &ResolvedFiles, worktree: Option<&str>, sandbox: Option<&str>) -> FilesLocation {
        FilesLocation {
            node_id: files.node_id.clone(),
            source_node_id: files.source_node_id.clone(),
            recipe: files.recipe_key(),
            repo: files.repo.clone(),
            container: files.repo_container().map(str::to_string),
            worktree_path: worktree.map(str::to_string),
            worktree_lease_id: None,
            worktree_lease_holder: None,
            sandbox: sandbox.map(str::to_string),
            created_at: 1,
        }
    }

    #[test]
    fn directory_reflects_worktree_state() {
        let dir = std::env::temp_dir();
        let host = Workdir::host(&dir);
        let mut files = resolved("", None);
        files.repo = None;
        assert!(matches!(files.directory(), FilesDirectory::Missing(_)));
        files.repo = Some(dir.display().to_string());
        assert_eq!(files.directory(), FilesDirectory::Ready(host.clone()));
        files.use_worktree = true;
        assert_eq!(files.directory(), FilesDirectory::NotMade);
        files.location = Some(location_at(&files, Some(&dir.display().to_string()), None));
        assert_eq!(files.directory(), FilesDirectory::Ready(host.clone()));

        // A dev container needs one chosen; mounted, the directory stays the host's.
        files.use_worktree = false;
        files.location = None;
        files.dev_container = Some(DevContainerSetting::default());
        assert_eq!(
            files.directory(),
            FilesDirectory::Missing("Choose a dev container".into())
        );
        files.dev_container = Some(DevContainerSetting {
            container: Some("dev".into()),
            repo_on_host: true,
            ..Default::default()
        });
        assert_eq!(files.directory(), FilesDirectory::Ready(host));
    }

    #[test]
    fn a_repository_in_a_dev_container_is_a_container_path() {
        let dev = DevContainerSetting { container: Some("dev".into()), ..Default::default() };
        let mut files = resolved(r"C:\not\there", Some(dev));
        assert!(matches!(files.directory(), FilesDirectory::Missing(_)));
        files.repo = Some("/workspaces/app".into());
        assert_eq!(
            files.directory(),
            FilesDirectory::Ready(Workdir::container("dev", "/workspaces/app"))
        );
        files.use_worktree = true;
        assert_eq!(files.directory(), FilesDirectory::NotMade);
        files.location = Some(location_at(&files, Some("/workspaces/app/.worktrees/x"), None));
        assert_eq!(
            files.directory(),
            FilesDirectory::Ready(Workdir::container("dev", "/workspaces/app/.worktrees/x"))
        );
    }

    #[test]
    fn each_node_gets_its_own_sandbox() {
        let dev = DevContainerSetting { sandbox: true, ..Default::default() };
        let mut files = resolved("/root/app", Some(dev));
        assert!(files.per_node());
        assert!(files.repo_is_remote());
        assert_eq!(files.directory(), FilesDirectory::NotMade);
        assert_eq!(files.repo_dir(), None, "no sandbox to find it in yet");
        files.location = Some(location_at(&files, None, Some("tod-x")));
        assert_eq!(files.repo_container(), None);
        assert_eq!(files.directory(), FilesDirectory::Ready(Workdir::sandbox("tod-x", "/root/app")));
        files.repo = Some(r"C:\src\app".into());
        assert!(matches!(files.directory(), FilesDirectory::Missing(_)));

        // Forking needs a sandbox to fork.
        let mut files = resolved(
            "/root/app",
            Some(DevContainerSetting {
                sandbox: true,
                sandbox_from: crate::fleet::repos::node_files::SandboxFrom::Fork(String::new()),
                ..Default::default()
            }),
        );
        assert_eq!(files.directory(), FilesDirectory::Missing("Choose the sandbox to fork".into()));
        files.dev_container.as_mut().unwrap().sandbox_from =
            crate::fleet::repos::node_files::SandboxFrom::Fork("template".into());
        assert_eq!(files.directory(), FilesDirectory::NotMade);
    }

    /// An inheriting node resolves its own branch and its own location, and
    /// one made from settings that have since changed is stale.
    #[test]
    fn inheriting_nodes_get_their_own_location() {
        let tree = setup_tree();
        enable(&tree.store, tree.grandparent, vec![Capability::Files]);
        let (gp, child) = (tree.grandparent.to_string(), tree.child.to_string());
        tree.store
            .enqueue(FleetMutation::UpdateTaskRepo { id: gp.clone(), repo: Some("/root/app".into()) })
            .unwrap();
        tree.store
            .enqueue(FleetMutation::UpdateTaskBranch { id: gp.clone(), branch: Some("main".into()) })
            .unwrap();
        tree.store
            .enqueue(FleetMutation::UpdateTaskBranch { id: child.clone(), branch: Some("task/child".into()) })
            .unwrap();
        tree.store
            .enqueue(FleetMutation::SetNodeDevContainer {
                node_id: gp.clone(),
                dev_container: Some(DevContainerSetting { sandbox: true, ..Default::default() }),
            })
            .unwrap();
        tree.store.writer().flush().unwrap();
        tree.store.reload_if_stale().unwrap();

        let files = tree.store.resolve_files_for_node(&child).unwrap().unwrap();
        assert!(files.inherited);
        assert_eq!(files.branch(), Some("task/child"));
        assert_eq!(files.directory(), FilesDirectory::NotMade);
        tree.store
            .enqueue(FleetMutation::RecordFilesLocation { location: location_at(&files, None, Some("tod-child")) })
            .unwrap();
        tree.store.writer().flush().unwrap();
        tree.store.reload_if_stale().unwrap();
        let files = tree.store.resolve_files_for_node(&child).unwrap().unwrap();
        assert_eq!(files.repo_sandbox(), Some("tod-child"));
        // The parent has none of its own.
        let parent = tree.store.resolve_files_for_node(&gp).unwrap().unwrap();
        assert_eq!(parent.branch(), Some("main"));
        assert_eq!(parent.directory(), FilesDirectory::NotMade);

        // Changing what sandboxes are made from leaves the child's stale.
        tree.store
            .enqueue(FleetMutation::SetNodeDevContainer {
                node_id: gp.clone(),
                dev_container: Some(DevContainerSetting {
                    sandbox: true,
                    sandbox_from: crate::fleet::repos::node_files::SandboxFrom::Image("other".into()),
                    ..Default::default()
                }),
            })
            .unwrap();
        tree.store.writer().flush().unwrap();
        tree.store.reload_if_stale().unwrap();
        let files = tree.store.resolve_files_for_node(&child).unwrap().unwrap();
        assert_eq!(files.location, None);
        assert_eq!(files.stale_location.as_ref().and_then(|l| l.sandbox()), Some("tod-child"));
        assert_eq!(files.directory(), FilesDirectory::NotMade);
        drop(tree.store);
        cleanup_fleet_root(&tree.root);
    }

    #[test]
    fn task_node_with_own_agent() {
        let tree = setup_tree();
        enable(&tree.store, tree.child, vec![Capability::Lifecycle, Capability::Agent]);
        assert!(tree.store.read(|conn| is_task_node(conn, tree.child)).unwrap());
        drop(tree.store);
        cleanup_fleet_root(&tree.root);
    }

    #[test]
    fn task_node_with_inherited_agent() {
        let tree = setup_tree();
        enable(&tree.store, tree.grandparent, vec![Capability::Agent]);
        enable(&tree.store, tree.child, vec![Capability::Lifecycle]);
        assert!(tree.store.read(|conn| is_task_node(conn, tree.child)).unwrap());
        // The ancestor itself has no Lifecycle, so it is not a task.
        assert!(!tree.store.read(|conn| is_task_node(conn, tree.grandparent)).unwrap());
        drop(tree.store);
        cleanup_fleet_root(&tree.root);
    }

    #[test]
    fn not_a_task_node_without_lifecycle_or_agent() {
        let tree = setup_tree();
        assert!(!tree.store.read(|conn| is_task_node(conn, tree.child)).unwrap());
        enable(&tree.store, tree.child, vec![Capability::Lifecycle]);
        assert!(!tree.store.read(|conn| is_task_node(conn, tree.child)).unwrap());
        drop(tree.store);
        cleanup_fleet_root(&tree.root);
    }
}
