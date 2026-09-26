//! Git worktree and Treehouse provisioning for agent workspaces.
//!
//! Every function works on a [`Workdir`]: git runs on this machine for a
//! repository here, and inside the dev container (via `docker exec`) for one
//! that lives there. So does Treehouse: in a container it is the `treehouse`
//! on the container's `PATH`, with the container's own configuration.

use crate::fleet::treehouse::{TREEHOUSE_NO_UPDATE_CHECK_ENV, TreehouseInvocation};
use crate::fleet::workdir::{CONTAINER_GIT_CONFIG_ENV, Workdir, strip_verbatim};
use crate::paths::TodPaths;
use crate::settings::{TodSettings, WorktreeBackend};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct TreehouseLease {
    pub lease_id: String,
    pub lease_holder: String,
}

#[derive(Debug, Clone)]
pub struct WorktreeHandle {
    pub path: Workdir,
    pub lease: Option<TreehouseLease>,
    /// What set up only partly (a submodule left off the branch), for the user.
    pub warnings: Vec<String>,
}

/// Returns true when the `treehouse` CLI is on PATH and responds.
pub use crate::fleet::treehouse::treehouse_available;

fn canonical_repo_key(repo: &Path) -> Result<String> {
    let canonical = repo
        .canonicalize()
        .with_context(|| format!("resolve repo path {}", repo.display()))?;
    Ok(canonical.to_string_lossy().into_owned())
}

fn branch_slug(branch: &str) -> String {
    let slug: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if slug.is_empty() {
        "default".into()
    } else {
        slug
    }
}

/// Where the worktree for `repo` + `branch` goes on this machine: under the
/// data root, keyed by the repository.
pub fn worktree_dest(data_root: &Path, repo: &Path, branch: &str) -> Result<PathBuf> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let key = canonical_repo_key(repo)?;
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());
    Ok(data_root
        .join("worktrees")
        .join(hash)
        .join(branch_slug(branch)))
}

/// Worktrees of a repository inside a dev container live in the repository,
/// under this directory, which is kept out of `git status` through the
/// repository's `info/exclude`. The data root is on this machine, and a
/// directory beside the repository may not outlive the container.
pub const CONTAINER_WORKTREES_DIR: &str = ".worktrees";

/// Where the worktree for `branch` of `repo` goes: see [`worktree_dest`] and
/// [`CONTAINER_WORKTREES_DIR`].
pub fn worktree_dest_for(data_root: &Path, repo: &Workdir, branch: &str) -> Result<Workdir> {
    match repo {
        Workdir::Host(path) => Ok(Workdir::Host(worktree_dest(data_root, path, branch)?)),
        Workdir::Container { .. } | Workdir::Sandbox { .. } => Ok(repo
            .join(CONTAINER_WORKTREES_DIR)
            .join(&branch_slug(branch))),
    }
}

/// Keep [`CONTAINER_WORKTREES_DIR`] out of `repo`'s `git status`.
fn exclude_container_worktrees(repo: &Workdir) -> Result<()> {
    let exclude = repo.git(&["rev-parse", "--git-path", "info/exclude"])?;
    let entry = format!("/{CONTAINER_WORKTREES_DIR}/");
    let script = r#"grep -qxF "$2" "$1" 2>/dev/null || { mkdir -p "$(dirname "$1")" && printf '%s\n' "$2" >> "$1"; }"#;
    let out = repo.output("sh", &["-c", script, "sh", &exclude, &entry])?;
    if !out.status.success() {
        bail!(
            "add {entry} to {exclude}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn run_git(repo: &Workdir, args: &[&str]) -> Result<String> {
    repo.git(args)
}

/// Returns the worktree where `branch` is checked out, if any.
fn worktree_holding_branch(repo: &Workdir, branch: &str) -> Result<Option<Workdir>> {
    let output = run_git(repo, &["worktree", "list", "--porcelain"])?;
    let branch_ref = format!("refs/heads/{branch}");
    let mut current_worktree: Option<Workdir> = None;
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_worktree = Some(repo.at(path));
        } else if let Some(b) = line.strip_prefix("branch ") {
            if b == branch_ref {
                return Ok(current_worktree);
            }
        }
    }
    Ok(None)
}

pub fn resolve_default_branch(repo: &Workdir) -> Result<String> {
    if let Ok(name) = run_git(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        if let Some(branch) = name.strip_prefix("origin/") {
            if !branch.is_empty() {
                return Ok(branch.to_string());
            }
        }
    }
    for candidate in ["main", "master"] {
        if run_git(repo, &["rev-parse", "--verify", candidate]).is_ok() {
            return Ok(candidate.to_string());
        }
    }
    run_git(repo, &["branch", "--show-current"]).or_else(|_| Ok("main".into()))
}

pub fn checkout_branch(worktree: &Workdir, branch: &str) -> Result<()> {
    if branch.is_empty() {
        return Ok(());
    }
    switch_to_branch(worktree, branch).map(|_| ())
}

/// The remote a branch of the same name is looked for on.
const REMOTE: &str = "origin";

/// What a repository has of `branch`, from one git call.
#[derive(Debug, Default)]
struct BranchState {
    /// Checked out.
    current: bool,
    local: bool,
    /// The local branch has an upstream.
    upstream: bool,
    /// `origin/<branch>` is known, as of the last fetch.
    remote: bool,
}

fn branch_state(repo: &Workdir, branch: &str) -> Result<BranchState> {
    let local = format!("refs/heads/{branch}");
    let remote = format!("refs/remotes/{REMOTE}/{branch}");
    // `%(HEAD)` last: it is a space when not checked out, and output is trimmed.
    let out = run_git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(upstream)%00%(HEAD)",
            &local,
            &remote,
        ],
    )?;
    let mut state = BranchState::default();
    for line in out.lines() {
        let mut fields = line.split('\0');
        let (name, upstream, head) = (
            fields.next().unwrap_or_default(),
            fields.next().unwrap_or_default(),
            fields.next().unwrap_or_default(),
        );
        if name == local {
            state.local = true;
            state.upstream = !upstream.is_empty();
            state.current = head.trim() == "*";
        } else if name == remote {
            state.remote = true;
        }
    }
    Ok(state)
}

/// Check `branch` out in `repo`. One that does not exist yet is created from
/// `origin/<branch>` when there is one (work pushed before, a pull request),
/// else at HEAD. A branch with no upstream is linked to `origin/<branch>`, so
/// `git pull` and `git push` work. Nothing is fetched: this goes by what the
/// last fetch saw. Returns whether the branch was created at HEAD.
fn switch_to_branch(repo: &Workdir, branch: &str) -> Result<bool> {
    let state = branch_state(repo, branch)?;
    let tracking = format!("{REMOTE}/{branch}");
    if !state.local {
        if state.remote {
            run_git(repo, &["switch", "--track", "-c", branch, &tracking])?;
            return Ok(false);
        }
        run_git(repo, &["switch", "-c", branch])?;
        return Ok(true);
    }
    if !state.current {
        run_git(repo, &["switch", branch])?;
    }
    if state.remote && !state.upstream {
        let upstream = format!("--set-upstream-to={tracking}");
        if let Err(err) = run_git(repo, &["branch", &upstream, branch]) {
            tracing::warn!("link {branch} in {repo} to {tracking}: {err:#}");
        }
    }
    Ok(false)
}

/// Initialized submodules of `repo`, recursively, parents before children.
/// Empty for a repository without submodules.
pub fn submodule_dirs(repo: &Workdir) -> Result<Vec<Workdir>> {
    Ok(submodule_paths(repo)?
        .iter()
        .map(|rel| repo.join(rel))
        .collect())
}

/// [`submodule_dirs`], as paths relative to `repo` (`lib/a`, `lib/a/b`).
pub fn submodule_paths(repo: &Workdir) -> Result<Vec<String>> {
    // `git submodule status --recursive` takes most of a second even with
    // nothing to report, and every run start and commit asks.
    if !has_gitmodules(repo) {
        return Ok(Vec::new());
    }
    let output = run_git(repo, &["submodule", "status", "--recursive"])?;
    Ok(output
        .lines()
        .filter_map(parse_submodule_status)
        .map(str::to_string)
        .collect())
}

/// The path from one `git submodule status` line, or `None` for a submodule
/// that is not initialized (`-`). A line is `<flag><sha> <path>[ (<describe>)]`.
fn parse_submodule_status(line: &str) -> Option<&str> {
    let mut chars = line.chars();
    let flag = chars.next()?;
    if flag == '-' {
        return None;
    }
    let (_sha, rest) = chars.as_str().split_once(' ')?;
    let path = match rest.rsplit_once(" (") {
        Some((path, describe)) if describe.ends_with(')') => path,
        _ => rest,
    };
    Some(path).filter(|p| !p.is_empty())
}

/// The branch checked out in `repo`, or `None` on a detached HEAD.
pub fn current_branch(repo: &Workdir) -> Option<String> {
    run_git(repo, &["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .filter(|b| !b.is_empty())
}

fn branch_exists(repo: &Workdir, branch: &str) -> bool {
    run_git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// Where a branch this app created in a submodule started, kept in the
/// submodule's own config so [`prune_submodule_branches`] can tell a branch
/// that was never used. `git branch -m` / `-D` carry and drop it with the branch.
fn start_key(branch: &str) -> String {
    format!("branch.{branch}.todstart")
}

/// Initialize the submodules of `worktree` that are not yet, recursively.
/// Leaves initialized ones as they are, so a worktree in use keeps its
/// submodules' branches.
pub fn init_submodules(worktree: &Workdir) -> Result<()> {
    if !has_gitmodules(worktree) {
        return Ok(());
    }
    // Repeat per level: a submodule's own submodules only show once it is.
    for _ in 0..8 {
        let status = run_git(worktree, &["submodule", "status", "--recursive"])?;
        let initialized: Vec<&str> = status.lines().filter_map(parse_submodule_status).collect();
        let missing: Vec<&str> = status
            .lines()
            .filter(|line| line.starts_with('-'))
            .filter_map(|line| line[1..].split_once(' ').map(|(_, path)| path))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        for path in missing {
            // A nested submodule is set up from inside its parent.
            let parent = initialized
                .iter()
                .filter(|parent| path.starts_with(&format!("{parent}/")))
                .max_by_key(|parent| parent.len());
            let (dir, rel) = match parent {
                Some(parent) => (worktree.join(parent), &path[parent.len() + 1..]),
                None => (worktree.clone(), path),
            };
            run_git(&dir, &["submodule", "update", "--init", "--", rel])
                .with_context(|| format!("set up submodule {path} in {worktree}"))?;
        }
    }
    Ok(())
}

/// Check `branch` out in every initialized submodule of `worktree` (see
/// [`switch_to_branch`]: from `origin/<branch>` if the submodule has one, else
/// at its current commit), so an agent's commits inside a submodule land on a
/// branch rather than a detached HEAD. Idempotent. Returns one warning per
/// submodule it could not switch.
pub fn branch_submodules(worktree: &Workdir, branch: &str) -> Result<Vec<String>> {
    if branch.is_empty() {
        return Ok(Vec::new());
    }
    let mut warnings = Vec::new();
    for dir in submodule_dirs(worktree)? {
        let switched = switch_to_branch(&dir, branch).and_then(|created_at_head| {
            if created_at_head {
                let head = run_git(&dir, &["rev-parse", "HEAD"])?;
                run_git(&dir, &["config", &start_key(branch), &head])?;
            }
            Ok(())
        });
        if let Err(err) = switched {
            warnings.push(format!("Submodule {dir} is not on \"{branch}\": {err:#}"));
        }
    }
    Ok(warnings)
}

/// Undo [`branch_submodules`] where it was not needed: a submodule still on
/// the `branch` this app created, at the commit it started from, with nothing
/// uncommitted, goes back to a detached HEAD and the branch is deleted.
pub fn prune_submodule_branches(worktree: &Workdir, branch: &str) -> Result<()> {
    if branch.is_empty() {
        return Ok(());
    }
    // Children first, so a parent's status no longer sees them as changed.
    for dir in submodule_dirs(worktree)?.into_iter().rev() {
        if current_branch(&dir).as_deref() != Some(branch) {
            continue;
        }
        let Ok(start) = run_git(&dir, &["config", "--get", &start_key(branch)]) else {
            continue;
        };
        let unused = run_git(&dir, &["rev-parse", "HEAD"]).is_ok_and(|head| head == start)
            && run_git(&dir, &["status", "--porcelain"]).is_ok_and(|s| s.is_empty());
        if unused {
            run_git(&dir, &["switch", "--detach", "-q"])?;
            run_git(&dir, &["branch", "-D", branch])?;
        }
    }
    Ok(())
}

/// Rename `old` to `new` in `worktree` and in every submodule that has it.
///
/// Commits and uncommitted changes come along; the worktree stays where it is.
/// Refused when `new` already exists in any of those repositories. Submodules
/// are renamed before the superproject, and a failure part-way renames the
/// ones already done back. Returns a warning per repository whose `old` had
/// an upstream: the remote branch (and any pull request) keeps the old name.
pub fn rename_branch(worktree: &Workdir, old: &str, new: &str) -> Result<Vec<String>> {
    if old.is_empty() || new.is_empty() {
        bail!("A branch name cannot be empty");
    }
    if old == new {
        return Ok(Vec::new());
    }
    run_git(worktree, &["check-ref-format", "--branch", new])
        .with_context(|| format!("\"{new}\" is not a valid branch name"))?;
    let mut repos = submodule_dirs(worktree)?;
    repos.reverse();
    repos.push(worktree.clone());
    let holders: Vec<Workdir> = repos
        .into_iter()
        .filter(|repo| branch_exists(repo, old))
        .collect();
    for repo in &holders {
        if branch_exists(repo, new) {
            bail!("Branch \"{new}\" already exists in {repo}");
        }
    }
    let mut warnings = Vec::new();
    for repo in &holders {
        if let Ok(upstream) = run_git(
            repo,
            &["rev-parse", "--abbrev-ref", &format!("{old}@{{upstream}}")],
        ) {
            warnings.push(format!(
                "{repo}: \"{old}\" was pushed as {upstream}; the remote keeps the old name"
            ));
        }
    }
    for (done, repo) in holders.iter().enumerate() {
        if let Err(err) = run_git(repo, &["branch", "-m", old, new]) {
            for renamed in &holders[..done] {
                let _ = run_git(renamed, &["branch", "-m", new, old]);
            }
            return Err(err);
        }
    }
    Ok(warnings)
}

/// The path git is given for `dir`.
fn git_path_arg(dir: &Workdir) -> Result<String> {
    Ok(match dir {
        Workdir::Host(path) => strip_verbatim(path)
            .to_str()
            .context("worktree path utf8")?
            .to_string(),
        Workdir::Container { path, .. } | Workdir::Sandbox { path, .. } => path.clone(),
    })
}

fn git_worktree_add(repo: &Workdir, dest: &Workdir, branch: &str) -> Result<Workdir> {
    if dest.is_dir() {
        return Ok(dest.clone());
    }
    let branch_ref = if branch.is_empty() {
        resolve_default_branch(repo)?
    } else {
        branch.to_string()
    };

    // Branch already checked out (primary repo or an existing worktree) — reuse it.
    if let Some(existing) = worktree_holding_branch(repo, &branch_ref)? {
        return Ok(existing);
    }

    if let Some(parent) = dest.parent() {
        parent
            .create_dir_all()
            .with_context(|| format!("create worktree parent {parent}"))?;
    }
    if repo.is_remote() {
        exclude_container_worktrees(repo)?;
    }
    let dest_str = git_path_arg(dest)?;
    let state = branch_state(repo, &branch_ref)?;
    if state.local {
        run_git(repo, &["worktree", "add", &dest_str, &branch_ref])?;
    } else if state.remote {
        let tracking = format!("{REMOTE}/{branch_ref}");
        run_git(
            repo,
            &["worktree", "add", "--track", "-b", &branch_ref, &dest_str, &tracking],
        )?;
    } else {
        run_git(repo, &["worktree", "add", "-b", &branch_ref, &dest_str])?;
    }
    // Only a new worktree: in one already in use this would put every
    // submodule back on a detached HEAD.
    if has_gitmodules(dest)
        && let Err(err) = run_git(dest, &["submodule", "update", "--init", "--recursive"])
    {
        // Leave nothing behind, so trying again starts clean.
        let _ = remove_git_worktree(repo, dest);
        return Err(err.context(format!("set up the submodules in {dest}")));
    }
    Ok(dest.clone())
}

/// Validate that an interview workspace can be provisioned for `repo` + `branch`.
pub fn validate_interview_workspace(repo: &Path, branch: &str) -> Result<()> {
    let canonical = validate_git_repo(&Workdir::host(repo))?;
    let branch_key = if branch.is_empty() {
        resolve_default_branch(&canonical)?
    } else {
        branch.to_string()
    };
    // Branch already checked out somewhere — provisioning reuses that directory.
    if worktree_holding_branch(&canonical, &branch_key)?.is_some() {
        return Ok(());
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct TreehouseLeaseJson {
    path: String,
    lease_id: String,
    lease_holder: String,
}

/// Whether Treehouse can serve `repo`: the configured executable here, or a
/// `treehouse` in the repository's dev container or sandbox.
fn treehouse_available_for(repo: &Workdir, settings: &TodSettings) -> bool {
    match repo {
        Workdir::Host(_) => treehouse_available(settings),
        Workdir::Container { .. } | Workdir::Sandbox { .. } => {
            matches!(remote_treehouse(repo), Ok(Some(_)))
        }
    }
}

/// The `treehouse` where `dir` is (a container or sandbox): the one on its
/// `PATH`, which sets up its own environment there. Tod's settings for
/// Treehouse name paths on this machine, so none of them apply.
fn remote_treehouse(dir: &Workdir) -> Result<Option<String>> {
    dir.find_remote_program("treehouse")
}

/// Run Treehouse with `args` in `dir`, which is in a dev container or sandbox.
fn run_container_treehouse(dir: &Workdir, args: &[&str]) -> Result<std::process::Output> {
    if !dir.is_remote() {
        bail!("not a container or sandbox directory: {dir}");
    }
    let program = remote_treehouse(dir)?.with_context(|| {
        format!("No `treehouse` on the PATH where {dir} is: install it there")
    })?;
    let no_update_check = format!("{TREEHOUSE_NO_UPDATE_CHECK_ENV}=1");
    // Treehouse runs git itself (in submodules too), which needs the same
    // ownership exception as tod's own git there.
    let mut all = vec![
        "-c",
        CONTAINER_GIT_CONFIG_ENV,
        "sh",
        "env",
        no_update_check.as_str(),
        program.as_str(),
    ];
    all.extend_from_slice(args);
    dir.output("sh", &all)
}

/// Whether `repo` declares submodules. Talks to Docker for a container.
fn has_gitmodules(repo: &Workdir) -> bool {
    match repo {
        Workdir::Host(path) => path.join(".gitmodules").is_file(),
        Workdir::Container { .. } | Workdir::Sandbox { .. } => repo
            .output("test", &["-f", ".gitmodules"])
            .is_ok_and(|out| out.status.success()),
    }
}

/// `treehouse get` for a lease, asking for the submodules to be set up too
/// when the repository has any.
fn lease_args(holder: &str, submodules: bool) -> Vec<&str> {
    let mut args = vec!["get", "--lease", "--lease-holder", holder, "--json"];
    if submodules {
        args.push("--submodules");
    }
    args
}

fn treehouse_get_lease(
    repo: &Workdir,
    holder: &str,
    settings: &TodSettings,
    paths: &TodPaths,
) -> Result<WorktreeHandle> {
    let args = lease_args(holder, has_gitmodules(repo));
    let output = match repo {
        Workdir::Host(repo) => {
            let invocation = TreehouseInvocation::resolve(settings, paths)?;
            invocation
                .command()
                .current_dir(repo)
                .args(&args)
                .output()
                .context("spawn treehouse get --lease")?
        }
        Workdir::Container { .. } | Workdir::Sandbox { .. } => {
            run_container_treehouse(repo, &args)?
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("treehouse get --lease failed: {}", stderr.trim());
    }
    let parsed: TreehouseLeaseJson =
        serde_json::from_slice(&output.stdout).context("parse treehouse get --json stdout")?;
    Ok(WorktreeHandle {
        path: repo.at(&parsed.path),
        lease: Some(TreehouseLease {
            lease_id: parsed.lease_id,
            lease_holder: parsed.lease_holder,
        }),
        warnings: Vec::new(),
    })
}

fn with_creation_lock<T>(data_root: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock_dir = data_root.join("worktrees");
    std::fs::create_dir_all(&lock_dir)?;
    let lock_path = lock_dir.join(".lock");
    let start = std::time::Instant::now();
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => {
                let result = f();
                let _ = std::fs::remove_file(&lock_path);
                return result;
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if start.elapsed() > Duration::from_secs(30) {
                    bail!("timed out waiting for worktree creation lock");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => return Err(err.into()),
        }
    }
}

/// A git worktree of `repo` for `branch` at [`worktree_dest_for`].
fn git_worktree(data_root: &Path, repo: &Workdir, branch: &str) -> Result<WorktreeHandle> {
    let dest = worktree_dest_for(data_root, repo, branch)?;
    let path = git_worktree_add(repo, &dest, branch)?;
    Ok(WorktreeHandle {
        path,
        lease: None,
        warnings: Vec::new(),
    })
}

/// Set up (or reuse) a worktree of `repo` for `branch`, with Treehouse or
/// git per `backend`, wherever the repository is.
/// The worktree another node already set up for a repository and branch:
/// `(repo, branch) -> path`. It should hold no lock beyond its own query,
/// since the rest of [`ensure_worktree`] can take minutes.
pub type SharedWorktreeLookup<'a> = &'a dyn Fn(&str, &str) -> Result<Option<String>>;

pub fn ensure_worktree(
    shared_worktree: SharedWorktreeLookup<'_>,
    backend: WorktreeBackend,
    settings: &TodSettings,
    paths: &TodPaths,
    data_root: &Path,
    repo: &Workdir,
    branch: &str,
    lease_holder: &str,
) -> Result<WorktreeHandle> {
    let repo_str = repo.storage();
    let branch_key = if branch.is_empty() {
        resolve_default_branch(repo)?
    } else {
        branch.to_string()
    };

    let shared = || -> Result<Option<Workdir>> {
        Ok(shared_worktree(&repo_str, &branch_key)?
            .map(|path| repo.at(&path))
            .filter(Workdir::is_dir))
    };
    if let Some(path) = shared()? {
        return Ok(WorktreeHandle {
            path,
            lease: None,
            warnings: Vec::new(),
        });
    }

    with_creation_lock(data_root, || {
        if let Some(path) = shared()? {
            return Ok(WorktreeHandle {
                path,
                lease: None,
                warnings: Vec::new(),
            });
        }

        let mut handle = match backend {
            WorktreeBackend::GitOnly => git_worktree(data_root, repo, &branch_key)?,
            WorktreeBackend::TreehouseRequired => {
                treehouse_get_lease(repo, lease_holder, settings, paths)?
            }
            WorktreeBackend::TreehouseWithGitFallback => {
                if treehouse_available_for(repo, settings) {
                    match treehouse_get_lease(repo, lease_holder, settings, paths) {
                        Ok(h) => h,
                        Err(err) => {
                            tracing::warn!(
                                "treehouse unavailable ({err:#}); falling back to git worktree"
                            );
                            git_worktree(data_root, repo, &branch_key)?
                        }
                    }
                } else {
                    tracing::warn!("treehouse not on PATH; falling back to git worktree");
                    git_worktree(data_root, repo, &branch_key)?
                }
            }
        };

        // Git only allows a branch to be checked out in one worktree at a time. If it's
        // already held elsewhere (e.g. the primary repo), leave a freshly provisioned
        // worktree (Treehouse's, typically detached) as-is rather than fail here.
        match worktree_holding_branch(repo, &branch_key)? {
            Some(holder) if !holder.same_location(&handle.path) => {}
            _ => {
                checkout_branch(&handle.path, &branch_key)?;
                // Whichever way the worktree was made, its submodules are set
                // up before they are put on the branch.
                if let Err(err) = init_submodules(&handle.path) {
                    handle.warnings.push(format!("{err:#}"));
                }
                handle
                    .warnings
                    .extend(branch_submodules(&handle.path, &branch_key)?);
            }
        }
        for warning in &handle.warnings {
            tracing::warn!("{warning}");
        }
        Ok(handle)
    })
}

/// Remove a git worktree previously added for `repo`. Refuses to touch the
/// primary checkout, and never forces: uncommitted changes make git refuse.
pub fn remove_git_worktree(repo: &Workdir, worktree: &Workdir) -> Result<()> {
    if repo.same_location(worktree) {
        return Ok(());
    }
    if !is_registered_worktree(repo, worktree)? {
        // Already unregistered — e.g. an earlier removal deleted the files but a
        // process with the folder open kept it on disk. Clean up what's left.
        worktree.remove_empty_dir();
        return Ok(());
    }
    let worktree_str = git_path_arg(worktree)?;
    if let Err(err) = run_git(repo, &["worktree", "remove", &worktree_str]) {
        // git unregisters the worktree before deleting its folder; when only the
        // folder delete failed, the worktree is already gone as far as git knows.
        if is_registered_worktree(repo, worktree)? {
            return Err(err);
        }
        tracing::warn!("git removed worktree {worktree} but left its folder: {err:#}");
    }
    Ok(())
}

/// Forget worktrees of `repo` whose folders no longer exist.
pub fn prune_git_worktrees(repo: &Workdir) -> Result<()> {
    run_git(repo, &["worktree", "prune"]).map(|_| ())
}

/// Whether git still lists `worktree` among `repo`'s worktrees.
fn is_registered_worktree(repo: &Workdir, worktree: &Workdir) -> Result<bool> {
    let output = run_git(repo, &["worktree", "list", "--porcelain"])?;
    let target = comparable_path(worktree);
    Ok(output
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|listed| repo.at(listed))
        .any(|listed| comparable_path(&listed) == target || listed.same_location(worktree)))
}

/// Path text normalized for comparing git's output with stored paths.
fn comparable_path(path: &Workdir) -> String {
    match path {
        Workdir::Host(path) => {
            let text = strip_verbatim(path).to_string_lossy().replace('\\', "/");
            let text = text.trim_end_matches('/');
            if cfg!(windows) {
                text.to_lowercase()
            } else {
                text.to_string()
            }
        }
        Workdir::Container { path, .. } | Workdir::Sandbox { path, .. } => {
            path.trim_end_matches('/').to_string()
        }
    }
}

/// Return a Treehouse lease so the worktree goes back to the pool.
pub fn treehouse_return(
    worktree: &Workdir,
    lease_id: &str,
    settings: &TodSettings,
    paths: &TodPaths,
) -> Result<()> {
    let output = match worktree {
        Workdir::Host(path) => {
            let invocation = TreehouseInvocation::resolve(settings, paths)?;
            invocation
                .command()
                .arg("return")
                .arg(path)
                .arg("--if-lease-id")
                .arg(lease_id)
                .output()
                .context("spawn treehouse return")?
        }
        Workdir::Container { path, .. } | Workdir::Sandbox { path, .. } => run_container_treehouse(
            &worktree.at("/"),
            &["return", path, "--if-lease-id", lease_id],
        )?,
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("treehouse return failed: {}", stderr.trim());
    }
    Ok(())
}

/// `repo` checked to be a git repository: canonicalized on this machine;
/// inside a container, as given.
pub fn validate_git_repo(repo: &Workdir) -> Result<Workdir> {
    let checked = match repo {
        Workdir::Host(path) => Workdir::Host(
            path.canonicalize()
                .with_context(|| format!("repo path {}", path.display()))?,
        ),
        Workdir::Container { .. } | Workdir::Sandbox { .. } => repo.clone(),
    };
    run_git(&checked, &["rev-parse", "--git-dir"])?;
    Ok(checked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command as StdCommand;

    fn init_temp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tod-wt-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        StdCommand::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&dir)
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .current_dir(&dir)
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["branch", "feature"])
            .current_dir(&dir)
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["branch", "dev"])
            .current_dir(&dir)
            .output()
            .unwrap();
        dir
    }

    fn git_in(dir: &Workdir, args: &[&str]) -> String {
        let mut all = vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "protocol.file.allow=always",
        ];
        all.extend_from_slice(args);
        let out = dir.git_output(&all).unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A superproject on `main` with two submodules, `a` and `b`, both detached.
    fn repo_with_submodules() -> Workdir {
        let sub = init_temp_repo();
        let top = Workdir::host(init_temp_repo());
        let sub_url = sub.to_string_lossy().replace('\\', "/");
        git_in(&top, &["submodule", "add", "-q", &sub_url, "a"]);
        git_in(&top, &["submodule", "add", "-q", &sub_url, "b"]);
        git_in(&top, &["commit", "-q", "-m", "subs"]);
        git_in(&top, &["submodule", "update", "-q", "--checkout"]);
        top
    }

    #[test]
    fn parses_submodule_status_lines() {
        assert_eq!(
            parse_submodule_status(" abc123 lib/a (heads/main)"),
            Some("lib/a")
        );
        assert_eq!(
            parse_submodule_status("+abc123 has space/x"),
            Some("has space/x")
        );
        assert_eq!(parse_submodule_status("-abc123 uninit"), None);
    }

    #[test]
    fn submodules_get_the_branch_and_unused_ones_are_pruned() {
        let top = repo_with_submodules();
        git_in(&top, &["switch", "-q", "-c", "tod/x"]);
        assert!(branch_submodules(&top, "tod/x").unwrap().is_empty());
        let (a, b) = (top.join("a"), top.join("b"));
        assert_eq!(current_branch(&a).as_deref(), Some("tod/x"));
        assert_eq!(current_branch(&b).as_deref(), Some("tod/x"));

        git_in(&a, &["commit", "-q", "--allow-empty", "-m", "work"]);
        prune_submodule_branches(&top, "tod/x").unwrap();
        assert_eq!(current_branch(&a).as_deref(), Some("tod/x"), "used: kept");
        assert_eq!(current_branch(&b), None, "unused: detached again");
        assert!(!branch_exists(&b, "tod/x"));
    }

    /// A clone of a repository whose `feature` has one commit more than `main`.
    fn clone_with_remote_feature() -> (Workdir, String) {
        let origin = Workdir::host(init_temp_repo());
        git_in(&origin, &["switch", "-q", "feature"]);
        git_in(&origin, &["commit", "-q", "--allow-empty", "-m", "pushed work"]);
        let tip = git_in(&origin, &["rev-parse", "HEAD"]);
        git_in(&origin, &["switch", "-q", "main"]);
        let dir = std::env::temp_dir().join(format!("tod-wt-clone-{}", uuid::Uuid::new_v4()));
        let out = StdCommand::new("git")
            .args(["clone", "-q"])
            .arg(origin.host_path().unwrap())
            .arg(&dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        (Workdir::host(dir), tip)
    }

    fn upstream_of(repo: &Workdir, branch: &str) -> Option<String> {
        run_git(repo, &["rev-parse", "--abbrev-ref", &format!("{branch}@{{upstream}}")]).ok()
    }

    #[test]
    fn a_branch_on_origin_is_checked_out_from_it_and_linked() {
        let (clone, tip) = clone_with_remote_feature();
        checkout_branch(&clone, "feature").unwrap();
        assert_eq!(current_branch(&clone).as_deref(), Some("feature"));
        assert_eq!(run_git(&clone, &["rev-parse", "HEAD"]).unwrap(), tip);
        assert_eq!(upstream_of(&clone, "feature").as_deref(), Some("origin/feature"));

        // A branch only here gets no upstream.
        checkout_branch(&clone, "local-only").unwrap();
        assert_eq!(upstream_of(&clone, "local-only"), None);
    }

    #[test]
    fn an_existing_branch_is_linked_to_origin_of_the_same_name() {
        let (clone, _) = clone_with_remote_feature();
        git_in(&clone, &["branch", "--no-track", "feature", "main"]);
        assert_eq!(upstream_of(&clone, "feature"), None);
        checkout_branch(&clone, "feature").unwrap();
        assert_eq!(upstream_of(&clone, "feature").as_deref(), Some("origin/feature"));

        // Already on it, still unlinked: linked, and nothing else changes.
        git_in(&clone, &["branch", "--unset-upstream", "feature"]);
        let head = run_git(&clone, &["rev-parse", "HEAD"]).unwrap();
        checkout_branch(&clone, "feature").unwrap();
        assert_eq!(upstream_of(&clone, "feature").as_deref(), Some("origin/feature"));
        assert_eq!(run_git(&clone, &["rev-parse", "HEAD"]).unwrap(), head);
    }

    #[test]
    fn a_new_git_worktree_of_a_branch_on_origin_tracks_it() {
        let (clone, tip) = clone_with_remote_feature();
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        let dest = worktree_dest_for(&data_root, &clone, "feature").unwrap();
        let path = git_worktree_add(&clone, &dest, "feature").unwrap();
        assert_eq!(run_git(&path, &["rev-parse", "HEAD"]).unwrap(), tip);
        assert_eq!(upstream_of(&path, "feature").as_deref(), Some("origin/feature"));
        let _ = fs::remove_dir_all(&data_root);
    }

    #[test]
    fn a_submodule_branch_on_origin_is_linked_and_never_pruned() {
        // The submodules' origin has a `feature` branch.
        let top = repo_with_submodules();
        // Its own `feature` predates the submodules.
        git_in(&top, &["branch", "-q", "-D", "feature"]);
        git_in(&top, &["switch", "-q", "-c", "feature"]);
        assert!(branch_submodules(&top, "feature").unwrap().is_empty());
        let a = top.join("a");
        assert_eq!(current_branch(&a).as_deref(), Some("feature"));
        assert_eq!(upstream_of(&a, "feature").as_deref(), Some("origin/feature"));
        prune_submodule_branches(&top, "feature").unwrap();
        assert_eq!(current_branch(&a).as_deref(), Some("feature"));
    }

    #[test]
    fn rename_moves_the_branch_in_the_superproject_and_submodules() {
        let top = repo_with_submodules();
        git_in(&top, &["switch", "-q", "-c", "tod/x"]);
        branch_submodules(&top, "tod/x").unwrap();
        git_in(
            &top.join("a"),
            &["commit", "-q", "--allow-empty", "-m", "work"],
        );

        assert!(
            rename_branch(&top, "tod/x", "renamed/y")
                .unwrap()
                .is_empty()
        );
        for repo in [top.clone(), top.join("a"), top.join("b")] {
            assert_eq!(current_branch(&repo).as_deref(), Some("renamed/y"));
            assert!(!branch_exists(&repo, "tod/x"));
        }
        // The start mark moved with the branch, so pruning still recognizes `b`.
        prune_submodule_branches(&top, "renamed/y").unwrap();
        assert_eq!(current_branch(&top.join("b")), None);
    }

    #[test]
    fn rename_refuses_a_name_taken_in_a_submodule_and_changes_nothing() {
        let top = repo_with_submodules();
        git_in(&top, &["switch", "-q", "-c", "tod/x"]);
        branch_submodules(&top, "tod/x").unwrap();
        git_in(&top.join("b"), &["branch", "taken"]);

        let err = rename_branch(&top, "tod/x", "taken").unwrap_err();
        assert!(format!("{err:#}").contains("already exists"), "{err:#}");
        for repo in [top.clone(), top.join("a"), top.join("b")] {
            assert_eq!(current_branch(&repo).as_deref(), Some("tod/x"));
        }
    }

    #[test]
    fn git_worktree_reuses_existing_checkout() {
        let repo = Workdir::host(init_temp_repo());
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        let dest = worktree_dest_for(&data_root, &repo, "feature").unwrap();
        let first = git_worktree_add(&repo, &dest, "feature").unwrap();
        checkout_branch(&first, "feature").unwrap();

        let dest2 = worktree_dest_for(&data_root, &repo, "feature").unwrap();
        let second = git_worktree_add(&repo, &dest2, "feature").unwrap();
        assert!(first.same_location(&second));

        let _ = fs::remove_dir_all(repo.host_path().unwrap());
        let _ = fs::remove_dir_all(&data_root);
    }

    #[test]
    fn git_worktree_reuses_primary_repo_when_branch_checked_out() {
        let repo = Workdir::host(init_temp_repo());
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        let dest = worktree_dest_for(&data_root, &repo, "main").unwrap();
        let path = git_worktree_add(&repo, &dest, "main").unwrap();
        assert!(path.same_location(&repo));
        assert!(!dest.is_dir());
        let _ = fs::remove_dir_all(repo.host_path().unwrap());
        let _ = fs::remove_dir_all(&data_root);
    }

    /// A repository inside a dev container gets its worktree there, under
    /// `.worktrees/`, kept out of its `git status`. Needs a running container
    /// with git: `TOD_TEST_DEV_CONTAINER=<name>`.
    #[test]
    fn a_repository_in_a_dev_container_gets_its_worktree_there() {
        let Ok(container) = std::env::var("TOD_TEST_DEV_CONTAINER") else {
            eprintln!("skipping: TOD_TEST_DEV_CONTAINER is not set");
            return;
        };
        let dir = format!("/tmp/tod-wt-{}", uuid::Uuid::new_v4());
        let scratch = Workdir::container(&container, "/tmp");
        let out = scratch
            .output(
                "sh",
                &[
                    "-c",
                    "git init -q -b main \"$1\" && cd \"$1\" && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m init",
                    "sh",
                    &dir,
                ],
            )
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

        let repo = validate_git_repo(&Workdir::container(&container, &dir)).unwrap();
        let data_root = std::env::temp_dir();
        let handle = git_worktree(&data_root, &repo, "task/in-container").unwrap();
        assert_eq!(handle.path.container_name(), Some(container.as_str()));
        assert_eq!(handle.path.path_text(), format!("{dir}/.worktrees/task_in-container"));
        assert!(handle.path.is_dir());
        assert_eq!(
            run_git(&handle.path, &["branch", "--show-current"]).unwrap(),
            "task/in-container"
        );
        assert_eq!(run_git(&repo, &["status", "--porcelain"]).unwrap(), "");

        // Asking again reuses it.
        let again = git_worktree(&data_root, &repo, "task/in-container").unwrap();
        assert!(again.path.same_location(&handle.path));

        remove_git_worktree(&repo, &handle.path).unwrap();
        assert!(!handle.path.is_dir());
        let _ = scratch.output("rm", &["-rf", &dir]);
    }

    /// Git in a dev container works on a repository some other user owns
    /// (as a bind mount can briefly report), where it would otherwise stop
    /// with "dubious ownership". Needs `TOD_TEST_DEV_CONTAINER`, as above.
    #[test]
    fn git_in_a_dev_container_ignores_who_owns_the_repository() {
        let Ok(container) = std::env::var("TOD_TEST_DEV_CONTAINER") else {
            eprintln!("skipping: TOD_TEST_DEV_CONTAINER is not set");
            return;
        };
        let dir = format!("/tmp/tod-own-{}", uuid::Uuid::new_v4());
        let as_root = |script: &str| {
            StdCommand::new("docker")
                .args(["exec", "-u", "root", &container, "sh", "-c", script, "sh", &dir])
                .output()
                .unwrap()
        };
        let out = as_root(
            "git init -q -b main \"$1\" && cd \"$1\" && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m init && chown -R 4321 \"$1\" && chmod -R a+rwX \"$1\"",
        );
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let scratch = Workdir::container(&container, "/tmp");
        let plain = scratch.output("git", &["-C", &dir, "rev-parse", "HEAD"]).unwrap();
        assert!(!plain.status.success(), "the container's git should check ownership");

        let repo = Workdir::container(&container, &dir);
        assert_eq!(run_git(&repo, &["branch", "--show-current"]).unwrap(), "main");

        // So does git a program run there starts itself (Treehouse), and git
        // config the container already puts in the environment still applies.
        let out = scratch
            .output(
                "env",
                &[
                    "GIT_CONFIG_COUNT=1",
                    "GIT_CONFIG_KEY_0=tod.test",
                    "GIT_CONFIG_VALUE_0=kept",
                    "sh",
                    "-c",
                    CONTAINER_GIT_CONFIG_ENV,
                    "sh",
                    "sh",
                    "-c",
                    "git -C \"$1\" rev-parse --abbrev-ref HEAD && git config tod.test",
                    "sh",
                    &dir,
                ],
            )
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "main\nkept");
        as_root("rm -rf \"$1\"");
    }

    #[test]
    fn a_lease_asks_for_submodules_only_when_the_repository_has_them() {
        let repo = Workdir::host(init_temp_repo());
        assert!(!has_gitmodules(&repo));
        assert!(!lease_args("h", false).contains(&"--submodules"));
        fs::write(repo.host_path().unwrap().join(".gitmodules"), "").unwrap();
        assert!(has_gitmodules(&repo));
        assert_eq!(
            lease_args("h", true),
            ["get", "--lease", "--lease-holder", "h", "--json", "--submodules"]
        );
        let _ = fs::remove_dir_all(repo.host_path().unwrap());
    }

    /// A repository with a submodule that has its own submodule, the way a
    /// worktree comes back when nothing set its submodules up.
    fn repo_with_nested_submodules() -> (Workdir, Vec<PathBuf>) {
        // SAFETY: every test that sets it sets the same values.
        unsafe {
            std::env::set_var("GIT_CONFIG_COUNT", "1");
            std::env::set_var("GIT_CONFIG_KEY_0", "protocol.file.allow");
            std::env::set_var("GIT_CONFIG_VALUE_0", "always");
        }
        let commit = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "add"];
        let url = |p: &Path| p.to_string_lossy().replace('\\', "/");
        let leaf = init_temp_repo();
        let mid = init_temp_repo();
        let mid_dir = Workdir::host(mid.clone());
        run_git(&mid_dir, &["submodule", "add", "-q", &url(&leaf), "leaf"]).unwrap();
        run_git(&mid_dir, &commit).unwrap();
        let top = init_temp_repo();
        let repo = Workdir::host(top.clone());
        run_git(&repo, &["submodule", "add", "-q", &url(&mid), "mid"]).unwrap();
        run_git(&repo, &commit).unwrap();
        (repo, vec![top, mid, leaf])
    }

    #[test]
    fn an_uninitialized_nested_submodule_is_set_up_and_put_on_the_branch() {
        let (repo, dirs) = repo_with_nested_submodules();
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        // A bare `git worktree add`, like a pool worktree nobody initialized.
        let bare = worktree_dest_for(&data_root, &repo, "bare").unwrap();
        run_git(&repo, &["worktree", "add", "-q", "--detach", &git_path_arg(&bare).unwrap()])
            .unwrap();
        assert!(submodule_dirs(&bare).unwrap().is_empty());
        init_submodules(&bare).unwrap();
        assert_eq!(submodule_dirs(&bare).unwrap().len(), 2);

        let no_shared = |_: &str, _: &str| Ok(None);
        let settings = TodSettings::default();
        crate::paths::set_data_root(data_root.clone());
        let paths = TodPaths::discover().unwrap();
        crate::paths::clear_data_root_override();
        let handle = ensure_worktree(
            &no_shared,
            WorktreeBackend::GitOnly,
            &settings,
            &paths,
            &data_root,
            &repo,
            "task/nested",
            "tod-test",
        )
        .unwrap();
        assert!(handle.warnings.is_empty(), "{:?}", handle.warnings);
        let subs = submodule_dirs(&handle.path).unwrap();
        assert_eq!(subs.len(), 2);
        for sub in &subs {
            assert_eq!(current_branch(sub).as_deref(), Some("task/nested"), "{sub}");
        }

        for dir in dirs {
            let _ = fs::remove_dir_all(dir);
        }
        let _ = fs::remove_dir_all(&data_root);
    }

    #[test]
    fn a_new_git_worktree_has_its_submodules() {
        let sub = init_temp_repo();
        let repo = Workdir::host(init_temp_repo());
        let host = repo.host_path().unwrap().to_path_buf();
        // Submodules from a local path need the file protocol allowed, and a
        // submodule clone reads no repository config, only the environment.
        // SAFETY: every test that sets it sets the same values.
        unsafe {
            std::env::set_var("GIT_CONFIG_COUNT", "1");
            std::env::set_var("GIT_CONFIG_KEY_0", "protocol.file.allow");
            std::env::set_var("GIT_CONFIG_VALUE_0", "always");
        }
        let url = sub.to_string_lossy().replace('\\', "/");
        run_git(&repo, &["submodule", "add", "-q", &url, "sub"]).unwrap();
        run_git(
            &repo,
            &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "add sub"],
        )
        .unwrap();

        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        let handle = git_worktree(&data_root, &repo, "with-sub").unwrap();
        let subs = submodule_dirs(&handle.path).unwrap();
        assert_eq!(subs.len(), 1, "submodule not initialized in {}", handle.path);
        assert!(run_git(&subs[0], &["rev-parse", "HEAD"]).is_ok());

        let _ = fs::remove_dir_all(&host);
        let _ = fs::remove_dir_all(&sub);
        let _ = fs::remove_dir_all(&data_root);
    }

    /// Needs `TOD_TEST_DEV_CONTAINER_TREEHOUSE`: a running container with
    /// git and a `treehouse` on its `PATH`.
    #[test]
    fn a_repository_in_a_dev_container_leases_from_its_treehouse() {
        let Ok(container) = std::env::var("TOD_TEST_DEV_CONTAINER_TREEHOUSE") else {
            eprintln!("skipping: TOD_TEST_DEV_CONTAINER_TREEHOUSE is not set");
            return;
        };
        let dir = format!("/tmp/tod-th-{}", uuid::Uuid::new_v4());
        let scratch = Workdir::container(&container, "/tmp");
        let out = scratch
            .output(
                "sh",
                &[
                    "-c",
                    "git init -q -b main \"$1\" && cd \"$1\" && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m init",
                    "sh",
                    &dir,
                ],
            )
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

        use crate::paths::{clear_data_root_override, set_data_root};
        let repo = Workdir::container(&container, &dir);
        let data_root = std::env::temp_dir().join(format!("tod-th-data-{}", uuid::Uuid::new_v4()));
        set_data_root(data_root.clone());
        let paths = TodPaths::discover().unwrap();
        clear_data_root_override();
        let settings = TodSettings::default();
        assert!(treehouse_available_for(&repo, &settings));

        let handle = treehouse_get_lease(&repo, "tod-test", &settings, &paths).unwrap();
        assert_eq!(handle.path.container_name(), Some(container.as_str()));
        assert!(handle.path.is_dir());
        let lease = handle.lease.expect("a lease");
        assert_eq!(lease.lease_holder, "tod-test");

        treehouse_return(&handle.path, &lease.lease_id, &settings, &paths).unwrap();
        let _ = scratch.output("rm", &["-rf", &dir]);
        let _ = fs::remove_dir_all(&data_root);
    }

    /// Needs `TOD_TEST_DEV_CONTAINER_TREEHOUSE`, as above.
    #[test]
    fn a_leased_worktree_in_a_dev_container_has_its_submodules() {
        let Ok(container) = std::env::var("TOD_TEST_DEV_CONTAINER_TREEHOUSE") else {
            eprintln!("skipping: TOD_TEST_DEV_CONTAINER_TREEHOUSE is not set");
            return;
        };
        let dir = format!("/tmp/tod-th-sub-{}", uuid::Uuid::new_v4());
        let scratch = Workdir::container(&container, "/tmp");
        let script = r#"set -e
mkdir -p "$1" && cd "$1"
git init -q -b main sub && git -C sub -c user.name=t -c user.email=t@t commit -q --allow-empty -m sub
git init -q -b main main && cd main
git -c protocol.file.allow=always submodule add -q "$1/sub" sub
git -c user.name=t -c user.email=t@t commit -q -m 'add sub'"#;
        let out = scratch.output("sh", &["-c", script, "sh", &dir]).unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

        use crate::paths::{clear_data_root_override, set_data_root};
        let repo = Workdir::container(&container, format!("{dir}/main"));
        assert!(has_gitmodules(&repo));
        let data_root = std::env::temp_dir().join(format!("tod-th-data-{}", uuid::Uuid::new_v4()));
        set_data_root(data_root.clone());
        let paths = TodPaths::discover().unwrap();
        clear_data_root_override();
        let settings = TodSettings::default();

        let no_shared = |_: &str, _: &str| Ok(None);
        let handle = ensure_worktree(
            &no_shared,
            WorktreeBackend::TreehouseRequired,
            &settings,
            &paths,
            &data_root,
            &repo,
            "task/with-sub",
            "tod-test",
        )
        .unwrap();
        assert!(handle.warnings.is_empty(), "{:?}", handle.warnings);
        let subs = submodule_dirs(&handle.path).unwrap();
        assert_eq!(subs.len(), 1, "submodule not initialized in {}", handle.path);
        assert_eq!(current_branch(&subs[0]).as_deref(), Some("task/with-sub"));

        let lease = handle.lease.expect("a lease");
        treehouse_return(&handle.path, &lease.lease_id, &settings, &paths).unwrap();
        let _ = scratch.output("rm", &["-rf", &dir]);
        let _ = fs::remove_dir_all(&data_root);
    }

    #[test]
    fn validate_interview_workspace_accepts_existing_worktree() {
        let repo = Workdir::host(init_temp_repo());
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        let dest = worktree_dest_for(&data_root, &repo, "feature").unwrap();
        let _ = git_worktree_add(&repo, &dest, "feature").unwrap();
        validate_interview_workspace(repo.host_path().unwrap(), "feature").unwrap();
        let _ = fs::remove_dir_all(repo.host_path().unwrap());
        let _ = fs::remove_dir_all(&data_root);
    }

    #[test]
    fn validate_interview_workspace_accepts_primary_checkout() {
        let repo = Workdir::host(init_temp_repo());
        validate_interview_workspace(repo.host_path().unwrap(), "main").unwrap();
        let _ = fs::remove_dir_all(repo.host_path().unwrap());
    }

    #[test]
    fn git_worktree_add_and_checkout() {
        let repo = Workdir::host(init_temp_repo());
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        let dest = worktree_dest_for(&data_root, &repo, "feature").unwrap();
        let path = git_worktree_add(&repo, &dest, "feature").unwrap();
        checkout_branch(&path, "feature").unwrap();
        let branch = run_git(&path, &["branch", "--show-current"]).unwrap();
        assert_eq!(branch, "feature");
        let _ = fs::remove_dir_all(repo.host_path().unwrap());
        let _ = fs::remove_dir_all(&data_root);
    }

    #[test]
    fn remove_git_worktree_finishes_a_half_removed_worktree() {
        let repo = Workdir::host(init_temp_repo());
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        let dest = worktree_dest_for(&data_root, &repo, "feature").unwrap();
        let path = git_worktree_add(&repo, &dest, "feature").unwrap();
        assert!(is_registered_worktree(&repo, &path).unwrap());

        // Simulate git unregistering the worktree but failing to delete its folder.
        let path_str = path.path_text();
        run_git(&repo, &["worktree", "remove", &path_str]).unwrap();
        path.create_dir_all().unwrap();
        assert!(!is_registered_worktree(&repo, &path).unwrap());

        remove_git_worktree(&repo, &path).unwrap();
        assert!(!path.is_dir());

        let _ = fs::remove_dir_all(repo.host_path().unwrap());
        let _ = fs::remove_dir_all(&data_root);
    }

    #[test]
    fn git_worktree_sharing_by_branch() {
        use crate::fleet::repos::node_files::NodeFilesRepo;
        use crate::fleet::repos::task::{FleetTask, TaskRepo};
        use crate::fleet::repos::{cleanup_test_dir, test_writer_conn};
        use crate::paths::{clear_data_root_override, set_data_root};
        use crate::settings::TodSettings;
        use crate::settings::WorktreeBackend;
        use uuid::Uuid;

        let git_repo = Workdir::host(init_temp_repo());
        let repo_str = git_repo.storage();
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        set_data_root(data_root.clone());
        let paths = TodPaths::discover().unwrap();
        let settings = TodSettings::default();
        let (db_dir, conn) = test_writer_conn();

        let node_main = Uuid::new_v4().to_string();
        let node_feature = Uuid::new_v4().to_string();
        let node_main2 = Uuid::new_v4().to_string();
        for (id, branch, slug) in [
            (&node_main, "feature", "feature-task"),
            (&node_feature, "dev", "dev-task"),
            (&node_main2, "feature", "feature-task-2"),
        ] {
            TaskRepo::new(&conn)
                .insert(&FleetTask {
                    id: id.clone(),
                    title: "t".into(),
                    slug: slug.into(),
                    lifecycle: "proposed".into(),
                    repo: Some(repo_str.clone()),
                    branch: Some(branch.into()),
                    ..FleetTask::new(id, "t", slug)
                })
                .unwrap();
        }

        let shared = |repo: &str, branch: &str| {
            NodeFilesRepo::new(&conn).resolve_shared_worktree_path(repo, branch)
        };
        let main_handle = ensure_worktree(
            &shared,
            WorktreeBackend::GitOnly,
            &settings,
            &paths,
            &data_root,
            &git_repo,
            "feature",
            "tod-a",
        )
        .unwrap();
        let feature_handle = ensure_worktree(
            &shared,
            WorktreeBackend::GitOnly,
            &settings,
            &paths,
            &data_root,
            &git_repo,
            "dev",
            "tod-b",
        )
        .unwrap();
        assert_ne!(main_handle.path, feature_handle.path);

        NodeFilesRepo::new(&conn)
            .update_worktree(
                &node_main,
                Some(main_handle.path.storage().as_str()),
                None,
                None,
            )
            .unwrap();

        let reused = ensure_worktree(
            &shared,
            WorktreeBackend::GitOnly,
            &settings,
            &paths,
            &data_root,
            &git_repo,
            "feature",
            "tod-c",
        )
        .unwrap();
        assert_eq!(reused.path, main_handle.path);
        assert!(reused.lease.is_none());

        remove_git_worktree(&git_repo, &main_handle.path).unwrap();
        assert!(!main_handle.path.is_dir());
        // The primary checkout is never removed.
        remove_git_worktree(&git_repo, &git_repo).unwrap();
        assert!(git_repo.is_dir());

        let _ = fs::remove_dir_all(git_repo.host_path().unwrap());
        let _ = fs::remove_dir_all(&data_root);
        cleanup_test_dir(&db_dir);
        clear_data_root_override();
    }
}
