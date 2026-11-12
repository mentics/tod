//! Pushing the node's branch: at step boundaries and before a run stops, so
//! a lost sandbox loses no code (design: "Where data lives"; `doc/agentd.md`,
//! "Pushing the branch"). Only the node's own branch is ever pushed: a
//! checkout on a default branch is refused. Credentials come from the
//! sandbox's proxy.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

fn git(workspace: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether `branch` is one a node must never push to: the repository's
/// default branch (`default`, from `origin/HEAD`, when known) or a
/// conventional one.
pub fn is_protected(branch: &str, default: Option<&str>) -> bool {
    default == Some(branch) || matches!(branch, "main" | "master" | "trunk" | "develop")
}

/// Whether `repo`'s HEAD has commits no remote branch has: work only this
/// checkout holds.
fn has_unpushed(repo: &Path) -> Result<bool> {
    Ok(!git(repo, &["rev-list", "-n1", "HEAD", "--not", "--remotes"])?.is_empty())
}

/// The checkout's default branch, from `origin/HEAD`, when known.
fn default_branch(repo: &Path) -> Option<String> {
    git(repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .ok()
        .map(|r| r.strip_prefix("origin/").unwrap_or(&r).to_string())
}

/// Pushes `repo`'s checked-out branch to `origin`, setting it as the
/// upstream. `Ok(false)` when there was nothing to do (detached, or no
/// `origin`); a default branch is an error.
fn push_repo(repo: &Path) -> Result<bool> {
    let branch = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "HEAD" {
        tracing::info!("detached HEAD: nothing to push");
        return Ok(false);
    }
    if git(repo, &["remote"])?.lines().all(|r| r != "origin") {
        tracing::info!("no origin: nothing to push");
        return Ok(false);
    }
    if is_protected(&branch, default_branch(repo).as_deref()) {
        bail!("refusing to push {branch}: it is a default branch, not the node's own");
    }
    git(repo, &["push", "--quiet", "-u", "origin", &format!("HEAD:refs/heads/{branch}")])?;
    Ok(true)
}

/// Every submodule of `workspace`, recursively, deepest first (a submodule's
/// own submodules before it), as absolute paths.
fn submodules_deepest_first(workspace: &Path) -> Result<Vec<std::path::PathBuf>> {
    let listed = git(workspace, &["submodule", "foreach", "--recursive", "--quiet", "echo \"$displaypath\""])?;
    let mut paths: Vec<_> = listed.lines().filter(|l| !l.trim().is_empty()).map(|l| workspace.join(l.trim())).collect();
    paths.reverse();
    Ok(paths)
}

/// Pushes the node's own branch in `workspace` and in each submodule that
/// holds commits no remote has (the agent's work in a submodule is lost with
/// the sandbox otherwise: the parent only records its commit). Submodules go
/// first, so the commit the parent records is on the remote when the parent
/// is. Every repository is tried; the failures come back together, naming each
/// repository: a submodule on a detached HEAD or on a default branch with
/// commits no remote has is a failure, never skipped.
pub fn push_branch(workspace: &Path) -> Result<()> {
    let mut failures = Vec::new();
    for sub in submodules_deepest_first(workspace)? {
        let name = sub.strip_prefix(workspace).unwrap_or(&sub).display().to_string();
        let result = (|| -> Result<()> {
            if !sub.join(".git").exists() || !has_unpushed(&sub)? {
                return Ok(());
            }
            if !push_repo(&sub)? {
                bail!("it has commits no remote has, but nothing was pushed (detached HEAD or no origin)");
            }
            Ok(())
        })();
        if let Err(err) = result {
            failures.push(format!("submodule {name}: {err:#}"));
        }
    }
    if let Err(err) = push_repo(workspace) {
        failures.push(format!("{err:#}"));
    }
    if !failures.is_empty() {
        bail!("{}", failures.join("; "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_protected, push_branch};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "protocol.file.allow=always"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A workspace with one submodule (`kb`), both cloned from bare remotes
    /// and on the node's branch. Returns (checkout, workspace remote, kb remote).
    fn fixture() -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("tod-sup-git-{}", std::process::id()))
            .join(format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        let (sub_remote, ws_remote) = (root.join("kb.git"), root.join("ws.git"));
        run(&root, &["init", "-q", "--bare", "-b", "main", sub_remote.to_str().unwrap()]);
        run(&root, &["init", "-q", "--bare", "-b", "main", ws_remote.to_str().unwrap()]);
        let subseed = root.join("subseed");
        run(&root, &["clone", "-q", sub_remote.to_str().unwrap(), subseed.to_str().unwrap()]);
        std::fs::write(subseed.join("a"), "a").unwrap();
        run(&subseed, &["add", "."]);
        run(&subseed, &["commit", "-qm", "kb"]);
        run(&subseed, &["push", "-q", "origin", "HEAD:main"]);
        let seed = root.join("seed");
        run(&root, &["clone", "-q", ws_remote.to_str().unwrap(), seed.to_str().unwrap()]);
        run(&seed, &["submodule", "add", "-q", sub_remote.to_str().unwrap(), "kb"]);
        run(&seed, &["commit", "-qm", "ws"]);
        run(&seed, &["push", "-q", "origin", "HEAD:main"]);
        let work = root.join("work");
        run(&root, &["clone", "-q", "--recurse-submodules", ws_remote.to_str().unwrap(), work.to_str().unwrap()]);
        run(&work, &["checkout", "-q", "-b", "task/x"]);
        run(&work.join("kb"), &["checkout", "-q", "-b", "task/x"]);
        (work, ws_remote, sub_remote)
    }

    fn commit_in_kb(work: &Path) {
        std::fs::write(work.join("kb/test_new.py"), "x").unwrap();
        run(&work.join("kb"), &["add", "."]);
        run(&work.join("kb"), &["commit", "-qm", "the node's work"]);
    }

    #[test]
    fn a_submodules_commits_are_pushed_with_the_node_branch() {
        let (work, ws_remote, sub_remote) = fixture();
        commit_in_kb(&work);
        push_branch(&work).unwrap();
        assert!(run(&ws_remote, &["branch", "--list", "task/x"]).contains("task/x"));
        assert_eq!(run(&sub_remote, &["rev-parse", "task/x"]), run(&work.join("kb"), &["rev-parse", "HEAD"]));
    }

    #[test]
    fn a_submodule_with_nothing_of_its_own_gets_no_branch() {
        let (work, _ws_remote, sub_remote) = fixture();
        push_branch(&work).unwrap();
        assert!(run(&sub_remote, &["branch", "--list", "task/x"]).is_empty());
    }

    #[test]
    fn unpushable_submodule_work_is_an_error_naming_it_and_the_rest_still_goes() {
        let (work, ws_remote, _sub_remote) = fixture();
        let kb = work.join("kb");
        run(&kb, &["checkout", "-q", "--detach"]);
        commit_in_kb(&work);
        let err = format!("{:#}", push_branch(&work).unwrap_err());
        assert!(err.contains("submodule kb") && err.contains("detached"), "{err}");
        assert!(run(&ws_remote, &["branch", "--list", "task/x"]).contains("task/x"), "the parent was still pushed");
    }

    #[test]
    fn default_and_conventional_branches_are_protected() {
        assert!(is_protected("main", None));
        assert!(is_protected("release", Some("release")));
        assert!(!is_protected("task/fix-login", Some("main")));
    }
}
