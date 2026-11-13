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

/// The commit the parent of a submodule pinned before the node's work: the
/// gitlink at `rel` (its path in that parent) at the merge base of the
/// parent's HEAD and its `origin` default branch. Commits reachable from it
/// are not the node's. `None` when it cannot be told (no remote, no entry).
fn pinned_commit(parent: &Path, rel: &str) -> Option<String> {
    let base = ["refs/remotes/origin/HEAD", "refs/remotes/origin/main", "refs/remotes/origin/master"]
        .iter()
        .find_map(|r| git(parent, &["merge-base", "HEAD", r]).ok())?;
    git(parent, &["rev-parse", "--verify", "-q", &format!("{base}:{rel}")]).ok().filter(|s| !s.is_empty())
}

/// Whether `repo`'s HEAD has commits of the node's own that no remote has:
/// not on any remote-tracking ref and not in the lineage `pinned` (what the
/// parent recorded before the node's work) already holds.
///
/// The remote-tracking refs are refreshed first: a stale `origin/main` makes an
/// upstream commit (a submodule checked out at what the base pins) look like
/// the node's own. A failed fetch is an error naming the repository, never a
/// guess from the stale refs. With no `origin` there is nothing to refresh.
fn has_unpushed(repo: &Path, pinned: Option<&str>) -> Result<bool> {
    if git(repo, &["remote", "get-url", "origin"]).is_ok() {
        git(repo, &["fetch", "--quiet", "--no-recurse-submodules", "origin"])
            .with_context(|| format!("refresh the remote refs of {} before checking for unpushed commits", repo.display()))?;
    }
    let mut args = vec!["rev-list", "-n1", "HEAD", "--not", "--remotes"];
    if let Some(p) = pinned {
        ensure_commit(repo, p)?;
        args.push(p);
    }
    Ok(!git(repo, &args)?.is_empty())
}

/// Makes sure `repo` has commit `sha`. A merge of the base brings in the
/// parent's new pointer for this submodule without the commit (tod fetches
/// without recursing), and `rev-list` then dies with "bad object". Fetch it
/// from the submodule's own `origin`; a commit that is not there either is an
/// error saying so.
fn ensure_commit(repo: &Path, sha: &str) -> Result<()> {
    let commit = format!("{sha}^{{commit}}");
    if git(repo, &["cat-file", "-e", &commit]).is_ok() {
        return Ok(());
    }
    // By id first (the server may allow it), else everything.
    if git(repo, &["fetch", "--quiet", "--no-recurse-submodules", "origin", sha]).is_err() {
        git(repo, &["fetch", "--quiet", "--no-recurse-submodules", "origin"])
            .with_context(|| format!("fetch the commit {sha} its parent pins"))?;
    }
    git(repo, &["cat-file", "-e", &commit])
        .with_context(|| format!("the parent pins commit {sha}, which is not in its checkout or on its origin"))?;
    Ok(())
}

/// The checkout's default branch, from `origin/HEAD`, when known.
fn default_branch(repo: &Path) -> Option<String> {
    git(repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .ok()
        .map(|r| r.strip_prefix("origin/").unwrap_or(&r).to_string())
}

/// Which branch `push_repo` may push.
#[derive(Clone, Copy)]
enum Expected<'a> {
    /// Whatever the repository is on (the workspace itself, which defines it).
    Any,
    /// Only this one, the node's.
    Branch(&'a str),
    /// The node's branch is not known: no named branch is pushed.
    Unknown,
}

/// Pushes `repo`'s checked-out branch to `origin`, setting it as the
/// upstream, only when it is `expected` (the node's own branch). `Ok(false)`
/// when there was nothing to do (detached, or no `origin`); a default branch
/// or another branch is an error.
fn push_repo(repo: &Path, expected: Expected) -> Result<bool> {
    let branch = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "HEAD" {
        tracing::info!("detached HEAD: nothing to push");
        return Ok(false);
    }
    // `git remote` also lists a remote that only has settings (a global
    // `remote.origin.prune`), so ask for its URL.
    if git(repo, &["remote", "get-url", "origin"]).is_err() {
        tracing::info!("no origin: nothing to push");
        return Ok(false);
    }
    if is_protected(&branch, default_branch(repo).as_deref()) {
        bail!("refusing to push {branch}: it is a default branch, not the node's own");
    }
    match expected {
        Expected::Any => {}
        Expected::Branch(expected) if branch == expected => {}
        Expected::Branch(expected) => {
            bail!("it is on {branch}, not the node's branch {expected}, and only the node's own branch is pushed")
        }
        Expected::Unknown => {
            bail!("it is on {branch}, but the node's own branch is not known (the checkout is detached), so nothing was pushed")
        }
    }
    git(repo, &["push", "--quiet", "-u", "origin", &format!("HEAD:refs/heads/{branch}")])?;
    Ok(true)
}

/// One submodule: where it is, the repository that records it, and its path
/// in that repository.
struct Sub {
    dir: std::path::PathBuf,
    parent: std::path::PathBuf,
    in_parent: String,
}

/// Every submodule of `workspace`, recursively, a nested one before the
/// submodule that holds it.
fn submodules_nested_first(workspace: &Path) -> Result<Vec<Sub>> {
    let listed = git(workspace, &["submodule", "foreach", "--recursive", "--quiet", "echo \"$displaypath\""])?;
    let mut subs: Vec<Sub> = listed
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let rel = Path::new(l.trim());
            // The nearest ancestor that is itself a checkout is the parent.
            let parent = rel
                .ancestors()
                .skip(1)
                .find(|p| !p.as_os_str().is_empty() && workspace.join(p).join(".git").exists());
            let (parent_dir, in_parent) = match parent {
                Some(p) => (workspace.join(p), rel.strip_prefix(p).unwrap_or(rel).display().to_string()),
                None => (workspace.to_path_buf(), rel.display().to_string()),
            };
            Sub { dir: workspace.join(rel), parent: parent_dir, in_parent }
        })
        .collect();
    subs.reverse();
    Ok(subs)
}

/// Pushes the node's own branch (the one `workspace` is on) in `workspace`
/// and in each submodule that holds commits of the node's own no remote has
/// (the agent's work in a submodule is lost with the sandbox otherwise: the
/// parent only records its commit). Submodules go first, a nested one before
/// the submodule holding it, so the commit a parent records is on the remote
/// when the parent is. Only the node's branch is pushed: a submodule with such
/// commits on any other branch, on a detached HEAD, or with no origin is a
/// failure, never skipped. Every repository is tried, including when listing
/// the submodules failed; the failures come back together, each naming its
/// repository.
pub fn push_branch(workspace: &Path) -> Result<()> {
    let mut failures = Vec::new();
    let node_branch = git(workspace, &["rev-parse", "--abbrev-ref", "HEAD"]).ok().filter(|b| b != "HEAD");
    match submodules_nested_first(workspace) {
        Ok(subs) => {
            for sub in subs {
                let name = sub.dir.strip_prefix(workspace).unwrap_or(&sub.dir).display().to_string();
                let result = (|| -> Result<()> {
                    if !sub.dir.join(".git").exists() {
                        return Ok(());
                    }
                    let pinned = pinned_commit(&sub.parent, &sub.in_parent)
                        // Nothing to compare with (the parent has no origin):
                        // what the parent records at its HEAD is the base.
                        .or_else(|| git(&sub.parent, &["remote", "get-url", "origin"]).err().and_then(|_| git(&sub.parent, &["rev-parse", "--verify", "-q", &format!("HEAD:{}", sub.in_parent)]).ok()))
                        .filter(|s| !s.is_empty());
                    if !has_unpushed(&sub.dir, pinned.as_deref())? {
                        return Ok(());
                    }
                    if !push_repo(&sub.dir, node_branch.as_deref().map_or(Expected::Unknown, Expected::Branch))? {
                        bail!("it has commits of the node's own no remote has, but nothing was pushed (detached HEAD or no origin)");
                    }
                    Ok(())
                })();
                if let Err(err) = result {
                    failures.push(format!("submodule {name}: {err:#}"));
                }
            }
        }
        Err(err) => failures.push(format!("listing the submodules: {err:#}")),
    }
    if let Err(err) = push_repo(workspace, Expected::Any) {
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

    /// The failure behind "fatal: bad object": the node merged a base that
    /// bumped the submodule, without fetching the submodule, so the commit the
    /// parent pins is not in the checkout.
    #[test]
    fn a_pinned_commit_the_base_merge_brought_in_unfetched_is_fetched() {
        let (work, _ws_remote, sub_remote) = fixture();
        let root = work.parent().unwrap().to_path_buf();
        // Someone else bumps kb on main.
        let subseed = root.join("subseed");
        std::fs::write(subseed.join("b"), "b").unwrap();
        run(&subseed, &["add", "."]);
        run(&subseed, &["commit", "-qm", "bump"]);
        run(&subseed, &["push", "-q", "origin", "HEAD:main"]);
        let seed = root.join("seed");
        run(&seed.join("kb"), &["fetch", "-q", "origin"]);
        run(&seed.join("kb"), &["checkout", "-q", "origin/main"]);
        run(&seed, &["commit", "-qam", "bump kb"]);
        run(&seed, &["push", "-q", "origin", "HEAD:main"]);
        // The node merges it, fetching without submodules.
        run(&work, &["fetch", "-q", "--no-recurse-submodules", "origin"]);
        run(&work, &["merge", "-q", "--no-edit", "origin/main"]);
        let bumped = run(&work, &["rev-parse", "HEAD:kb"]);
        let have = Command::new("git").arg("-C").arg(work.join("kb")).args(["cat-file", "-e", &bumped]).output().unwrap();
        assert!(!have.status.success(), "precondition: the pinned commit is not fetched");
        commit_in_kb(&work);
        push_branch(&work).unwrap();
        assert!(run(&sub_remote, &["branch", "--list", "task/x"]).contains("task/x"));
    }

    /// The live failure: a submodule sitting on an upstream commit (what the
    /// base pins) that its stale remote-tracking ref has not seen is not the
    /// node's work, and nothing is pushed or failed.
    #[test]
    fn an_upstream_commit_behind_a_stale_remote_ref_is_not_the_nodes_own() {
        let (work, _ws_remote, sub_remote) = fixture();
        let root = work.parent().unwrap().to_path_buf();
        let subseed = root.join("subseed");
        std::fs::write(subseed.join("b"), "b").unwrap();
        run(&subseed, &["add", "."]);
        run(&subseed, &["commit", "-qm", "upstream"]);
        run(&subseed, &["push", "-q", "origin", "HEAD:main"]);
        let upstream = run(&subseed, &["rev-parse", "HEAD"]);
        let kb = work.join("kb");
        // Fetch the commit without updating origin/main, then sit on it detached.
        run(&kb, &["fetch", "-q", "origin", &upstream]);
        run(&kb, &["checkout", "-q", "--detach", &upstream]);
        assert_ne!(run(&kb, &["rev-parse", "origin/main"]), upstream, "precondition: origin/main is stale");
        push_branch(&work).unwrap();
        assert!(run(&sub_remote, &["branch", "--list", "task/x"]).is_empty());
    }

    #[test]
    fn an_own_commit_on_a_detached_head_still_fails_after_the_refresh() {
        let (work, _ws_remote, sub_remote) = fixture();
        run(&work.join("kb"), &["checkout", "-q", "--detach"]);
        commit_in_kb(&work);
        let err = format!("{:#}", push_branch(&work).unwrap_err());
        assert!(err.contains("submodule kb") && err.contains("not known") || err.contains("nothing was pushed"), "{err}");
        assert!(run(&sub_remote, &["branch", "--list", "task/x"]).is_empty());
    }

    #[test]
    fn an_unreachable_origin_is_an_error_naming_the_repository() {
        let (work, _ws_remote, _sub_remote) = fixture();
        run(&work.join("kb"), &["remote", "set-url", "origin", "/nonexistent/kb.git"]);
        let err = format!("{:#}", push_branch(&work).unwrap_err());
        assert!(err.contains("submodule kb") && err.contains("refresh the remote refs"), "{err}");
    }

    #[test]
    fn a_submodule_on_another_branch_with_commits_is_an_error_and_is_not_pushed() {
        let (work, _ws_remote, sub_remote) = fixture();
        run(&work.join("kb"), &["checkout", "-q", "-b", "shared/other"]);
        commit_in_kb(&work);
        let err = format!("{:#}", push_branch(&work).unwrap_err());
        assert!(err.contains("submodule kb") && err.contains("shared/other"), "{err}");
        assert!(run(&sub_remote, &["branch", "--list", "shared/other"]).is_empty());
    }

    #[test]
    fn a_submodule_without_origin_and_without_work_is_nothing_to_push() {
        let (work, _ws_remote, _sub_remote) = fixture();
        run(&work.join("kb"), &["remote", "remove", "origin"]);
        push_branch(&work).unwrap();
        // With work of its own and nowhere to push it, it is an error.
        commit_in_kb(&work);
        let err = format!("{:#}", push_branch(&work).unwrap_err());
        assert!(err.contains("submodule kb") && err.contains("no origin"), "{err}");
    }

    #[test]
    fn a_detached_parent_never_pushes_a_named_submodule_branch() {
        let (work, _ws_remote, sub_remote) = fixture();
        run(&work.join("kb"), &["checkout", "-q", "-b", "shared/other"]);
        commit_in_kb(&work);
        run(&work, &["checkout", "-q", "--detach"]);
        let err = format!("{:#}", push_branch(&work).unwrap_err());
        assert!(err.contains("submodule kb") && err.contains("not known"), "{err}");
        assert!(run(&sub_remote, &["branch", "--list", "shared/other"]).is_empty());
    }

    #[test]
    fn no_origin_anywhere_and_no_work_is_fine_and_own_work_is_an_error() {
        let (work, _ws_remote, _sub_remote) = fixture();
        run(&work, &["remote", "remove", "origin"]);
        run(&work.join("kb"), &["remote", "remove", "origin"]);
        push_branch(&work).unwrap();
        commit_in_kb(&work);
        let err = format!("{:#}", push_branch(&work).unwrap_err());
        assert!(err.contains("submodule kb") && err.contains("no origin"), "{err}");
    }

    #[test]
    fn a_parent_with_no_remote_is_not_an_error() {
        let (work, _ws_remote, _sub_remote) = fixture();
        run(&work, &["remote", "remove", "origin"]);
        push_branch(&work).unwrap();
    }

    #[test]
    fn a_listing_failure_is_reported_and_the_parent_is_still_pushed() {
        let (work, ws_remote, _sub_remote) = fixture();
        std::fs::write(work.join(".gitmodules"), "[submodule \"kb\"\n").unwrap();
        let err = format!("{:#}", push_branch(&work).unwrap_err());
        assert!(err.contains("listing the submodules"), "{err}");
        assert!(run(&ws_remote, &["branch", "--list", "task/x"]).contains("task/x"));
    }

    #[test]
    fn a_nested_submodules_work_is_pushed_before_its_parent_submodule() {
        let (work, _ws_remote, sub_remote) = fixture();
        // A second repository nested in kb.
        let root = work.parent().unwrap().to_path_buf();
        let inner_remote = root.join("inner.git");
        run(&root, &["init", "-q", "--bare", "-b", "main", inner_remote.to_str().unwrap()]);
        let seed = root.join("innerseed");
        run(&root, &["clone", "-q", inner_remote.to_str().unwrap(), seed.to_str().unwrap()]);
        std::fs::write(seed.join("i"), "i").unwrap();
        run(&seed, &["add", "."]);
        run(&seed, &["commit", "-qm", "inner"]);
        run(&seed, &["push", "-q", "origin", "HEAD:main"]);
        let kbseed = root.join("kbseed");
        run(&root, &["clone", "-q", sub_remote.to_str().unwrap(), kbseed.to_str().unwrap()]);
        run(&kbseed, &["submodule", "add", "-q", inner_remote.to_str().unwrap(), "inner"]);
        run(&kbseed, &["commit", "-qm", "nest"]);
        run(&kbseed, &["push", "-q", "origin", "HEAD:main"]);
        let kb = work.join("kb");
        run(&kb, &["pull", "-q", "origin", "main"]);
        run(&kb, &["submodule", "update", "--init", "--recursive"]);
        run(&kb.join("inner"), &["checkout", "-q", "-b", "task/x"]);
        std::fs::write(kb.join("inner/work"), "w").unwrap();
        run(&kb.join("inner"), &["add", "."]);
        run(&kb.join("inner"), &["commit", "-qm", "nested work"]);
        push_branch(&work).unwrap();
        assert_eq!(run(&inner_remote, &["rev-parse", "task/x"]), run(&kb.join("inner"), &["rev-parse", "HEAD"]));
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
