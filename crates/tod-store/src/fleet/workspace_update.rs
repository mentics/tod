//! "Update workspace to latest main": brings a node's workspace checkout up to
//! the latest `main` from `origin`, for development, when the workspace holds
//! files (skills, process docs) that changed after the checkout was made and
//! the node's own task does not change it.
//!
//! It only ever fast-forwards, and only a clean checkout: nothing is
//! discarded, merged or rebased. Submodules are left where they are, since a
//! node's repository is one of them and carries the work.

use crate::fleet::FleetStore;
use crate::fleet::provision::launch_cwd_if_made;
use crate::fleet::workdir::Workdir;
use anyhow::{Context, Result, bail};

/// Updates `node_id`'s workspace (the directory its launches run in, on this
/// machine, in its container or in its sandbox) and says what it did. Runs
/// git there, so never call it on the UI thread.
pub fn update_workspace_to_main(fleet: &FleetStore, node_id: &str) -> Result<String> {
    let dir = launch_cwd_if_made(fleet, node_id)?
        .context("this node has no workspace yet: it is made the first time something launches from it")?;
    update_to_main(&dir)
}

/// [`update_workspace_to_main`] for a directory.
pub fn update_to_main(dir: &Workdir) -> Result<String> {
    let branch = dir.git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    let dirty = dir.git(&["status", "--porcelain", "--ignore-submodules=all"])?;
    if !dirty.is_empty() {
        let shown: Vec<&str> = dirty.lines().take(5).collect();
        bail!(
            "the workspace at {dir} has uncommitted changes, so it was left alone:\n{}{}",
            shown.join("\n"),
            if dirty.lines().count() > shown.len() { "\n…" } else { "" }
        );
    }
    dir.git(&["fetch", "-q", "origin", "main"]).context("fetch main from origin")?;
    let before = dir.git(&["rev-parse", "--short", "HEAD"])?;
    let latest = dir.git(&["rev-parse", "--short", "FETCH_HEAD"])?;
    if is_ancestor(dir, "FETCH_HEAD", "HEAD")? {
        return Ok(format!("The workspace ({branch}) already has the latest main ({latest})."));
    }
    if !is_ancestor(dir, "HEAD", "FETCH_HEAD")? {
        bail!(
            "the workspace at {dir} ({branch} at {before}) has commits that origin/main ({latest}) does not, so it \
             cannot be fast-forwarded and was left alone. Update it by hand."
        );
    }
    let new_commits = dir.git(&["rev-list", "--count", "HEAD..FETCH_HEAD"])?;
    dir.git(&["merge", "--ff-only", "-q", "FETCH_HEAD"])?;
    Ok(format!("Updated the workspace ({branch}) from {before} to {latest}: {new_commits} new commit(s) from main."))
}

fn is_ancestor(dir: &Workdir, ancestor: &str, of: &str) -> Result<bool> {
    let out = dir.git_output(&["merge-base", "--is-ancestor", ancestor, of])?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("git merge-base failed in {dir}: {}", String::from_utf8_lossy(&out.stderr).trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// An origin with one commit on `main`, a checkout of it, and a way to
    /// push another commit to the origin.
    struct Repos {
        _tmp: tempfile::TempDir,
        origin: std::path::PathBuf,
        work: std::path::PathBuf,
    }

    impl Repos {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let origin = tmp.path().join("origin.git");
            let seed = tmp.path().join("seed");
            let work = tmp.path().join("work");
            std::fs::create_dir_all(&origin).unwrap();
            git(&origin, &["init", "-q", "--bare", "-b", "main"]);
            git(tmp.path(), &["clone", "-q", origin.to_str().unwrap(), seed.to_str().unwrap()]);
            std::fs::write(seed.join("a"), "1").unwrap();
            git(&seed, &["add", "."]);
            git(&seed, &["commit", "-q", "-m", "one"]);
            git(&seed, &["push", "-q", "origin", "HEAD:main"]);
            git(tmp.path(), &["clone", "-q", origin.to_str().unwrap(), work.to_str().unwrap()]);
            Repos { _tmp: tmp, origin, work }
        }

        fn push_to_main(&self, file: &str) {
            let other = self._tmp.path().join(format!("other-{file}"));
            git(self._tmp.path(), &["clone", "-q", self.origin.to_str().unwrap(), other.to_str().unwrap()]);
            std::fs::write(other.join(file), "x").unwrap();
            git(&other, &["add", "."]);
            git(&other, &["commit", "-q", "-m", file]);
            git(&other, &["push", "-q", "origin", "HEAD:main"]);
        }
    }

    #[test]
    fn a_clean_checkout_fast_forwards_to_main() {
        let r = Repos::new();
        let dir = Workdir::host(&r.work);
        assert!(update_to_main(&dir).unwrap().contains("already has the latest main"));
        r.push_to_main("b");
        r.push_to_main("c");
        let said = update_to_main(&dir).unwrap();
        assert!(said.contains("2 new commit(s)"), "{said}");
        assert!(r.work.join("c").exists());
    }

    #[test]
    fn a_branch_of_its_own_is_fast_forwarded_too() {
        let r = Repos::new();
        git(&r.work, &["checkout", "-q", "-b", "task/x"]);
        r.push_to_main("b");
        update_to_main(&Workdir::host(&r.work)).unwrap();
        assert!(r.work.join("b").exists());
    }

    #[test]
    fn uncommitted_changes_leave_it_alone() {
        let r = Repos::new();
        r.push_to_main("b");
        std::fs::write(r.work.join("a"), "changed").unwrap();
        let err = update_to_main(&Workdir::host(&r.work)).unwrap_err().to_string();
        assert!(err.contains("uncommitted changes"), "{err}");
        assert!(!r.work.join("b").exists());
    }

    #[test]
    fn commits_main_lacks_leave_it_alone() {
        let r = Repos::new();
        std::fs::write(r.work.join("mine"), "1").unwrap();
        git(&r.work, &["add", "."]);
        git(&r.work, &["commit", "-q", "-m", "mine"]);
        r.push_to_main("b");
        let err = update_to_main(&Workdir::host(&r.work)).unwrap_err().to_string();
        assert!(err.contains("cannot be fast-forwarded"), "{err}");
        assert!(r.work.join("mine").exists() && !r.work.join("b").exists());
    }
}
