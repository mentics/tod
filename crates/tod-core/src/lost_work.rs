//! Lost work: a node past implementation whose code is gone.
//!
//! The one place the app moves a node back to `active` by itself. Everywhere
//! else a state that no longer holds is the user's to move
//! ([`crate::lifecycle_validity`]); this rule applies only to a condition the
//! checkout proves, so nobody has anything to decide:
//!
//! * the node is in `verifying`, `review` or `pr`, so its implementation is
//!   claimed done;
//! * at least one of its implementation plan steps is `implemented` or
//!   `verified`;
//! * and its Files checkout holds none of that work: in the checkout and in
//!   every initialized submodule (recursively) the branch has no commit beyond
//!   its base (the repository's default branch; for a submodule, the commit
//!   its parent pinned there before the node's work), and nothing is
//!   uncommitted.
//!
//! This is what a sandbox replaced before its branch was pushed leaves
//! behind (the supervisor now pushes every repository at each step boundary,
//! but work lost before that fix, or to a deleted sandbox, is still lost).
//! A reviewer or verifier who finds the checkout missing the change used to
//! have only the user to ask, and the answer "send it back" moved nothing.
//!
//! [`check`] reads the checkout; it says nothing unless every repository is
//! proven untouched (a repository whose base cannot be told, such as one with
//! no `origin`, proves nothing). [`reopen`] then sets the node back to
//! `active`, makes its implemented and verified steps (and the verified
//! verdicts) open work again, withdraws the `broken` questions a reviewer or
//! verifier asked about it, and leaves a note on the node's activity saying
//! why. The autopilot runs both ([`crate::autopilot`]) before it spends a turn
//! in one of those states, and again when such an agent has asked a `broken`
//! question there, so an agent that finds the code missing needs only to ask
//! (`tod-cli decisions ask --reason broken`) and never to offer "send it
//! back" as an option.

use crate::lifecycle;
use anyhow::{Context, Result};
use tod_store::decisions::{DecisionRepo, REASON_BROKEN};
use tod_store::fleet::{FleetMutation, FleetStore, Workdir};
use tod_store::interview::InterviewCommand;
use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};
use tod_store::outline::repos::PlanStepRepo;
use tod_store::outline::repos::plan_steps::{PHASE_ACTIVE, PHASE_VERIFYING, STATUS_IMPLEMENTED, STATUS_VERIFIED};
use uuid::Uuid;

/// The states in which a node's implementation is claimed done, so lost work
/// is looked for.
pub const STATES: [&str; 3] = ["verifying", "review", "pr"];

/// The protocols of the agents that work in [`STATES`]: a `broken` question
/// from one of them may be about lost work.
const PROTOCOLS: [&str; 3] = ["verification", "review", "pr"];

/// Who the app's own writes here are attributed to.
const ACTOR: &str = "autopilot";

/// The title of the note left on the node's activity.
pub const NOTE_TITLE: &str = "Lost work: implementation reopened";

/// What one repository's checkout holds of the node's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoState {
    /// No commit beyond the base and nothing uncommitted.
    Untouched,
    /// At least one commit beyond the base.
    OwnCommits,
    /// Uncommitted changes or untracked files.
    Dirty,
    /// The node's branch is on `origin` at a commit other than the base (merged
    /// into it, or behind it): its work is there.
    RemoteBranch,
    /// There is no `origin` to compare with; it proves nothing either way.
    Unknown(String),
}

/// What [`check`] found: lost work, with the evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    /// Each repository looked at (`.` is the checkout itself).
    pub repos: Vec<String>,
    /// How many implementation steps are marked done.
    pub done_steps: usize,
}

impl Proof {
    /// Why the node is being reopened, in a sentence for the node's activity
    /// and the plan steps' notes.
    pub fn why(&self, state: &str) -> String {
        format!(
            "The node was in {state} with {} implementation step(s) marked done, but its branch has no commits of its \
             own beyond its base in {} and nothing uncommitted: the code was lost (for example with a sandbox that was \
             replaced before its branch was pushed). The steps are open again; implement them once more.",
            self.done_steps,
            self.repos.join(", ")
        )
    }
}

/// `git args` in `repo`, its trimmed output; any failure is an error naming
/// the command.
fn git_text(repo: &Workdir, args: &[&str]) -> Result<String> {
    repo.git(args)
}

/// A git query that answers "there is no such thing" with exit status 1
/// (`rev-parse --verify -q`, `config --get`): `None` then, an error for any
/// other failure.
fn git_ref(repo: &Workdir, args: &[&str]) -> Result<Option<String>> {
    let out = repo.git_output(args).with_context(|| format!("run git {} in {repo}", args.join(" ")))?;
    if out.status.success() {
        return Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_string()));
    }
    if out.status.code() == Some(1) {
        return Ok(None);
    }
    anyhow::bail!("git {} in {repo} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())
}

/// The repository's default branch as a ref, from `origin`. `None` only when
/// it has no `origin` at all; an `origin` with no default branch ref is an
/// error (the checkout never fetched it).
fn default_ref(repo: &Workdir) -> Result<Option<String>> {
    if git_ref(repo, &["config", "--get", "remote.origin.url"])?.is_none() {
        return Ok(None);
    }
    for candidate in ["refs/remotes/origin/HEAD", "refs/remotes/origin/main", "refs/remotes/origin/master"] {
        if git_ref(repo, &["rev-parse", "--verify", "-q", &format!("{candidate}^{{commit}}")])?.is_some() {
            return Ok(Some(candidate.to_string()));
        }
    }
    anyhow::bail!("{repo} has an origin but none of refs/remotes/origin/HEAD, main or master, so the node's base cannot be told; fetch origin")
}

/// The commit a submodule at `rel` in `parent` was pinned to before the
/// node's work: its gitlink at the merge base of the parent's HEAD and its
/// default branch. `None` when the parent has no origin or did not have it.
fn pinned(parent: &Workdir, rel: &str) -> Result<Option<String>> {
    let Some(base) = default_ref(parent)? else {
        return Ok(None);
    };
    let merge_base = git_text(parent, &["merge-base", "HEAD", &base])?;
    Ok(git_ref(parent, &["rev-parse", "--verify", "-q", &format!("{merge_base}:{rel}")])?.filter(|s| !s.is_empty()))
}

/// What `repo` holds, against `base` (a ref or commit; `None` when there is no
/// origin). Commits on the node's branch (`branch`: local or on `origin`)
/// count wherever the checkout sits.
fn state_of(repo: &Workdir, base: Option<&str>, what: &str, branch: Option<&str>) -> Result<RepoState> {
    if !git_text(repo, &["status", "--porcelain"])?.is_empty() {
        return Ok(RepoState::Dirty);
    }
    let Some(base) = base else {
        return Ok(RepoState::Unknown(format!("{what} has no origin to compare with")));
    };
    let mut args = vec!["rev-list".to_string(), "--count".to_string(), "HEAD".to_string()];
    let mut remote_tip = None;
    if let Some(branch) = branch {
        for r in [format!("refs/heads/{branch}"), format!("refs/remotes/origin/{branch}")] {
            if let Some(sha) = git_ref(repo, &["rev-parse", "--verify", "-q", &format!("{r}^{{commit}}")])? {
                if r.starts_with("refs/remotes/") {
                    remote_tip = Some(sha);
                }
                args.push(r);
            }
        }
    }
    args.push("--not".to_string());
    args.push(base.to_string());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let counted = git_text(repo, &args)?;
    let count: u64 = counted
        .parse()
        .with_context(|| format!("git {} in {repo} printed `{counted}`, not a count", args.join(" ")))?;
    if count > 0 {
        return Ok(RepoState::OwnCommits);
    }
    // The node's branch is on origin at a commit other than the base (merged
    // into it, or left behind it): its work is there, not lost.
    if let Some(tip) = remote_tip {
        let base_tip = git_text(repo, &["rev-parse", &format!("{base}^{{commit}}")])?;
        if tip != base_tip {
            return Ok(RepoState::RemoteBranch);
        }
    }
    Ok(RepoState::Untouched)
}

/// What the checkout at `cwd` and each initialized submodule hold; `branch` is
/// the node's branch, else the one checked out. Empty when `cwd` is not a git
/// work tree (there is no branch to have lost).
pub fn inspect(cwd: &Workdir, branch: Option<&str>) -> Result<Vec<(String, RepoState)>> {
    let probe = cwd
        .git_output(&["rev-parse", "--is-inside-work-tree"])
        .with_context(|| format!("run git rev-parse in {cwd}"))?;
    if !probe.status.success() {
        let stderr = String::from_utf8_lossy(&probe.stderr);
        if stderr.contains("not a git repository") {
            return Ok(Vec::new());
        }
        anyhow::bail!("git rev-parse --is-inside-work-tree in {cwd} failed: {}", stderr.trim());
    }
    if String::from_utf8_lossy(&probe.stdout).trim() != "true" {
        return Ok(Vec::new());
    }
    let checked_out = git_text(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch
        .map(str::to_string)
        .or_else(|| Some(checked_out).filter(|b| b != "HEAD" && !matches!(b.as_str(), "main" | "master")));
    let branch = branch.as_deref();
    let mut out = vec![(".".to_string(), state_of(cwd, default_ref(cwd)?.as_deref(), "the checkout", branch)?)];
    // No `.gitmodules` is the only way to have none; a listing that fails is an error.
    let subs: Vec<String> = if git_text(cwd, &["ls-files", "--", ".gitmodules"])?.is_empty() {
        Vec::new()
    } else {
        git_text(cwd, &["submodule", "foreach", "--recursive", "--quiet", "echo \"$displaypath\""])?
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    };
    for rel in &subs {
        // The nearest listed ancestor is the repository that records it.
        let parent_rel = subs.iter().filter(|p| rel.starts_with(&format!("{p}/"))).max_by_key(|p| p.len());
        let (parent, in_parent) = match parent_rel {
            Some(p) => (cwd.join(p), rel[p.len() + 1..].to_string()),
            None => (cwd.clone(), rel.clone()),
        };
        let base = pinned(&parent, &in_parent)?;
        out.push((rel.clone(), state_of(&cwd.join(rel), base.as_deref(), &format!("submodule {rel}"), branch)?));
    }
    Ok(out)
}

/// Whether `repos` prove the work lost: there is at least one, and each is
/// untouched.
pub fn is_lost(repos: &[(String, RepoState)]) -> bool {
    !repos.is_empty() && repos.iter().all(|(_, s)| *s == RepoState::Untouched)
}

/// How many of the node's implementation (and verification) steps are marked
/// done: the ones [`reopen`] opens again.
pub fn done_steps(fleet: &FleetStore, node: Uuid) -> Result<usize> {
    fleet.read(|conn| {
        Ok(PlanStepRepo::new(conn)
            .list_for_node(node)?
            .iter()
            .filter(|s| matches!(s.phase.as_str(), PHASE_ACTIVE | PHASE_VERIFYING) && matches!(s.status.as_str(), STATUS_IMPLEMENTED | STATUS_VERIFIED))
            .count())
    })
}

/// The `broken` questions pending on the node from the agents that work in
/// [`STATES`].
pub fn broken_questions(fleet: &FleetStore, node: Uuid) -> Result<Vec<Uuid>> {
    fleet.read(|conn| {
        Ok(DecisionRepo::new(conn)
            .list_pending_for_node(node)?
            .into_iter()
            .filter(|d| d.reason == REASON_BROKEN && d.protocol.as_deref().is_some_and(|p| PROTOCOLS.contains(&p)))
            .map(|d| d.id)
            .collect())
    })
}

/// Whether the node, in `state`, has lost its work. `None` when the state is
/// not one of [`STATES`], no implementation step is done, or there is no
/// checkout to look at (Files not set up or not made: a launch says so
/// itself).
pub fn check(fleet: &FleetStore, node: Uuid, state: &str) -> Result<Option<Proof>> {
    if !STATES.contains(&state) {
        return Ok(None);
    }
    let done = done_steps(fleet, node)?;
    if done == 0 {
        return Ok(None);
    }
    // Not made yet: nothing to look at. Any error (a stale location, Files
    // missing) is the caller's to see.
    let Some(cwd) = tod_store::fleet::provision::launch_cwd_if_made(fleet, &node.to_string())? else {
        return Ok(None);
    };
    let branch = fleet.resolve_files_for_node(&node.to_string())?.and_then(|files| files.branch().map(str::to_string));
    let repos = inspect(&cwd, branch.as_deref())?;
    Ok(is_lost(&repos).then(|| Proof { repos: repos.into_iter().map(|(name, _)| name).collect(), done_steps: done }))
}

/// Reopen the implementation of a node [`check`] found lost: back to `active`
/// first (so a restart in the middle finds it again from the steps), then the
/// steps and verdicts, the questions about it, and the note.
pub fn reopen(fleet: &FleetStore, node: Uuid, state: &str, proof: &Proof) -> Result<()> {
    let why = proof.why(state);
    let questions = broken_questions(fleet, node)?;
    lifecycle::set_lifecycle(fleet, node, "active")?;
    fleet.interview(ACTOR, InterviewCommand::ReopenLostWork { node_id: node, why: why.clone() })?;
    for decision_id in questions {
        fleet.interview(ACTOR, InterviewCommand::WithdrawDecision { decision_id })?;
    }
    let entry = TimelineEntry::new(node, KIND_NOTE, "active", NOTE_TITLE, false, format!("From {state}. {why}"));
    fleet.enqueue(FleetMutation::RecordTimeline(Box::new(entry)))?;
    fleet.writer().flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// A clone of a workspace with one submodule (`kb`), both from bare
    /// remotes, on the node's branch. Returns (tempdir, checkout).
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
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
        (dir, work)
    }

    fn states(work: &Path) -> Vec<(String, RepoState)> {
        inspect(&Workdir::host(work), None).unwrap()
    }

    #[test]
    fn a_checkout_with_nothing_of_the_nodes_is_lost_work() {
        let (_dir, work) = fixture();
        let repos = states(&work);
        assert_eq!(repos.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), [".", "kb"]);
        assert!(is_lost(&repos), "{repos:?}");
    }

    #[test]
    fn a_commit_in_the_submodule_alone_is_not_lost() {
        let (_dir, work) = fixture();
        std::fs::write(work.join("kb/new"), "x").unwrap();
        run(&work.join("kb"), &["add", "."]);
        run(&work.join("kb"), &["commit", "-qm", "work"]);
        let repos = states(&work);
        // The checkout shows the submodule as modified, and the submodule has a commit of its own.
        assert_eq!(repos[1].1, RepoState::OwnCommits, "{repos:?}");
        assert!(!is_lost(&repos));
    }

    #[test]
    fn a_commit_in_the_checkout_is_not_lost_even_when_pushed() {
        let (_dir, work) = fixture();
        std::fs::write(work.join("f"), "x").unwrap();
        run(&work, &["add", "f"]);
        run(&work, &["commit", "-qm", "work"]);
        run(&work, &["push", "-q", "origin", "HEAD:refs/heads/task/x"]);
        let repos = states(&work);
        assert_eq!(repos[0].1, RepoState::OwnCommits);
        assert!(!is_lost(&repos));
    }

    #[test]
    fn uncommitted_work_is_not_lost() {
        let (_dir, work) = fixture();
        std::fs::write(work.join("untracked"), "x").unwrap();
        let repos = states(&work);
        assert_eq!(repos[0].1, RepoState::Dirty);
        assert!(!is_lost(&repos));
    }

    #[test]
    fn a_checkout_with_no_origin_proves_nothing() {
        let (_dir, work) = fixture();
        run(&work, &["remote", "remove", "origin"]);
        let repos = states(&work);
        assert!(matches!(repos[0].1, RepoState::Unknown(_)), "{repos:?}");
        assert!(!is_lost(&repos));
    }

    #[test]
    fn a_directory_that_is_not_a_repository_is_nothing_to_look_at() {
        let dir = tempfile::tempdir().unwrap();
        assert!(states(dir.path()).is_empty());
        assert!(!is_lost(&[]));
    }

    fn commit_in(dir: &Path, file: &str) {
        std::fs::write(dir.join(file), file).unwrap();
        run(dir, &["add", file]);
        run(dir, &["commit", "-qm", file]);
    }

    #[test]
    fn a_failed_submodule_listing_is_an_error_not_no_submodules() {
        let (_dir, work) = fixture();
        std::fs::write(work.join(".gitmodules"), "[submodule \"kb\"\n").unwrap();
        let err = format!("{:#}", inspect(&Workdir::host(&work), None).unwrap_err());
        // Git refuses a work tree whose .gitmodules is unreadable, at whichever
        // call comes first; it is never read as "no submodules".
        assert!(err.contains("git "), "{err}");
    }

    #[test]
    fn work_on_the_nodes_branch_is_found_though_the_checkout_sits_on_main() {
        let (_dir, work) = fixture();
        commit_in(&work, "w");
        run(&work, &["push", "-q", "origin", "HEAD:refs/heads/task/x"]);
        run(&work, &["checkout", "-q", "main"]);
        // The local branch has it.
        let repos = inspect(&Workdir::host(&work), Some("task/x")).unwrap();
        assert_eq!(repos[0].1, RepoState::OwnCommits, "{repos:?}");
        // Only origin has it.
        run(&work, &["branch", "-q", "-D", "task/x"]);
        let repos = inspect(&Workdir::host(&work), Some("task/x")).unwrap();
        assert_eq!(repos[0].1, RepoState::OwnCommits, "{repos:?}");
        assert!(!is_lost(&repos));
    }

    #[test]
    fn a_branch_merged_into_main_and_left_behind_it_is_not_lost() {
        let (_dir, work) = fixture();
        commit_in(&work, "w");
        run(&work, &["push", "-q", "origin", "HEAD:refs/heads/task/x"]);
        run(&work, &["checkout", "-q", "main"]);
        run(&work, &["merge", "-q", "--ff-only", "task/x"]);
        commit_in(&work, "later");
        run(&work, &["push", "-q", "origin", "main"]);
        run(&work, &["branch", "-q", "-D", "task/x"]);
        run(&work, &["fetch", "-q", "origin"]);
        let repos = inspect(&Workdir::host(&work), Some("task/x")).unwrap();
        assert_eq!(repos[0].1, RepoState::RemoteBranch, "{repos:?}");
        assert!(!is_lost(&repos));
    }

    #[test]
    fn an_origin_without_a_default_branch_ref_is_an_error() {
        let (_dir, work) = fixture();
        run(&work, &["update-ref", "-d", "refs/remotes/origin/main"]);
        run(&work, &["update-ref", "-d", "refs/remotes/origin/HEAD"]);
        let err = format!("{:#}", inspect(&Workdir::host(&work), None).unwrap_err());
        assert!(err.contains("fetch origin"), "{err}");
    }
}
