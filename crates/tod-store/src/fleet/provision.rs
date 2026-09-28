//! Launch directories from the Files capability: each node's own worktree or
//! sandbox, made when the node first needs its files, and removed (its
//! branch pushed first) when the settings it was made from change.

use crate::fleet::FleetStore;
use crate::fleet::node_actions::{FilesDirectory, ResolvedFiles};
use crate::fleet::repos::files_location::{FilesLocation, FilesLocationRepo};
use crate::fleet::sandbox::{self, Sandboxes};
use crate::fleet::terminal::{prune_stale_shell_sessions, prune_stale_terminal_agent_runs};
use crate::fleet::workdir::Workdir;
use crate::fleet::worktree::{self, WorktreeHandle, validate_git_repo};
use crate::fleet::writer::FleetMutation;
use crate::paths::TodPaths;
use crate::settings::TodSettings;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

fn resolve_files(fleet: &FleetStore, node_id: &str) -> Result<ResolvedFiles> {
    fleet.reload_if_stale().ok();
    fleet
        .resolve_files_for_node(node_id)?
        .context("Enable Files and set a workspace directory")
}

/// Directory that shells, editors, and coding agents launched from `node_id`
/// run in: on this machine, inside the dev container the repository lives
/// in, or in the node's cloud sandbox.
///
/// When the node's Files settings give each node a worktree or sandbox of
/// its own and this node's has not been made yet, it is made now (git,
/// Docker, Treehouse, or a sandbox minutes away): call it off the UI thread.
/// [`launch_cwd_if_made`] is the check that never makes anything.
pub fn resolve_launch_cwd(fleet: &FleetStore, node_id: &str) -> Result<Workdir> {
    Ok(resolve_launch_cwd_with(fleet, node_id, &mut |step| tracing::info!("files for {node_id}: {step}"))?.0)
}

/// [`resolve_launch_cwd`], reporting what making the location is doing, and
/// what it did only in part (a submodule left off the branch) for the user.
pub fn resolve_launch_cwd_with(
    fleet: &FleetStore,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<(Workdir, Vec<String>)> {
    let files = resolve_files(fleet, node_id)?;
    match files.directory() {
        FilesDirectory::Ready(dir) => Ok((dir, Vec::new())),
        FilesDirectory::Missing(reason) => bail!("{reason}"),
        FilesDirectory::NotMade => {
            let paths = TodPaths::discover()?;
            let settings = TodSettings::load(&paths)?;
            make_location(fleet, &paths, &settings, node_id, progress)
        }
    }
}

/// Where `node_id` launches, without making anything: `Ok(Some)` when ready,
/// `Ok(None)` when its worktree or sandbox will be made on first launch,
/// `Err` with the user-facing reason it can't launch. Cheap enough for the
/// UI thread when the directory is on this machine.
pub fn launch_cwd_if_made(fleet: &FleetStore, node_id: &str) -> Result<Option<Workdir>> {
    let files = resolve_files(fleet, node_id)?;
    if let Some(stale) = &files.stale_location
        && files.location.is_none()
        && files.per_node()
    {
        bail!("{}", stale_message(&files, stale));
    }
    match files.directory() {
        FilesDirectory::Ready(dir) => Ok(Some(dir)),
        FilesDirectory::NotMade => Ok(None),
        FilesDirectory::Missing(reason) => bail!("{reason}"),
    }
}

fn stale_message(files: &ResolvedFiles, stale: &FilesLocation) -> String {
    format!(
        "This node's {} was made from Files settings that have since changed \
         (now {}'s). Remove it first; its branch is pushed before it goes.",
        stale.describe(),
        files.source_title
    )
}

/// One lock per node, so two launches at once don't both make its location.
fn node_lock(node_id: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(Default::default)
        .lock()
        .expect("files location locks")
        .entry(node_id.to_string())
        .or_default()
        .clone()
}

/// The branch `node_id` works on: its own, else `task/<slug>`, recorded.
fn node_branch(fleet: &FleetStore, files: &ResolvedFiles) -> Result<String> {
    if let Some(branch) = files.branch() {
        return Ok(branch.to_string());
    }
    let node = fleet
        .get_node(&files.node_id)?
        .ok_or_else(|| anyhow!("no node {}", files.node_id))?;
    let branch = format!("task/{}", node.slug);
    fleet.enqueue(FleetMutation::UpdateTaskBranch {
        id: files.node_id.clone(),
        branch: Some(branch.clone()),
    })?;
    fleet.writer().flush()?;
    Ok(branch)
}

/// Make `node_id`'s worktree or sandbox from the Files settings it resolves
/// to, and record it. Slow: never on the UI thread.
pub fn make_location(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<(Workdir, Vec<String>)> {
    let lock = node_lock(node_id);
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Another launch may have made it while this one waited.
    let files = resolve_files(fleet, node_id)?;
    match files.directory() {
        FilesDirectory::Ready(dir) => return Ok((dir, Vec::new())),
        FilesDirectory::Missing(reason) => bail!("{reason}"),
        FilesDirectory::NotMade => {}
    }
    if let Some(stale) = &files.stale_location {
        bail!("{}", stale_message(&files, stale));
    }
    let branch = node_branch(fleet, &files)?;
    let location = FilesLocation {
        node_id: node_id.to_string(),
        source_node_id: files.source_node_id.clone(),
        recipe: files.recipe_key(),
        repo: files.repo.clone(),
        container: files.repo_container().map(str::to_string),
        worktree_path: None,
        worktree_lease_id: None,
        worktree_lease_holder: None,
        sandbox: None,
        created_at: 0,
    };
    let (location, warnings) = if files.runs_in_sandbox() {
        make_sandbox(fleet, &files, location, &branch, progress)?
    } else {
        make_worktree(fleet, paths, settings, &files, location, &branch, progress)?
    };
    let dir = location
        .directory()
        .ok_or_else(|| anyhow!("made {} but found no directory in it", location.describe()))?;
    Ok((dir, warnings))
}

fn record(fleet: &FleetStore, location: &FilesLocation) -> Result<()> {
    fleet.enqueue(FleetMutation::RecordFilesLocation { location: location.clone() })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;
    Ok(())
}

fn make_worktree(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    files: &ResolvedFiles,
    mut location: FilesLocation,
    branch: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<(FilesLocation, Vec<String>)> {
    let repo = files
        .repo_dir()
        .context("Set a workspace directory before a worktree can be made")?;
    let repo_path = validate_git_repo(&repo)?;
    let lease_holder = format!("tod-{}", files.node_id);
    let data_root = settings.resolve_fleet_storage_root(paths)?;
    progress(&format!("making a worktree of {repo_path} on {branch}…"));
    // No store lock is held while git, Docker, or Treehouse run: the UI reads
    // the store on every frame.
    let handle: WorktreeHandle = worktree::ensure_worktree(
        &|repo, branch| fleet.read(|conn| FilesLocationRepo::new(conn).worktree_for(repo, branch)),
        settings.worktree_backend,
        settings,
        paths,
        &data_root,
        &repo_path,
        branch,
        &lease_holder,
    )?;
    location.worktree_path = Some(handle.path.storage());
    location.worktree_lease_id = handle.lease.as_ref().map(|l| l.lease_id.clone());
    location.worktree_lease_holder = handle.lease.as_ref().map(|l| l.lease_holder.clone());
    record(fleet, &location)?;
    if !handle.path.is_dir() {
        bail!("the worktree made for this node is missing at {}", handle.path);
    }
    Ok((location, handle.warnings))
}

/// A name for `slug`'s sandbox that no known sandbox has.
fn free_sandbox_name(fleet: &FleetStore, root: &std::path::Path, slug: &str, skip: usize) -> Result<String> {
    let known = sandbox::known(root);
    let base = sandbox::suggested_name(&format!("tod-{slug}"));
    let mut taken = skip;
    for n in 1.. {
        let name = if n == 1 {
            base.clone()
        } else {
            let suffix = format!("-{n}");
            let stem: String = base.chars().take(48 - suffix.len()).collect();
            format!("{}{suffix}", stem.trim_end_matches('-'))
        };
        let in_use = known.contains(&name) || fleet.read(|conn| FilesLocationRepo::new(conn).sandbox_in_use(&name))?;
        if in_use {
            continue;
        }
        if taken == 0 {
            return Ok(name);
        }
        taken -= 1;
    }
    unreachable!()
}

fn make_sandbox(
    fleet: &FleetStore,
    files: &ResolvedFiles,
    mut location: FilesLocation,
    branch: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<(FilesLocation, Vec<String>)> {
    let dev = files.dev_container.clone().unwrap_or_default();
    let root = fleet.paths().root().to_path_buf();
    let node = fleet
        .get_node(&files.node_id)?
        .ok_or_else(|| anyhow!("no node {}", files.node_id))?;
    let mut sandboxes = Sandboxes::load(&root)?;
    let source = dev.sandbox_from.source();
    let mut attempt = 0;
    let name = loop {
        let name = free_sandbox_name(fleet, &root, &node.slug, attempt)?;
        progress(&format!("making sandbox {name} from {}…", dev.sandbox_from.describe()));
        match sandboxes.create(&name, &source, true, false, progress) {
            Ok(_) => break name,
            // Blaxel has one by that name that this data root doesn't know.
            Err(err) if attempt < 5 && format!("{err:#}").contains("already exists") => attempt += 1,
            Err(err) => {
                return Err(err.context(format!(
                    "make sandbox {name} for this node (if it was made in part, \
                     `tod-sandbox delete {name}` removes it)"
                )));
            }
        }
    };
    location.sandbox = Some(name.clone());
    // Recorded before anything else can fail, so the sandbox is never lost track of.
    record(fleet, &location)?;
    let repo = files.repo().unwrap_or_default();
    let dir = Workdir::sandbox(&name, repo);
    dir.git(&["rev-parse", "--show-toplevel"]).with_context(|| {
        format!(
            "sandbox {name} has no git repository at {repo}: the image or the forked \
             sandbox must hold it there"
        )
    })?;
    progress(&format!("fetching origin in {name}…"));
    worktree::fetch_origin(&dir)?;
    progress(&format!("checking out {branch} in {name}…"));
    worktree::checkout_branch(&dir, branch)?;
    let mut warnings = Vec::new();
    if let Err(err) = worktree::init_submodules(&dir) {
        warnings.push(format!("{err:#}"));
    }
    warnings.extend(worktree::branch_submodules(&dir, branch)?);
    Ok((location, warnings))
}

/// Change `node_id`'s branch while it has a worktree or sandbox: the branch
/// is renamed in place there, in the superproject and every submodule that
/// has it (see [`worktree::rename_branch`]), then recorded on the node.
/// Returns warnings the user should see (a renamed branch that was already
/// pushed).
pub fn rename_branch_for_node(fleet: &FleetStore, node_id: &str, new: &str) -> Result<Vec<String>> {
    let files = resolve_files(fleet, node_id)?;
    let path = files
        .location
        .as_ref()
        .and_then(FilesLocation::directory)
        .context("This node's files have not been made yet")?;
    if new.is_empty() {
        bail!("A node with its own files needs a branch");
    }
    let old = match files.branch().filter(|b| !b.is_empty()) {
        Some(branch) => branch.to_string(),
        None => worktree::current_branch(&path)
            .context("The worktree is on a detached HEAD; there is no branch to rename")?,
    };
    let warnings = worktree::rename_branch(&path, &old, new)?;
    fleet.enqueue(FleetMutation::UpdateTaskBranch {
        id: node_id.to_string(),
        branch: Some(new.to_string()),
    })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;
    Ok(warnings)
}

/// Drop shells and terminal agents whose processes have exited (closed outside
/// the app), so they don't block a removal.
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

/// Whether a node's location can be removed now, as far as its files go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationState {
    /// Nothing uncommitted: removing it pushes the branch and deletes it.
    Clean,
    /// Uncommitted changes (`git status --porcelain` lines).
    Dirty(Vec<String>),
    /// Something still runs in it (user-facing).
    Busy(String),
    /// It is already gone; removing it only forgets it.
    Gone,
}

/// `node_id`'s location, current or stale.
pub fn node_location(fleet: &FleetStore, node_id: &str) -> Result<Option<FilesLocation>> {
    fleet.reload_if_stale().ok();
    fleet.read(|conn| FilesLocationRepo::new(conn).get(node_id))
}

fn location_dir(location: &FilesLocation) -> Result<Workdir> {
    location
        .directory()
        .ok_or_else(|| anyhow!("{} has no directory recorded", location.describe()))
}

/// Check `node_id`'s location before removing it: git and maybe the
/// network, so never on the UI thread.
pub fn location_state(fleet: &FleetStore, paths: &TodPaths, node_id: &str) -> Result<LocationState> {
    let location = node_location(fleet, node_id)?.context("This node has no files of its own")?;
    prune_stale_sessions(fleet, paths);
    if let Some(reason) = fleet.location_blocker(node_id)? {
        return Ok(LocationState::Busy(reason));
    }
    let dir = location_dir(&location)?;
    if !dir.is_dir() {
        return Ok(LocationState::Gone);
    }
    // A submodule's recorded commit is not work worth keeping (it moves
    // whenever the submodule does), so the parent ignores submodules
    // altogether and each one is checked for its own uncommitted files.
    let status = dir.git(&["status", "--porcelain", "--ignore-submodules=all"])?;
    let mut lines: Vec<String> = status.lines().map(str::to_string).filter(|l| !l.trim().is_empty()).collect();
    lines.extend(submodule_dirty_lines(&dir)?);
    Ok(if lines.is_empty() { LocationState::Clean } else { LocationState::Dirty(lines) })
}

/// Uncommitted files inside every initialized submodule (recursively), as
/// `git status --porcelain` lines with the submodule's path prepended.
/// Which commit a submodule is on is not reported.
fn submodule_dirty_lines(dir: &Workdir) -> Result<Vec<String>> {
    const SCRIPT: &str = r#"git status --porcelain --ignore-submodules=all | while IFS= read -r l; do rest=${l#???}; printf '%s%s/%s\n' "${l%"$rest"}" "$displaypath" "$rest"; done"#;
    let out = dir.git(&["submodule", "foreach", "--quiet", "--recursive", SCRIPT])?;
    Ok(out.lines().map(str::to_string).filter(|l| !l.trim().is_empty()).collect())
}

/// Every repository in `dir`'s checkout, submodules (recursively) first and
/// `dir` itself last, so a change in a submodule is dealt with before the
/// parent that records it.
fn repos_deepest_first(dir: &Workdir) -> Result<Vec<Workdir>> {
    let mut repos = worktree::submodule_dirs(dir)?;
    repos.reverse();
    repos.push(dir.clone());
    Ok(repos)
}

/// Commit everything uncommitted in `node_id`'s location on its branch, and
/// in each of its submodules on theirs, so removing it loses nothing. Which
/// commit a submodule is on is not recorded in the parent (it is not work
/// worth keeping, and the check for uncommitted changes ignores it).
pub fn commit_location_changes(fleet: &FleetStore, node_id: &str) -> Result<()> {
    let location = node_location(fleet, node_id)?.context("This node has no files of its own")?;
    let dir = location_dir(&location)?;
    for repo in repos_deepest_first(&dir)? {
        repo.git(&["add", "-A"])?;
        // Unstage any submodule pointer `add -A` picked up.
        let staged = repo.git(&["diff", "--cached", "--raw", "--no-renames"])?;
        for line in staged.lines() {
            let Some((meta, path)) = line.split_once('\t') else { continue };
            let mut modes = meta.trim_start_matches(':').split(' ');
            if modes.next() == Some("160000") || modes.next() == Some("160000") {
                repo.git(&["reset", "-q", "--", path])?;
            }
        }
        if repo.git(&["diff", "--cached", "--name-only"])?.is_empty() {
            continue;
        }
        repo.git(&["commit", "--no-verify", "-m", "tod: save work before this node's files were removed"])?;
    }
    Ok(())
}

/// Throw away everything uncommitted in `node_id`'s location and in each of
/// its submodules.
pub fn discard_location_changes(fleet: &FleetStore, node_id: &str) -> Result<()> {
    let location = node_location(fleet, node_id)?.context("This node has no files of its own")?;
    let dir = location_dir(&location)?;
    for repo in repos_deepest_first(&dir)? {
        repo.git(&["reset", "--hard", "-q"])?;
        repo.git(&["clean", "-fdq"])?;
    }
    Ok(())
}

/// Push the branch checked out in `dir` to `origin`, setting it upstream.
fn push_branch(dir: &Workdir) -> Result<()> {
    let branch = worktree::current_branch(dir)
        .context("its checkout is on a detached HEAD, so there is no branch to push")?;
    push_named_branch(dir, &branch)
}

fn push_named_branch(dir: &Workdir, branch: &str) -> Result<()> {
    let remote = dir.git(&["remote"])?;
    if !remote.lines().any(|r| r.trim() == "origin") {
        bail!("it has no origin remote to push {branch} to");
    }
    dir.git(&["push", "--quiet", "-u", "origin", &format!("HEAD:refs/heads/{branch}")])
        .with_context(|| format!("push {branch}"))?;
    Ok(())
}

/// Push the branch of `dir` and of each submodule that is on one (a
/// submodule left on a detached HEAD has nothing of its own to push), the
/// submodules first.
fn push_branches(dir: &Workdir) -> Result<()> {
    for repo in repos_deepest_first(dir)? {
        if repo == *dir {
            push_branch(&repo)?;
        } else if let Some(branch) = worktree::current_branch(&repo) {
            push_named_branch(&repo, &branch)
                .with_context(|| format!("submodule {repo}"))?;
        }
    }
    Ok(())
}

/// Remove `node_id`'s location: refused while something runs in it or it
/// has uncommitted changes; otherwise its branch is pushed (a failed push
/// keeps it) and the worktree or sandbox is deleted and forgotten. Slow:
/// never on the UI thread.
pub fn remove_location(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    node_id: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<()> {
    let lock = node_lock(node_id);
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(location) = node_location(fleet, node_id)? else {
        return Ok(());
    };
    match location_state(fleet, paths, node_id)? {
        LocationState::Busy(reason) => bail!("{reason}"),
        LocationState::Dirty(lines) => bail!(
            "{} has {} uncommitted change(s): commit or discard them first",
            location.describe(),
            lines.len()
        ),
        LocationState::Gone => {
            if location.sandbox().is_none()
                && let Some(repo) = location.repo_dir()
            {
                let _ = worktree::prune_git_worktrees(&repo);
            }
        }
        LocationState::Clean => {
            let dir = location_dir(&location)?;
            progress(&format!("pushing the branch in {}…", location.describe()));
            push_branches(&dir).with_context(|| format!("{} was kept", location.describe()))?;
            progress(&format!("removing {}…", location.describe()));
            if let Some(name) = location.sandbox() {
                let mut sandboxes = Sandboxes::load(fleet.paths().root())?;
                let bx = sandboxes.blaxel()?;
                sandboxes.delete(&bx, name).with_context(|| format!("delete sandbox {name}"))?;
            } else if let Some(lease_id) = location.lease_id() {
                worktree::treehouse_return(&dir, lease_id, settings, paths)?;
            } else {
                let shared = fleet.read(|conn| {
                    FilesLocationRepo::new(conn).others_using_worktree(node_id, &dir.storage())
                })?;
                if shared.is_empty()
                    && let Some(repo) = location.repo_dir()
                {
                    worktree::remove_git_worktree(&repo, &dir)?;
                }
            }
        }
    }
    fleet.enqueue(FleetMutation::DeleteFilesLocation { node_id: node_id.to_string() })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;
    Ok(())
}

/// A location a change would remove, for the confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffectedLocation {
    pub node_id: String,
    pub node_title: String,
    pub location: FilesLocation,
}

/// The locations a change to the Files settings on `node_id` would leave
/// made from settings that no longer apply: every one made from its
/// settings, plus any at or below it made from an ancestor's (which turning
/// Files on here overrides). One made from Files set further down is not
/// affected. Includes ones already stale.
pub fn locations_affected_by(fleet: &FleetStore, node_id: &str) -> Result<Vec<AffectedLocation>> {
    let node = uuid::Uuid::parse_str(node_id)?;
    locations_where(fleet, |location, chain| {
        location.source_node_id == node_id || {
            let at = chain.iter().position(|id| *id == node);
            let source = uuid::Uuid::parse_str(&location.source_node_id).ok();
            let from = source.and_then(|s| chain.iter().position(|id| *id == s));
            matches!((at, from), (Some(at), Some(from)) if from < at)
        }
    })
}

/// Every location at or below `node_id`, which deleting it would leave
/// behind.
pub fn locations_in_subtree(fleet: &FleetStore, node_id: &str) -> Result<Vec<AffectedLocation>> {
    let node = uuid::Uuid::parse_str(node_id)?;
    locations_where(fleet, |_, chain| chain.contains(&node))
}

/// The locations `keep` accepts, given each one's node's ancestor chain
/// (root → leaf, both ends included).
fn locations_where(
    fleet: &FleetStore,
    keep: impl Fn(&FilesLocation, &[uuid::Uuid]) -> bool,
) -> Result<Vec<AffectedLocation>> {
    fleet.reload_if_stale().ok();
    fleet.read(|conn| {
        let all = FilesLocationRepo::new(conn).list_all()?;
        let mut out = Vec::new();
        for location in all {
            let target = uuid::Uuid::parse_str(&location.node_id)?;
            let chain = crate::outline::ancestor_chain(conn, target)?;
            if !keep(&location, &chain) {
                continue;
            }
            let node_title = crate::outline::repos::NodeRepo::new(conn)
                .get(target)?
                .map(|n| n.title)
                .unwrap_or_default();
            out.push(AffectedLocation { node_id: location.node_id.clone(), node_title, location });
        }
        Ok(out)
    })
}

/// Every node's location that is stale: made from settings that changed,
/// or no longer resolved to.
pub fn stale_locations(fleet: &FleetStore) -> Result<Vec<AffectedLocation>> {
    fleet.reload_if_stale().ok();
    let all = fleet.read(|conn| FilesLocationRepo::new(conn).list_all())?;
    let mut out = Vec::new();
    for location in all {
        let files = fleet.resolve_files_for_node(&location.node_id)?;
        if files.as_ref().is_some_and(|f| f.location.is_some()) {
            continue;
        }
        let node_title = fleet.get_node(&location.node_id)?.map(|n| n.title).unwrap_or_default();
        out.push(AffectedLocation { node_id: location.node_id.clone(), node_title, location });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::test_util::{cleanup_fleet_root, temp_fleet_root};
    use crate::outline::types::Capability;
    use crate::outline::{CreatePosition, OutlineMutation};
    use crate::settings::WorktreeBackend;
    use std::path::Path;
    use std::process::Command;
    use uuid::Uuid;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repository with a bare `origin`, one commit on main.
    fn repo_with_origin(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let origin = root.join("origin.git");
        let repo = root.join("repo");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&repo, &["remote", "add", "origin", &origin.to_string_lossy()]);
        git(&repo, &["push", "-q", "-u", "origin", "main"]);
        (repo, origin)
    }

    /// A parent with Files (a worktree per node) and a child that inherits it.
    fn tree(store: &FleetStore, repo: &Path) -> (String, String) {
        store
            .enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })
            .unwrap();
        store.writer().flush().unwrap();
        let list_id = store.list_outline_lists().unwrap()[0].id;
        let (parent, child) = (Uuid::new_v4(), Uuid::new_v4());
        for (id, under, title) in [(parent, None, "Tasks"), (child, Some(parent), "Fix login")] {
            store
                .enqueue_outline(OutlineMutation::CreateNode {
                    node_id: Some(id),
                    list_id,
                    parent_id: under,
                    anchor_id: under,
                    position: if under.is_some() { CreatePosition::Child } else { CreatePosition::Below },
                    title: title.into(),
                })
                .unwrap();
            store.writer().flush().unwrap();
        }
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: parent,
                capabilities: vec![Capability::Files],
            })
            .unwrap();
        let parent = parent.to_string();
        store
            .enqueue(FleetMutation::UpdateTaskRepo {
                id: parent.clone(),
                repo: Some(repo.to_string_lossy().into_owned()),
            })
            .unwrap();
        store
            .enqueue(FleetMutation::SetNodeUseWorktree { node_id: parent.clone(), use_worktree: true })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().unwrap();
        (parent, child.to_string())
    }

    /// An inheriting node gets a worktree of its own on its own branch the
    /// first time it needs one; removing it refuses uncommitted work until
    /// it is committed, then pushes the branch and deletes the worktree.
    #[test]
    fn a_node_gets_its_own_worktree_and_removing_it_pushes_first() {
        let root = temp_fleet_root();
        let (repo, origin) = repo_with_origin(&root);
        crate::paths::set_data_root(root.join("data"));
        let paths = TodPaths::discover().unwrap();
        let settings = TodSettings {
            worktree_backend: WorktreeBackend::GitOnly,
            fleet_storage_root: Some(root.join("data")),
            ..TodSettings::default()
        };
        let store = FleetStore::open(root.join("fleet")).unwrap();
        let (parent, child) = tree(&store, &repo);
        // Someone else pushes to main; this checkout has not fetched it.
        let other = root.join("other");
        git(&root, &["clone", "-q", &origin.to_string_lossy(), &other.to_string_lossy()]);
        git(&other, &["-c", "user.email=o@example.com", "-c", "user.name=o", "commit", "-q", "--allow-empty", "-m", "newer"]);
        git(&other, &["push", "-q", "origin", "main"]);
        let latest = git(&origin, &["rev-parse", "main"]);
        assert_ne!(git(&repo, &["rev-parse", "main"]), latest);

        let (dir, _) = make_location(&store, &paths, &settings, &child, &mut |_| {}).unwrap();
        assert!(dir.is_dir());
        let slug = store.get_node(&child).unwrap().unwrap().slug;
        let branch = format!("task/{slug}");
        assert_eq!(worktree::current_branch(&dir).as_deref(), Some(branch.as_str()));
        // The new branch starts from the latest of origin, not the stale main.
        assert_eq!(dir.git(&["rev-parse", "HEAD"]).unwrap(), latest);
        let files = store.resolve_files_for_node(&child).unwrap().unwrap();
        assert_eq!(files.branch(), Some(branch.as_str()));
        assert_eq!(files.ready_directory(), Some(dir.clone()));
        // Making it again is the same one; the parent has none of its own.
        assert_eq!(make_location(&store, &paths, &settings, &child, &mut |_| {}).unwrap().0, dir);
        assert_eq!(store.resolve_files_for_node(&parent).unwrap().unwrap().directory(), FilesDirectory::NotMade);

        let affected = locations_affected_by(&store, &parent).unwrap();
        assert_eq!(affected.iter().map(|a| a.node_title.as_str()).collect::<Vec<_>>(), ["Fix login"]);
        // Deleting either node would leave the child's worktree behind.
        for node in [&parent, &child] {
            let below = locations_in_subtree(&store, node).unwrap();
            assert_eq!(below.iter().map(|a| a.node_title.as_str()).collect::<Vec<_>>(), ["Fix login"]);
        }

        std::fs::write(dir.host_path().unwrap().join("work.txt"), "done").unwrap();
        assert!(matches!(location_state(&store, &paths, &child).unwrap(), LocationState::Dirty(lines) if lines.len() == 1));
        assert!(remove_location(&store, &paths, &settings, &child, &mut |_| {}).is_err());
        let dir_path = dir.host_path().unwrap().to_path_buf();
        git(&dir_path, &["config", "user.email", "t@example.com"]);
        git(&dir_path, &["config", "user.name", "t"]);
        commit_location_changes(&store, &child).unwrap();
        assert_eq!(location_state(&store, &paths, &child).unwrap(), LocationState::Clean);

        remove_location(&store, &paths, &settings, &child, &mut |_| {}).unwrap();
        assert!(!dir_path.exists());
        assert!(node_location(&store, &child).unwrap().is_none());
        let pushed = git(&origin, &["log", "--format=%s", &branch]);
        assert!(pushed.starts_with("tod: save work"), "{pushed}");

        drop(store);
        crate::paths::clear_data_root_override();
        cleanup_fleet_root(&root);
    }
}
