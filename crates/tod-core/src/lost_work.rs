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
use tod_store::outline::repos::plan_steps::{PHASE_ACTIVE, STATUS_IMPLEMENTED, STATUS_VERIFIED};
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
    /// There is nothing to compare with; it proves nothing either way.
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

fn git_ok(repo: &Workdir, args: &[&str]) -> Result<Option<String>> {
    let out = repo.git_output(args).with_context(|| format!("run git {} in {repo}", args.join(" ")))?;
    Ok(out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

/// The repository's default branch as a ref, from `origin`.
fn default_ref(repo: &Workdir) -> Result<Option<String>> {
    for candidate in ["refs/remotes/origin/HEAD", "refs/remotes/origin/main", "refs/remotes/origin/master"] {
        if git_ok(repo, &["rev-parse", "--verify", "-q", &format!("{candidate}^{{commit}}")])?.is_some() {
            return Ok(Some(candidate.to_string()));
        }
    }
    Ok(None)
}

/// The commit a submodule at `rel` in `parent` was pinned to before the
/// node's work: its gitlink at the merge base of the parent's HEAD and its
/// default branch.
fn pinned(parent: &Workdir, rel: &str) -> Result<Option<String>> {
    let Some(base) = default_ref(parent)? else {
        return Ok(None);
    };
    let Some(merge_base) = git_ok(parent, &["merge-base", "HEAD", &base])? else {
        return Ok(None);
    };
    Ok(git_ok(parent, &["rev-parse", "--verify", "-q", &format!("{merge_base}:{rel}")])?.filter(|s| !s.is_empty()))
}

/// What `repo` holds, against `base` (a ref or commit), `None` when it is not
/// known.
fn state_of(repo: &Workdir, base: Option<&str>, what: &str) -> Result<RepoState> {
    let Some(status) = git_ok(repo, &["status", "--porcelain"])? else {
        return Ok(RepoState::Unknown(format!("git status failed in {what}")));
    };
    if !status.is_empty() {
        return Ok(RepoState::Dirty);
    }
    let Some(base) = base else {
        return Ok(RepoState::Unknown(format!("{what} has no base to compare with (no origin default branch)")));
    };
    match git_ok(repo, &["rev-list", "--count", "HEAD", "--not", base])?.and_then(|n| n.parse::<u64>().ok()) {
        Some(0) => Ok(RepoState::Untouched),
        Some(_) => Ok(RepoState::OwnCommits),
        None => Ok(RepoState::Unknown(format!("could not count {what}'s commits beyond {base}"))),
    }
}

/// What the checkout at `cwd` and each initialized submodule hold. Empty when
/// `cwd` is not a git work tree (there is no branch to have lost).
pub fn inspect(cwd: &Workdir) -> Result<Vec<(String, RepoState)>> {
    if git_ok(cwd, &["rev-parse", "--is-inside-work-tree"])?.as_deref() != Some("true") {
        return Ok(Vec::new());
    }
    let mut out = vec![(".".to_string(), state_of(cwd, default_ref(cwd)?.as_deref(), "the checkout")?)];
    let listed = git_ok(cwd, &["submodule", "foreach", "--recursive", "--quiet", "echo \"$displaypath\""])?;
    let subs: Vec<String> = listed
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    for rel in &subs {
        // The nearest listed ancestor is the repository that records it.
        let parent_rel = subs
            .iter()
            .filter(|p| rel.starts_with(&format!("{p}/")))
            .max_by_key(|p| p.len());
        let (parent, in_parent) = match parent_rel {
            Some(p) => (cwd.join(p), rel[p.len() + 1..].to_string()),
            None => (cwd.clone(), rel.clone()),
        };
        let base = pinned(&parent, &in_parent)?;
        out.push((rel.clone(), state_of(&cwd.join(rel), base.as_deref(), &format!("submodule {rel}"))?));
    }
    Ok(out)
}

/// Whether `repos` prove the work lost: there is at least one, and each is
/// untouched.
pub fn is_lost(repos: &[(String, RepoState)]) -> bool {
    !repos.is_empty() && repos.iter().all(|(_, s)| *s == RepoState::Untouched)
}

/// How many of the node's implementation steps are marked done.
pub fn done_steps(fleet: &FleetStore, node: Uuid) -> Result<usize> {
    fleet.read(|conn| {
        Ok(PlanStepRepo::new(conn)
            .list_for_node(node)?
            .iter()
            .filter(|s| s.phase == PHASE_ACTIVE && matches!(s.status.as_str(), STATUS_IMPLEMENTED | STATUS_VERIFIED))
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
    let Ok(Some(cwd)) = tod_store::fleet::provision::launch_cwd_if_made(fleet, &node.to_string()) else {
        return Ok(None);
    };
    let repos = inspect(&cwd)?;
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
        inspect(&Workdir::host(work)).unwrap()
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
}
