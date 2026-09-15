//! Git worktree and Treehouse provisioning for agent workspaces.

use crate::fleet::treehouse::TreehouseInvocation;
use crate::paths::TodPaths;
use crate::settings::{TodSettings, WorktreeBackend};
use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Strip Win32 `\\?\` extended-length prefix so git and error messages see normal paths.
fn path_for_git(path: &Path) -> PathBuf {
    let raw = path.as_os_str().to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(stripped) = raw.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path.to_path_buf()
}

fn paths_refer_to_same_location(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[derive(Debug, Clone)]
pub struct TreehouseLease {
    pub lease_id: String,
    pub lease_holder: String,
}

#[derive(Debug, Clone)]
pub struct WorktreeHandle {
    pub path: PathBuf,
    pub lease: Option<TreehouseLease>,
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

fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let git_repo = path_for_git(repo);
    let output = Command::new("git")
        .arg("-C")
        .arg(&git_repo)
        .args(args)
        .output()
        .with_context(|| format!("spawn git -C {} {:?}", git_repo.display(), args))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "git -C {} {} failed: {}",
        git_repo.display(),
        args.join(" "),
        stderr.trim()
    );
}

/// Returns the worktree path where `branch` is checked out, if any.
fn worktree_holding_branch(repo: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let output = run_git(repo, &["worktree", "list", "--porcelain"])?;
    let branch_ref = format!("refs/heads/{branch}");
    let mut current_worktree: Option<PathBuf> = None;
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_worktree = Some(PathBuf::from(path));
        } else if let Some(b) = line.strip_prefix("branch ") {
            if b == branch_ref {
                return Ok(current_worktree);
            }
        }
    }
    Ok(None)
}

pub fn resolve_default_branch(repo: &Path) -> Result<String> {
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

pub fn checkout_branch(worktree: &Path, branch: &str) -> Result<()> {
    if branch.is_empty() {
        return Ok(());
    }
    if run_git(worktree, &["rev-parse", "--verify", branch]).is_ok() {
        run_git(worktree, &["switch", branch])?;
    } else {
        run_git(worktree, &["switch", "-c", branch])?;
    }
    Ok(())
}

fn git_worktree_add(repo: &Path, dest: &Path, branch: &str) -> Result<PathBuf> {
    if dest.exists() {
        return Ok(dest.to_path_buf());
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
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create worktree parent {}", parent.display()))?;
    }
    let dest_str = path_for_git(dest)
        .to_str()
        .context("worktree dest utf8")?
        .to_string();
    if run_git(repo, &["rev-parse", "--verify", &branch_ref]).is_ok() {
        run_git(repo, &["worktree", "add", &dest_str, &branch_ref])?;
    } else {
        run_git(repo, &["worktree", "add", "-b", &branch_ref, &dest_str])?;
    }
    Ok(dest.to_path_buf())
}

/// Validate that an interview workspace can be provisioned for `repo` + `branch`.
pub fn validate_interview_workspace(repo: &Path, branch: &str) -> Result<()> {
    let canonical = validate_git_repo(repo)?;
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

fn treehouse_get_lease(
    repo: &Path,
    holder: &str,
    settings: &TodSettings,
    paths: &TodPaths,
) -> Result<WorktreeHandle> {
    let invocation = TreehouseInvocation::resolve(settings, paths)?;
    let mut command = Command::new("treehouse");
    invocation.apply_to(&mut command);
    let output = command
        .current_dir(repo)
        .arg("get")
        .arg("--lease")
        .arg("--lease-holder")
        .arg(holder)
        .arg("--json")
        .output()
        .context("spawn treehouse get --lease")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("treehouse get --lease failed: {}", stderr.trim());
    }
    let parsed: TreehouseLeaseJson =
        serde_json::from_slice(&output.stdout).context("parse treehouse get --json stdout")?;
    Ok(WorktreeHandle {
        path: PathBuf::from(parsed.path),
        lease: Some(TreehouseLease {
            lease_id: parsed.lease_id,
            lease_holder: parsed.lease_holder,
        }),
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

pub fn ensure_worktree(
    conn: &Connection,
    backend: WorktreeBackend,
    settings: &TodSettings,
    paths: &TodPaths,
    data_root: &Path,
    repo: &Path,
    branch: &str,
    lease_holder: &str,
) -> Result<WorktreeHandle> {
    use crate::fleet::repos::node_files::NodeFilesRepo;

    let repo_str = repo.to_string_lossy();
    let branch_key = if branch.is_empty() {
        resolve_default_branch(repo)?
    } else {
        branch.to_string()
    };

    if let Some(shared) =
        NodeFilesRepo::new(conn).resolve_shared_worktree_path(&repo_str, &branch_key)?
    {
        let path = PathBuf::from(shared);
        if path.is_dir() {
            return Ok(WorktreeHandle { path, lease: None });
        }
    }

    with_creation_lock(data_root, || {
        if let Some(shared) =
            NodeFilesRepo::new(conn).resolve_shared_worktree_path(&repo_str, &branch_key)?
        {
            let path = PathBuf::from(shared);
            if path.is_dir() {
                return Ok(WorktreeHandle { path, lease: None });
            }
        }

        let handle = match backend {
            WorktreeBackend::GitOnly => {
                let dest = worktree_dest(data_root, repo, &branch_key)?;
                let path = git_worktree_add(repo, &dest, &branch_key)?;
                WorktreeHandle { path, lease: None }
            }
            WorktreeBackend::TreehouseRequired => {
                treehouse_get_lease(repo, lease_holder, settings, paths)?
            }
            WorktreeBackend::TreehouseWithGitFallback => {
                if treehouse_available() {
                    match treehouse_get_lease(repo, lease_holder, settings, paths) {
                        Ok(h) => h,
                        Err(err) => {
                            tracing::warn!(
                                "treehouse unavailable ({err:#}); falling back to git worktree"
                            );
                            let dest = worktree_dest(data_root, repo, &branch_key)?;
                            let path = git_worktree_add(repo, &dest, &branch_key)?;
                            WorktreeHandle { path, lease: None }
                        }
                    }
                } else {
                    tracing::warn!("treehouse not on PATH; falling back to git worktree");
                    let dest = worktree_dest(data_root, repo, &branch_key)?;
                    let path = git_worktree_add(repo, &dest, &branch_key)?;
                    WorktreeHandle { path, lease: None }
                }
            }
        };

        // Git only allows a branch to be checked out in one worktree at a time. If it's
        // already held elsewhere (e.g. the primary repo), leave a freshly provisioned
        // worktree (Treehouse's, typically detached) as-is rather than fail here.
        match worktree_holding_branch(repo, &branch_key)? {
            Some(holder) if !paths_refer_to_same_location(&holder, &handle.path) => {}
            _ => checkout_branch(&handle.path, &branch_key)?,
        }
        Ok(handle)
    })
}

/// Remove a git worktree previously added for `repo`. Refuses to touch the
/// primary checkout, and never forces: uncommitted changes make git refuse.
pub fn remove_git_worktree(repo: &Path, worktree: &Path) -> Result<()> {
    if paths_refer_to_same_location(repo, worktree) {
        return Ok(());
    }
    let worktree = path_for_git(worktree);
    if !is_registered_worktree(repo, &worktree)? {
        // Already unregistered — e.g. an earlier removal deleted the files but a
        // process with the folder open kept it on disk. Clean up what's left.
        let _ = std::fs::remove_dir(&worktree);
        return Ok(());
    }
    let worktree_str = worktree.to_str().context("worktree path utf8")?.to_string();
    if let Err(err) = run_git(repo, &["worktree", "remove", &worktree_str]) {
        // git unregisters the worktree before deleting its folder; when only the
        // folder delete failed, the worktree is already gone as far as git knows.
        if is_registered_worktree(repo, &worktree)? {
            return Err(err);
        }
        tracing::warn!(
            "git removed worktree {} but left its folder: {err:#}",
            worktree.display()
        );
    }
    Ok(())
}

/// Forget worktrees of `repo` whose folders no longer exist.
pub fn prune_git_worktrees(repo: &Path) -> Result<()> {
    run_git(repo, &["worktree", "prune"]).map(|_| ())
}

/// Whether git still lists `worktree` among `repo`'s worktrees.
fn is_registered_worktree(repo: &Path, worktree: &Path) -> Result<bool> {
    let output = run_git(repo, &["worktree", "list", "--porcelain"])?;
    let target = comparable_path(worktree);
    Ok(output
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(Path::new)
        .any(|listed| {
            comparable_path(listed) == target || paths_refer_to_same_location(listed, worktree)
        }))
}

/// Path text normalized for comparing git's output with stored paths.
fn comparable_path(path: &Path) -> String {
    let text = path_for_git(path).to_string_lossy().replace('\\', "/");
    let text = text.trim_end_matches('/');
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

/// Return a Treehouse lease so the worktree goes back to the pool.
pub fn treehouse_return(
    worktree: &Path,
    lease_id: &str,
    settings: &TodSettings,
    paths: &TodPaths,
) -> Result<()> {
    let invocation = TreehouseInvocation::resolve(settings, paths)?;
    let mut command = Command::new("treehouse");
    invocation.apply_to(&mut command);
    let output = command
        .arg("return")
        .arg(worktree)
        .arg("--if-lease-id")
        .arg(lease_id)
        .output()
        .context("spawn treehouse return")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("treehouse return failed: {}", stderr.trim());
    }
    Ok(())
}

pub fn validate_git_repo(repo: &Path) -> Result<PathBuf> {
    let canonical = repo
        .canonicalize()
        .with_context(|| format!("repo path {}", repo.display()))?;
    run_git(&canonical, &["rev-parse", "--git-dir"])?;
    Ok(canonical)
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

    #[test]
    fn git_worktree_reuses_existing_checkout() {
        let repo = init_temp_repo();
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        let dest = worktree_dest(&data_root, &repo, "feature").unwrap();
        let first = git_worktree_add(&repo, &dest, "feature").unwrap();
        checkout_branch(&first, "feature").unwrap();

        let dest2 = worktree_dest(&data_root, &repo, "feature").unwrap();
        let second = git_worktree_add(&repo, &dest2, "feature").unwrap();
        assert!(paths_refer_to_same_location(&first, &second));

        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&data_root);
    }

    #[test]
    fn git_worktree_reuses_primary_repo_when_branch_checked_out() {
        let repo = init_temp_repo();
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        let dest = worktree_dest(&data_root, &repo, "main").unwrap();
        let path = git_worktree_add(&repo, &dest, "main").unwrap();
        assert!(paths_refer_to_same_location(&path, &repo));
        assert!(!dest.exists());
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&data_root);
    }

    #[test]
    fn validate_interview_workspace_accepts_existing_worktree() {
        let repo = init_temp_repo();
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        let dest = worktree_dest(&data_root, &repo, "feature").unwrap();
        let _ = git_worktree_add(&repo, &dest, "feature").unwrap();
        validate_interview_workspace(&repo, "feature").unwrap();
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&data_root);
    }

    #[test]
    fn validate_interview_workspace_accepts_primary_checkout() {
        let repo = init_temp_repo();
        validate_interview_workspace(&repo, "main").unwrap();
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn git_worktree_add_and_checkout() {
        let repo = init_temp_repo();
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        let dest = worktree_dest(&data_root, &repo, "feature").unwrap();
        let path = git_worktree_add(&repo, &dest, "feature").unwrap();
        checkout_branch(&path, "feature").unwrap();
        let branch = run_git(&path, &["branch", "--show-current"]).unwrap();
        assert_eq!(branch, "feature");
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&data_root);
    }

    #[test]
    fn remove_git_worktree_finishes_a_half_removed_worktree() {
        let repo = init_temp_repo();
        let data_root = std::env::temp_dir().join(format!("tod-wt-data-{}", uuid::Uuid::new_v4()));
        let dest = worktree_dest(&data_root, &repo, "feature").unwrap();
        let path = git_worktree_add(&repo, &dest, "feature").unwrap();
        assert!(is_registered_worktree(&repo, &path).unwrap());

        // Simulate git unregistering the worktree but failing to delete its folder.
        let path_str = path_for_git(&path).to_string_lossy().into_owned();
        run_git(&repo, &["worktree", "remove", &path_str]).unwrap();
        fs::create_dir_all(&path).unwrap();
        assert!(!is_registered_worktree(&repo, &path).unwrap());

        remove_git_worktree(&repo, &path).unwrap();
        assert!(!path.exists());

        let _ = fs::remove_dir_all(&repo);
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

        let git_repo = init_temp_repo();
        let repo_str = git_repo.to_string_lossy().into_owned();
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

        let main_handle = ensure_worktree(
            &conn,
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
            &conn,
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
                Some(main_handle.path.to_string_lossy().as_ref()),
                None,
                None,
            )
            .unwrap();

        let reused = ensure_worktree(
            &conn,
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
        assert!(!main_handle.path.exists());
        // The primary checkout is never removed.
        remove_git_worktree(&git_repo, &git_repo).unwrap();
        assert!(git_repo.exists());

        let _ = fs::remove_dir_all(&git_repo);
        let _ = fs::remove_dir_all(&data_root);
        cleanup_test_dir(&db_dir);
        clear_data_root_override();
    }
}
