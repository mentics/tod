//! Configured fixers (`doc/requests/design.md` 4.2.3, `doc/lifecycle/checks.md`):
//! a command that fixes a failing check mechanically (a formatter, a lint
//! autofix). tod runs it in the check's directory, commits what it changed as
//! its own mechanical commit, and reads only that check again; no agent turn,
//! and no review of a change a tool made.
//!
//! Two callers: tod's own checks (`checks::run_stale_notifying`, a
//! [`tod_store::lifecycle_config::Check::fix`]) and CI checks in the babysitter
//! (`pr_readiness.check_fixes`, which also pushes the commit itself).

use anyhow::{Context, Result};
use tod_store::check_runs::Verdict;
use tod_store::fleet::Workdir;

/// The first line of a mechanical commit's message: how tod's own fixer
/// commits are told from anyone's.
pub const COMMIT_SUBJECT_PREFIX: &str = "tod: fix ";

/// What running a fixer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixOutcome {
    /// It changed files and tod committed them (the repository's new head).
    Committed { head: String },
    /// It changed files, but the tree already had uncommitted work that was
    /// not the fixer's, so tod left it all uncommitted: committing would
    /// put someone else's change into a commit marked mechanical.
    ChangedUncommitted,
    /// It ran and changed nothing.
    NoChange,
    /// It failed or did not finish; the output says why.
    Failed(String),
}

impl FixOutcome {
    /// The files changed, committed or not.
    pub fn changed(&self) -> bool {
        matches!(self, Self::Committed { .. } | Self::ChangedUncommitted)
    }
}

/// Run `command` in `at` (the check's directory) and commit what it changed
/// in the repository holding `at`, named `name` in the commit's message.
/// Errors only when git itself fails; a fixer that fails is
/// [`FixOutcome::Failed`], never hidden.
pub fn run_fix(at: &Workdir, name: &str, command: &str) -> Result<FixOutcome> {
    let command = command.trim();
    anyhow::ensure!(!command.is_empty(), "no fix command to run for {name}");
    let was_clean = at
        .git(&["status", "--porcelain", "--ignore-submodules=all"])
        .context("read the tree before the fix")?
        .is_empty();
    let (verdict, output) = crate::checks::run_shell(at, command, &[], crate::checks::CHECK_TIMEOUT);
    if verdict != Verdict::Passed {
        return Ok(FixOutcome::Failed(format!(
            "the fix `{command}` {}: {output}",
            if verdict == Verdict::Failed { "failed" } else { "could not run" }
        )));
    }
    let after = at.git(&["status", "--porcelain", "--ignore-submodules=all"]).context("read the tree after the fix")?;
    if after.is_empty() {
        return Ok(FixOutcome::NoChange);
    }
    if !was_clean {
        return Ok(FixOutcome::ChangedUncommitted);
    }
    at.git(&["add", "-A"]).context("stage the fix")?;
    // A submodule pointer is never committed by tod.
    for line in at.git(&["diff", "--cached", "--raw", "--no-renames"])?.lines() {
        let Some((meta, path)) = line.split_once('\t') else { continue };
        let mut modes = meta.trim_start_matches(':').split(' ');
        if modes.next() == Some("160000") || modes.next() == Some("160000") {
            at.git(&["reset", "-q", "--", path])?;
        }
    }
    if at.git(&["diff", "--cached", "--name-only"])?.is_empty() {
        return Ok(FixOutcome::NoChange);
    }
    let message = format!("{COMMIT_SUBJECT_PREFIX}{name}\n\nMechanical: tod ran `{command}` and committed what it changed.");
    at.git(&["commit", "--no-verify", "-m", &message]).context("commit the fix")?;
    let head = at.git(&["rev-parse", "HEAD"])?;
    Ok(FixOutcome::Committed { head })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::process::Command;

    /// A repository with one commit holding `a.txt`.
    pub(crate) fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git").args(args).current_dir(dir.path()).output().unwrap();
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("a.txt"), "ugly\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "init"]);
        dir
    }

    #[test]
    fn a_fix_that_changes_files_is_committed_as_its_own_commit() {
        let dir = repo();
        let at = Workdir::host(dir.path());
        let out = run_fix(&at, "format", "echo pretty > a.txt").unwrap();
        let FixOutcome::Committed { head } = out else { panic!("{out:?}") };
        assert_eq!(at.git(&["rev-parse", "HEAD"]).unwrap(), head);
        assert!(at.git(&["log", "-1", "--format=%s"]).unwrap().starts_with(COMMIT_SUBJECT_PREFIX));
        assert!(at.git(&["status", "--porcelain"]).unwrap().is_empty());
    }

    #[test]
    fn a_fix_that_changes_nothing_commits_nothing() {
        let dir = repo();
        let at = Workdir::host(dir.path());
        let before = at.git(&["rev-parse", "HEAD"]).unwrap();
        assert_eq!(run_fix(&at, "format", "true").unwrap(), FixOutcome::NoChange);
        assert_eq!(at.git(&["rev-parse", "HEAD"]).unwrap(), before);
    }

    #[test]
    fn a_failing_fix_is_reported_with_its_output() {
        let dir = repo();
        let at = Workdir::host(dir.path());
        let FixOutcome::Failed(why) = run_fix(&at, "format", "echo boom; exit 3").unwrap() else { panic!() };
        assert!(why.contains("boom") && why.contains("failed"), "{why}");
    }

    #[test]
    fn a_fix_in_a_tree_with_other_uncommitted_work_leaves_it_all_uncommitted() {
        let dir = repo();
        std::fs::write(dir.path().join("b.txt"), "mine\n").unwrap();
        let at = Workdir::host(dir.path());
        let before = at.git(&["rev-parse", "HEAD"]).unwrap();
        assert_eq!(run_fix(&at, "format", "echo pretty > a.txt").unwrap(), FixOutcome::ChangedUncommitted);
        assert_eq!(at.git(&["rev-parse", "HEAD"]).unwrap(), before);
    }
}
