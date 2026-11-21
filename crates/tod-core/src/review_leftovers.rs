//! Files a review round leaves in the checkout (`doc/lifecycle/find-it-first.md`,
//! section 6i). A reviewer may run code to check a hunch, and an untracked
//! scratch test in the checkout fails the project's own checks (ruff, the
//! quality ratchet) and costs the task a whole fix round (ENG-3311). The
//! reviewers are told to keep scratch files outside the checkout
//! (`surface/review.md`); this is the app's backstop.
//!
//! The untracked files, gitignored ones aside, are listed before a round
//! starts ([`untracked`]) and again when it is done; what is new is what the
//! round left ([`left_by_round`]). A review changes no code, so those files
//! are removed ([`remove`]) and the Activity says which, rather than letting
//! the checks or the next reviewer find them.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use tod_store::fleet::{Workdir, worktree};

/// The untracked, not-ignored files of the checkout at `cwd` and of each of
/// its submodules, as paths under `cwd`.
pub fn untracked(cwd: &Workdir) -> Result<BTreeSet<String>> {
    let mut repos = vec![(String::new(), cwd.clone())];
    for rel in worktree::submodule_paths(cwd).context("list the submodules")? {
        let dir = cwd.join(&rel);
        repos.push((rel, dir));
    }
    let mut files = BTreeSet::new();
    for (prefix, repo) in repos {
        let listed = repo.git_output(&["ls-files", "-o", "--exclude-standard", "-z"])?;
        anyhow::ensure!(
            listed.status.success(),
            "git ls-files failed in {repo}: {}",
            String::from_utf8_lossy(&listed.stderr).trim()
        );
        for name in listed.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()) {
            let name = String::from_utf8_lossy(name);
            files.insert(if prefix.is_empty() { name.into_owned() } else { format!("{prefix}/{name}") });
        }
    }
    Ok(files)
}

/// What `after` has that `before` did not: the files a round left.
pub fn left_by_round(before: &[String], after: &BTreeSet<String>) -> Vec<String> {
    let before: BTreeSet<&str> = before.iter().map(String::as_str).collect();
    after.iter().filter(|f| !before.contains(f.as_str())).cloned().collect()
}

/// Delete `files` (paths under `cwd`, as [`untracked`] lists them), then the
/// directories that leaves empty. A file that cannot be removed is an error
/// naming it.
pub fn remove(cwd: &Workdir, files: &[String]) -> Result<()> {
    for chunk in files.chunks(100) {
        let mut args = vec!["-f", "--"];
        args.extend(chunk.iter().map(String::as_str));
        let out = cwd.output("rm", &args).context("remove the files the review left")?;
        anyhow::ensure!(
            out.status.success(),
            "could not remove the files the review left ({}): {}",
            chunk.join(", "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // Directories the files were in, deepest first; one still holding
    // something is left (`rmdir` refuses it).
    let mut dirs: BTreeSet<&str> = BTreeSet::new();
    for file in files {
        let mut path = file.as_str();
        while let Some((parent, _)) = path.rsplit_once('/') {
            dirs.insert(parent);
            path = parent;
        }
    }
    for dir in dirs.into_iter().rev() {
        cwd.output("rmdir", &[dir]).context("remove a directory the review left")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn a_scratch_file_a_round_left_is_found_and_removed_and_nothing_else_is() {
        let dir = tod_test_util::scratch_dir("review-leftovers");
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "t@example.com"]);
        git(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("tracked.py"), "x = 1\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "build/\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);
        // Untracked before the round: not the round's.
        std::fs::write(dir.join("wip.py"), "y = 2\n").unwrap();
        let cwd = Workdir::Host(dir.clone());
        let before: Vec<String> = untracked(&cwd).unwrap().into_iter().collect();
        assert_eq!(before, ["wip.py"]);

        std::fs::create_dir_all(dir.join("kb/tests/model")).unwrap();
        std::fs::write(dir.join("kb/tests/model/test_zz_scratch_review.py"), "def test(): pass\n").unwrap();
        std::fs::create_dir_all(dir.join("build")).unwrap();
        std::fs::write(dir.join("build/out.txt"), "ignored\n").unwrap();
        let left = left_by_round(&before, &untracked(&cwd).unwrap());
        assert_eq!(left, ["kb/tests/model/test_zz_scratch_review.py"], "ignored files and earlier ones are not the round's");

        remove(&cwd, &left).unwrap();
        assert_eq!(untracked(&cwd).unwrap().into_iter().collect::<Vec<_>>(), ["wip.py"]);
        assert!(!dir.join("kb").exists(), "the directory the scratch file was alone in goes too");
        assert!(dir.join("build/out.txt").exists());
        assert!(dir.join("wip.py").exists());
    }
}
