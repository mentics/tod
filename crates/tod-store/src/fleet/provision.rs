//! Launch directories from the Files capability, and worktree set up / release.

use crate::fleet::FleetStore;
use crate::fleet::node_actions::{FilesDirectory, ResolvedFiles};
use crate::fleet::terminal::{prune_stale_shell_sessions, prune_stale_terminal_agent_runs};
use crate::fleet::workdir::Workdir;
use crate::fleet::worktree::{self, WorktreeHandle, validate_git_repo};
use crate::fleet::writer::FleetMutation;
use crate::paths::TodPaths;
use crate::settings::TodSettings;
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;

fn resolve_files(fleet: &FleetStore, node_id: &str) -> Result<ResolvedFiles> {
    fleet.reload_if_stale().ok();
    fleet
        .resolve_files_for_node(node_id)?
        .context("Enable Files and set a workspace directory")
}

/// Directory that shells, editors, and coding agents launched from `node_id`
/// run in: on this machine, or inside the dev container the repository
/// lives in.
///
/// Never provisions: a node whose worktree hasn't been set up is an error.
pub fn resolve_launch_cwd(fleet: &FleetStore, node_id: &str) -> Result<Workdir> {
    match resolve_files(fleet, node_id)?.directory() {
        FilesDirectory::Ready(dir) => Ok(dir),
        FilesDirectory::NeedsWorktreeSetup => {
            bail!("Set up the worktree (Files) before launching")
        }
        FilesDirectory::Missing(reason) => bail!("{reason}"),
    }
}

/// Set up (or reuse) the worktree for the node owning `node_id`'s Files capability,
/// using git or Treehouse per settings, and record it on that node. For a
/// repository inside a dev container, git or Treehouse runs there.
pub fn setup_worktree_for_node(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    node_id: &str,
) -> Result<Workdir> {
    let files = resolve_files(fleet, node_id)?;
    if !files.use_worktree {
        bail!("Turn on the worktree flag (Files) before setting up a worktree");
    }
    if let FilesDirectory::Missing(reason) = files.directory() {
        bail!("{reason}");
    }
    if let Some(dir) = files.worktree_dir() {
        if dir.is_dir() {
            return Ok(dir);
        }
    }
    let repo = files
        .repo_dir()
        .context("Set a workspace directory before setting up a worktree")?;
    let repo_path = validate_git_repo(&repo)?;
    let branch = files.branch().unwrap_or_default().to_string();
    let owner = files.source_node_id.clone();
    let lease_holder = format!("tod-{owner}");
    let data_root = settings.resolve_fleet_storage_root(paths)?;

    let handle: WorktreeHandle = {
        let projection = fleet.projection();
        let guard = projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        worktree::ensure_worktree(
            &conn,
            settings.worktree_backend,
            settings,
            paths,
            &data_root,
            &repo_path,
            &branch,
            &lease_holder,
        )?
    };

    fleet.enqueue(FleetMutation::UpdateNodeWorktree {
        node_id: owner,
        worktree_path: Some(handle.path.storage()),
        worktree_lease_id: handle.lease.as_ref().map(|l| l.lease_id.clone()),
        worktree_lease_holder: handle.lease.as_ref().map(|l| l.lease_holder.clone()),
    })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;

    if !handle.path.is_dir() {
        bail!("set-up worktree missing at {}", handle.path);
    }
    Ok(handle.path)
}

/// Change the branch of the node owning `node_id`'s Files capability while its
/// worktree is set up: the worktree's branch is renamed in place, in the
/// superproject and every submodule that has it (see [`worktree::rename_branch`]),
/// then recorded. Returns warnings the user should see (a renamed branch that
/// was already pushed).
pub fn rename_branch_for_node(fleet: &FleetStore, node_id: &str, new: &str) -> Result<Vec<String>> {
    let files = resolve_files(fleet, node_id)?;
    let path = files.worktree_dir().context("No worktree is set up")?;
    if new.is_empty() {
        bail!("A worktree needs a branch; release it to clear the branch");
    }
    let old = match files.branch().filter(|b| !b.is_empty()) {
        Some(branch) => branch.to_string(),
        None => worktree::current_branch(&path)
            .context("The worktree is on a detached HEAD; there is no branch to rename")?,
    };
    let warnings = worktree::rename_branch(&path, &old, new)?;
    fleet.enqueue(FleetMutation::UpdateTaskBranch {
        id: files.source_node_id.clone(),
        branch: Some(new.to_string()),
    })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;
    Ok(warnings)
}

/// Release the worktree recorded for the node owning `node_id`'s Files capability.
///
/// Drop shells and terminal agents whose processes have exited (closed outside
/// the app), so they don't block a release.
fn prune_stale_sessions(fleet: &FleetStore, paths: &TodPaths) {
    let mut nodes = BTreeSet::new();
    nodes.extend(
        fleet
            .list_all_shells()
            .unwrap_or_default()
            .into_iter()
            .map(|shell| shell.node_id),
    );
    nodes.extend(
        fleet
            .list_unended_runs()
            .unwrap_or_default()
            .into_iter()
            .map(|run| run.node_id),
    );
    for node_id in nodes {
        let _ = prune_stale_shell_sessions(fleet, paths, &node_id);
        let _ = prune_stale_terminal_agent_runs(fleet, paths, &node_id);
    }
    let _ = fleet.reload_if_stale();
}

/// Refused while shells or agents run in the worktree. Treehouse leases are
/// returned; git worktrees are removed only when no other node records the same
/// path (and never the primary checkout). The node's worktree columns are
/// cleared either way.
pub fn release_worktree_for_node(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    node_id: &str,
) -> Result<()> {
    let files = resolve_files(fleet, node_id)?;
    let owner = files.source_node_id.clone();
    let Some(path) = files.worktree_dir() else {
        return Ok(());
    };
    prune_stale_sessions(fleet, paths);
    if let Some(reason) = fleet.worktree_release_blocker(&owner)? {
        bail!("{reason}");
    }
    let has_lease = files
        .worktree_lease_id
        .as_deref()
        .is_some_and(|id| !id.is_empty());
    let exists = path.is_dir();
    if !exists && !has_lease {
        // Folder already gone: just drop git's record of it.
        if let Some(repo) = files.repo_dir() {
            let _ = worktree::prune_git_worktrees(&repo);
        }
    }
    if exists {
        let lease_id = files.worktree_lease_id.as_deref().filter(|s| !s.is_empty());
        if let Some(lease_id) = lease_id {
            worktree::treehouse_return(&path, lease_id, settings, paths)?;
        } else {
            let shared = fleet.read(|conn| {
                crate::fleet::repos::node_files::NodeFilesRepo::new(conn)
                    .other_nodes_using_worktree(&owner, &path.storage())
            })?;
            if shared.is_empty() {
                if let Some(repo) = files.repo_dir() {
                    worktree::remove_git_worktree(&repo, &path)?;
                }
            }
        }
    }
    fleet.enqueue(FleetMutation::UpdateNodeWorktree {
        node_id: owner,
        worktree_path: None,
        worktree_lease_id: None,
        worktree_lease_holder: None,
    })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;
    Ok(())
}
