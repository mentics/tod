//! Shared git helpers for worktree activation and module discovery.

use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre::{Context, eyre};

/// Run `git` in `cwd` and return stdout on success.
pub fn git_stdout(cwd: impl AsRef<Path>, args: &[&str]) -> color_eyre::Result<String> {
    let cwd = cwd.as_ref();
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .wrap_err_with(|| format!("failed to run git {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!(
            "git {} failed (in {}): {}",
            args.join(" "),
            cwd.display(),
            stderr.trim()
        ));
    }

    String::from_utf8(output.stdout).wrap_err("git stdout was not valid UTF-8")
}

/// Resolve the git repository toplevel for `cwd`.
pub fn repo_toplevel(cwd: impl AsRef<Path>) -> color_eyre::Result<PathBuf> {
    let out = git_stdout(cwd.as_ref(), &["rev-parse", "--show-toplevel"])
        .wrap_err("resolving git repository root")?;
    Ok(PathBuf::from(out.trim()))
}

/// Basename of the repository root directory (main module name).
pub fn main_repo_name(root: impl AsRef<Path>) -> color_eyre::Result<String> {
    let root = root.as_ref();
    root.file_name()
        .ok_or_else(|| eyre!("git root has no directory name: {}", root.display()))
        .map(|n| n.to_string_lossy().into_owned())
}

/// Submodule `(name, path)` pairs from `.gitmodules` under `root`.
pub fn submodule_entries(root: impl AsRef<Path>) -> color_eyre::Result<Vec<(String, PathBuf)>> {
    let root = root.as_ref();
    let gitmodules = root.join(".gitmodules");
    if !gitmodules.is_file() {
        return Ok(Vec::new());
    }

    let out = match git_stdout(
        root,
        &[
            "config",
            "-f",
            ".gitmodules",
            "--get-regexp",
            r"^submodule\..*\.path$",
        ],
    ) {
        Ok(out) => out,
        Err(_) => return Ok(Vec::new()),
    };

    let mut entries = Vec::new();
    for line in out.lines() {
        let mut parts = line.split_whitespace();
        let key = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");
        if path.is_empty() {
            continue;
        }
        if let Some(name) = key
            .strip_prefix("submodule.")
            .and_then(|rest| rest.strip_suffix(".path"))
            && !name.is_empty()
        {
            entries.push((name.to_string(), PathBuf::from(path)));
        }
    }
    Ok(entries)
}

/// True if local branch `name` exists in `repo`.
pub fn local_branch_exists(repo: impl AsRef<Path>, name: &str) -> color_eyre::Result<bool> {
    let refname = format!("refs/heads/{name}");
    let output = Command::new("git")
        .args(["show-ref", "--verify", "--quiet", &refname])
        .current_dir(repo.as_ref())
        .output()
        .wrap_err("running git show-ref")?;
    Ok(output.status.success())
}

/// Create `branch` if missing, then check it out in `repo`.
pub fn checkout_or_create_branch(repo: impl AsRef<Path>, branch: &str) -> color_eyre::Result<()> {
    let repo = repo.as_ref();
    if branch.is_empty() {
        return Err(eyre!("branch name is empty"));
    }

    if local_branch_exists(repo, branch)? {
        git_stdout(repo, &["checkout", branch])
            .wrap_err_with(|| format!("checking out branch `{branch}` in {}", repo.display()))?;
    } else {
        git_stdout(repo, &["checkout", "-b", branch]).wrap_err_with(|| {
            format!(
                "creating and checking out branch `{branch}` in {}",
                repo.display()
            )
        })?;
    }
    Ok(())
}

/// Drop registrations for worktrees whose directories are gone (`git worktree prune`).
pub fn worktree_prune(repo: impl AsRef<Path>) -> color_eyre::Result<()> {
    let repo = repo.as_ref();
    git_stdout(repo, &["worktree", "prune"])
        .wrap_err_with(|| format!("git worktree prune failed in {}", repo.display()))?;
    Ok(())
}

/// Unregister a worktree path, even if the directory is missing (`git worktree remove --force`).
pub fn worktree_remove_force(
    repo: impl AsRef<Path>,
    worktree_path: impl AsRef<Path>,
) -> color_eyre::Result<()> {
    let repo = repo.as_ref();
    let path = worktree_path.as_ref();
    let path_str = path
        .to_str()
        .ok_or_else(|| eyre!("worktree path is not valid UTF-8: {}", path.display()))?;
    git_stdout(repo, &["worktree", "remove", "--force", path_str]).wrap_err_with(|| {
        format!(
            "git worktree remove --force {} failed in {}",
            path.display(),
            repo.display()
        )
    })?;
    Ok(())
}

/// True if `path` looks like a Treehouse pool worktree (safe to delete as recovery).
pub fn is_treehouse_pool_path(path: impl AsRef<Path>) -> bool {
    path.as_ref()
        .components()
        .any(|c| c.as_os_str() == ".treehouse")
}

/// Delete a leftover Treehouse worktree directory after verifying it is under `.treehouse/`.
pub fn remove_treehouse_pool_dir(path: impl AsRef<Path>) -> color_eyre::Result<()> {
    let path = path.as_ref();
    if !is_treehouse_pool_path(path) {
        return Err(eyre!(
            "refusing to delete {}: not under a `.treehouse` directory",
            path.display()
        ));
    }
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(path)
        .wrap_err_with(|| format!("failed to delete leftover path {}", path.display()))?;
    Ok(())
}

/// Clear a blocking worktree path: try `git worktree remove --force`, then delete leftovers.
pub fn clear_worktree_path(
    repo: impl AsRef<Path>,
    worktree_path: impl AsRef<Path>,
) -> color_eyre::Result<()> {
    let path = worktree_path.as_ref();
    // Prefer the official git removal when the path is registered.
    match worktree_remove_force(repo.as_ref(), path) {
        Ok(()) => {}
        Err(_) if path.exists() => {
            // Not registered (or remove failed); fall through to directory delete.
        }
        Err(err) => {
            // Path already gone and remove failed — treat as cleared.
            if !path.exists() {
                return Ok(());
            }
            return Err(err);
        }
    }
    if path.exists() {
        remove_treehouse_pool_dir(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn init_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        assert!(
            Command::new("git")
                .args(["init"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "user.email", "test@example.com"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "user.name", "Test"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
        fs::write(dir.join("README"), "hi").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "README"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["commit", "-m", "init"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn checkout_creates_and_switches() {
        let dir = std::env::temp_dir().join(format!("tod-gitutil-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        init_repo(&dir);

        checkout_or_create_branch(&dir, "feature/x").unwrap();
        let head = git_stdout(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(head.trim(), "feature/x");

        checkout_or_create_branch(&dir, "temp1").unwrap();
        let head = git_stdout(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(head.trim(), "temp1");

        checkout_or_create_branch(&dir, "feature/x").unwrap();
        let head = git_stdout(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(head.trim(), "feature/x");

        let _ = fs::remove_dir_all(&dir);
    }
}
