//! Checkpoints: a node's uncommitted work, kept on the remote under a hidden
//! ref so a sandbox that expires while the node waits loses nothing.
//!
//! A checkpoint is a commit built from a temporary index (the node's branch,
//! its index, and its working tree are never touched) whose parent is the
//! checkout's `HEAD`, so it also carries any commits not pushed yet. It is
//! pushed, forced, to `refs/tod/checkpoints/<node>/<repo>`: not a branch or a
//! tag, so it shows in no pull request, branch list, or default clone.
//! Anything `.gitignore` covers (the agent scratchpad, build output) is never
//! included.
//!
//! A checkpoint refuses what does not belong on a remote: a file over
//! [`Limits::file_bytes`], a binary (an archive, an image, a build artifact)
//! over [`Limits::binary_bytes`], or more than [`Limits::total_bytes`] in all.
//! It fails naming them, and nothing is pushed.
//!
//! [`restore_repo`] puts one back as uncommitted changes when the node's
//! checkout is made again; [`delete_ref`] and [`remote_refs`] are the cleanup.
//! The node-level entry points (which checkout, locking, submodules) are in
//! [`crate::fleet::provision`].

use crate::fleet::workdir::Workdir;
use anyhow::{Context, Result, bail};
use std::process::Output;

/// Where every checkpoint lives.
pub const REF_ROOT: &str = "refs/tod/checkpoints";

const MIB: u64 = 1024 * 1024;

/// What a checkpoint may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Any one file.
    pub file_bytes: u64,
    /// Any one binary file (git's own test: a NUL byte in it).
    pub binary_bytes: u64,
    /// Every changed file together.
    pub total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { file_bytes: 5 * MIB, binary_bytes: MIB, total_bytes: 50 * MIB }
    }
}

/// What [`save_repo`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Saved {
    /// Nothing uncommitted: there is nothing to keep.
    Clean,
    /// The same work as the checkpoint already pushed.
    Unchanged,
    /// Pushed.
    Pushed { commit: String, files: usize },
}

/// What [`restore_repo`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restored {
    /// No checkpoint on the remote, or it changes nothing here.
    Nothing,
    /// Its changes are in the working tree, uncommitted.
    Applied { files: usize },
}

/// The ref a node's checkpoint of the repository at `rel` (its path from the
/// node's checkout; empty for the checkout itself) is stored at. A ref cannot
/// also be a folder, so the checkout's own is `…/<node>/root` and a
/// submodule's `…/<node>/sub/<path>`.
pub fn ref_name(node_id: &str, rel: &str) -> String {
    let rel = rel.trim_matches('/');
    if rel.is_empty() {
        return format!("{REF_ROOT}/{node_id}/root");
    }
    let safe: String = rel
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/') { c } else { '_' })
        .collect();
    format!("{REF_ROOT}/{node_id}/sub/{safe}")
}

/// `refs/tod/checkpoints/<node>/`: every ref the node has.
pub fn node_prefix(node_id: &str) -> String {
    format!("{REF_ROOT}/{node_id}/")
}

/// The node a checkpoint ref belongs to.
pub fn node_of_ref(reference: &str) -> Option<&str> {
    reference.strip_prefix(REF_ROOT)?.strip_prefix('/')?.split('/').next().filter(|n| !n.is_empty())
}

const IDENTITY: [&str; 4] = ["-c", "user.name=tod", "-c", "user.email=tod@localhost"];

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// `git args…` with the temporary index `index` instead of the repository's.
fn git_with_index(repo: &Workdir, index: &str, args: &[&str]) -> Result<String> {
    let mut all = vec![
        "-c",
        r#"GIT_INDEX_FILE="$0" exec git -c safe.directory='*' "$@""#,
        index,
    ];
    all.extend_from_slice(args);
    let output = repo.output("sh", &all).with_context(|| format!("run git in {repo}"))?;
    if !output.status.success() {
        bail!(
            "git {} failed in {repo}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(text(&output))
}

/// Paths `git status` reports as changed (staged, unstaged, or untracked and
/// not ignored). Submodules are their own repositories and are left out.
fn changed_paths(repo: &Workdir) -> Result<Vec<String>> {
    let out = repo
        .git_output(&["status", "--porcelain=v1", "-z", "-uall", "--ignore-submodules=all"])
        .with_context(|| format!("run git status in {repo}"))?;
    if !out.status.success() {
        bail!("git status failed in {repo}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut paths = Vec::new();
    let mut fields = out.stdout.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(entry) = fields.next() {
        let entry = String::from_utf8_lossy(entry);
        let Some((status, path)) = entry.split_at_checked(3) else { continue };
        paths.push(path.to_string());
        // A rename or copy is followed by the path it came from.
        if status.starts_with('R') || status.starts_with('C') || status[1..].starts_with('R') {
            fields.next();
        }
    }
    Ok(paths)
}

/// The size of each of `paths` that exists as a file (a deleted one has none).
fn file_sizes(repo: &Workdir, paths: &[String]) -> Result<Vec<(String, u64)>> {
    let mut sizes = Vec::new();
    for chunk in paths.chunks(200) {
        let mut args = vec!["-c", "--"];
        args.extend(chunk.iter().map(String::as_str));
        // A missing path or a folder makes `wc` exit non-zero but still
        // reports the rest, so its status is not the answer.
        let out = repo.output("wc", &args).with_context(|| format!("measure files in {repo}"))?;
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let line = line.trim_start();
            let Some((size, name)) = line.split_once(' ') else { continue };
            let Ok(size) = size.parse::<u64>() else { continue };
            let name = name.trim_start();
            if let Some(path) = chunk.iter().find(|p| p.as_str() == name) {
                sizes.push((path.clone(), size));
            }
        }
    }
    Ok(sizes)
}

/// Whether `path` holds text (`grep -I` treats a file with a NUL byte as
/// binary and matches nothing in it).
fn is_binary(repo: &Workdir, path: &str) -> Result<bool> {
    let out = repo
        .output("sh", &["-c", r#"grep -Iq '^' -- "$0""#, path])
        .with_context(|| format!("look at {path} in {repo}"))?;
    match out.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => bail!(
            "could not tell whether {path} is binary in {repo}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / MIB as f64)
}

/// Refuse a change set that is too large or carries binaries, naming what
/// to remove. Called before anything is staged, so nothing big is hashed.
fn gate(repo: &Workdir, paths: &[String], limits: &Limits) -> Result<()> {
    let sizes = file_sizes(repo, paths)?;
    let total: u64 = sizes.iter().map(|(_, s)| s).sum();
    let mut refused: Vec<String> = Vec::new();
    for (path, size) in &sizes {
        if *size > limits.file_bytes {
            refused.push(format!("{path} ({}, over {} for any one file)", mib(*size), mib(limits.file_bytes)));
        } else if *size > limits.binary_bytes && is_binary(repo, path)? {
            refused.push(format!("{path} (binary, {}, over {} for a binary)", mib(*size), mib(limits.binary_bytes)));
        }
    }
    if !refused.is_empty() {
        bail!(
            "not checkpointed, {}: {}. Delete them, move them out of the checkout, or add them to .gitignore",
            repo,
            refused.join("; ")
        );
    }
    if total > limits.total_bytes {
        bail!(
            "not checkpointed, {repo}: the uncommitted files add up to {}, over {}. Commit, delete, or \
             ignore what is not needed",
            mib(total),
            mib(limits.total_bytes)
        );
    }
    Ok(())
}

fn has_origin(repo: &Workdir) -> Result<()> {
    if !repo.git(&["remote"])?.lines().any(|r| r.trim() == "origin") {
        bail!("{repo} has no origin remote to keep a checkpoint on");
    }
    Ok(())
}

/// Checkpoint the uncommitted work in `repo` to `reference` on `origin`.
pub fn save_repo(repo: &Workdir, reference: &str, limits: &Limits) -> Result<Saved> {
    let head = repo
        .git(&["rev-parse", "-q", "--verify", "HEAD"])
        .with_context(|| format!("{repo} has no commit yet to checkpoint on"))?;
    let paths = changed_paths(repo)?;
    if paths.is_empty() {
        return Ok(Saved::Clean);
    }
    gate(repo, &paths, limits)?;
    has_origin(repo)?;

    let git_dir = repo.git(&["rev-parse", "--absolute-git-dir"])?;
    let index = format!("{}/tod-checkpoint.index", git_dir.trim_end_matches('/'));
    let result = build_commit(repo, &index, &head, reference);
    // Never left behind, whatever happened.
    let _ = repo.output("rm", &["-f", &index]);
    let (commit, files) = match result? {
        Built::Nothing => return Ok(Saved::Clean),
        Built::Same => return Ok(Saved::Unchanged),
        Built::Commit { commit, files } => (commit, files),
    };
    repo.git(&["push", "--quiet", "--force", "origin", &format!("{commit}:{reference}")])
        .with_context(|| format!("push the checkpoint {reference}"))?;
    // Only after it is on the remote, so "unchanged" never means unpushed.
    repo.git(&["update-ref", reference, &commit])?;
    Ok(Saved::Pushed { commit, files })
}

enum Built {
    /// The tree is `HEAD`'s.
    Nothing,
    /// The tree is the last pushed checkpoint's.
    Same,
    Commit { commit: String, files: usize },
}

fn build_commit(repo: &Workdir, index: &str, head: &str, reference: &str) -> Result<Built> {
    let _ = repo.output("rm", &["-f", index]);
    git_with_index(repo, index, &["read-tree", head])?;
    git_with_index(repo, index, &["add", "-A"])?;
    // A submodule's recorded commit is not work worth keeping (it moves with
    // the submodule, which has a checkpoint of its own).
    let staged = git_with_index(repo, index, &["diff", "--cached", "--raw", "--no-renames", head])?;
    for line in staged.lines() {
        let Some((meta, path)) = line.split_once('\t') else { continue };
        let mut modes = meta.trim_start_matches(':').split(' ');
        if modes.next() == Some("160000") || modes.next() == Some("160000") {
            git_with_index(repo, index, &["reset", "-q", head, "--", path])?;
        }
    }
    let tree = git_with_index(repo, index, &["write-tree"])?;
    let head_tree = repo.git(&["rev-parse", &format!("{head}^{{tree}}")])?;
    if tree == head_tree {
        return Ok(Built::Nothing);
    }
    let last = repo.git(&["rev-parse", "-q", "--verify", &format!("{reference}^{{tree}}")]);
    if last.is_ok_and(|t| t == tree) {
        return Ok(Built::Same);
    }
    let files = git_with_index(repo, index, &["diff", "--cached", "--name-only", head])?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    let mut args: Vec<&str> = IDENTITY.to_vec();
    args.extend(["commit-tree", &tree, "-p", head, "-m", "tod checkpoint: uncommitted work"]);
    let commit = repo.git(&args)?;
    Ok(Built::Commit { commit, files })
}

/// The commit `reference` points at on `origin`, if it is there.
pub fn remote_ref(repo: &Workdir, reference: &str) -> Result<Option<String>> {
    Ok(remote_refs(repo, reference)?.into_iter().find(|(_, r)| r == reference).map(|(sha, _)| sha))
}

/// Every ref on `origin` matching `pattern` (`refs/tod/checkpoints/<node>/*`
/// or a full name), as `(commit, ref)`.
pub fn remote_refs(repo: &Workdir, pattern: &str) -> Result<Vec<(String, String)>> {
    has_origin(repo)?;
    let out = repo.git(&["ls-remote", "origin", pattern]).with_context(|| format!("list {pattern} on origin"))?;
    Ok(out
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(sha, name)| (sha.trim().to_string(), name.trim().to_string()))
        .collect())
}

/// Delete `reference` on `origin` (and the local copy). `false` when there
/// was none.
pub fn delete_ref(repo: &Workdir, reference: &str) -> Result<bool> {
    let existed = remote_ref(repo, reference)?.is_some();
    if existed {
        repo.git(&["push", "--quiet", "origin", "--delete", reference])
            .with_context(|| format!("delete the checkpoint {reference}"))?;
    }
    repo.git(&["update-ref", "-d", reference])?;
    Ok(existed)
}

/// Put the checkpoint at `reference` back into `repo`, which must have no
/// uncommitted changes (a checkout made just now). The branch is moved to
/// the checkpoint's parent when that is ahead of it (commits that were never
/// pushed), then the checkpoint's changes are applied uncommitted.
///
/// A checkpoint that does not apply (the branch has moved on) leaves the
/// checkout as it was and says so; the checkpoint stays on the remote.
pub fn restore_repo(repo: &Workdir, reference: &str) -> Result<Restored> {
    if remote_ref(repo, reference)?.is_none() {
        return Ok(Restored::Nothing);
    }
    if !repo.git(&["status", "--porcelain", "--ignore-submodules=all"])?.is_empty() {
        bail!("{repo} has uncommitted changes, so the checkpoint {reference} was not applied");
    }
    repo.git(&["fetch", "--quiet", "origin", reference])
        .with_context(|| format!("fetch the checkpoint {reference}"))?;
    let commit = repo.git(&["rev-parse", "FETCH_HEAD"])?;
    let parent = repo.git(&["rev-parse", &format!("{commit}^")])?;
    let head = repo.git(&["rev-parse", "HEAD"])?;
    let is_ancestor = |a: &str, b: &str| -> Result<bool> {
        let out = repo.git_output(&["merge-base", "--is-ancestor", a, b])?;
        Ok(out.status.success())
    };
    if head != parent && !is_ancestor(&parent, &head)? && is_ancestor(&head, &parent)? {
        repo.git(&["merge", "--ff-only", "--quiet", &parent])
            .with_context(|| format!("bring the branch up to {parent}, where the checkpoint was made"))?;
    }
    let mut args: Vec<&str> = IDENTITY.to_vec();
    args.extend(["cherry-pick", "--no-commit", &commit]);
    let applied = repo.git_output(&args)?;
    if !applied.status.success() {
        // Put it back as it was: it was clean, so everything now is the
        // failed attempt's.
        repo.git(&["reset", "--hard", "--quiet", "HEAD"])?;
        repo.git(&["clean", "-fdq"])?;
        bail!(
            "the checkpoint {reference} does not apply to this branch ({}); it is still on origin",
            String::from_utf8_lossy(&applied.stderr).trim().lines().next().unwrap_or("conflict")
        );
    }
    // Uncommitted, as it was, not staged.
    repo.git(&["reset", "--quiet"])?;
    let files = repo.git(&["status", "--porcelain", "--ignore-submodules=all"])?.lines().count();
    Ok(if files == 0 { Restored::Nothing } else { Restored::Applied { files } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    struct Fixture {
        root: PathBuf,
        remote: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("tod-ckpt-{}", uuid::Uuid::new_v4()));
            let remote = root.join("remote.git");
            std::fs::create_dir_all(&remote).unwrap();
            run(&remote, &["init", "--bare", "-q", "-b", "main"]);
            let f = Self { root, remote };
            let seed = f.clone_named("seed");
            std::fs::write(seed.join("a.txt"), "one\n").unwrap();
            std::fs::write(seed.join(".gitignore"), "scratch/\n").unwrap();
            run(&seed, &["add", "."]);
            run(&seed, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
            run(&seed, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
            f
        }

        fn clone_named(&self, name: &str) -> PathBuf {
            let dir = self.root.join(name);
            let out = Command::new("git")
                .args(["clone", "-q"])
                .arg(&self.remote)
                .arg(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            dir
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const REF: &str = "refs/tod/checkpoints/n1/root";

    #[test]
    fn ref_names_never_nest_and_name_their_node() {
        assert_eq!(ref_name("n1", ""), "refs/tod/checkpoints/n1/root");
        assert_eq!(ref_name("n1", "lib/a b"), "refs/tod/checkpoints/n1/sub/lib/a_b");
        assert_eq!(node_of_ref("refs/tod/checkpoints/n1/sub/x"), Some("n1"));
        assert_eq!(node_of_ref("refs/heads/main"), None);
    }

    #[test]
    fn a_clean_checkout_has_nothing_to_save() {
        let f = Fixture::new();
        let work = Workdir::host(f.clone_named("w"));
        assert_eq!(save_repo(&work, REF, &Limits::default()).unwrap(), Saved::Clean);
        assert!(remote_ref(&work, REF).unwrap().is_none());
    }

    #[test]
    fn saves_edits_and_new_files_but_not_ignored_ones_and_leaves_the_checkout_alone() {
        let f = Fixture::new();
        let dir = f.clone_named("w");
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        std::fs::create_dir_all(dir.join("scratch")).unwrap();
        std::fs::write(dir.join("scratch/notes.md"), "temp\n").unwrap();
        let work = Workdir::host(&dir);
        let before = work.git(&["status", "--porcelain"]).unwrap();
        let head = work.git(&["rev-parse", "HEAD"]).unwrap();

        let saved = save_repo(&work, REF, &Limits::default()).unwrap();
        assert!(matches!(saved, Saved::Pushed { files: 2, .. }), "{saved:?}");

        // The checkout, its index, and its branch are exactly as they were.
        assert_eq!(work.git(&["status", "--porcelain"]).unwrap(), before);
        assert_eq!(work.git(&["rev-parse", "HEAD"]).unwrap(), head);
        assert!(!dir.join(".git/tod-checkpoint.index").exists());
        // The ignored scratchpad is not in the checkpoint.
        let on_remote = remote_ref(&work, REF).unwrap().expect("pushed");
        let files = Workdir::host(&f.remote).git(&["ls-tree", "-r", "--name-only", &on_remote]).unwrap();
        assert!(files.contains("new.txt") && !files.contains("scratch"), "{files}");
        // The branch on the remote is untouched.
        assert_eq!(
            Workdir::host(&f.remote).git(&["rev-parse", "refs/heads/main"]).unwrap(),
            head
        );
        // The same work again pushes nothing.
        assert_eq!(save_repo(&work, REF, &Limits::default()).unwrap(), Saved::Unchanged);
    }

    #[test]
    fn restores_into_a_fresh_checkout_as_uncommitted_changes_then_deletes() {
        let f = Fixture::new();
        let dir = f.clone_named("w");
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        // A commit that never reached the remote travels with it.
        run(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "local only"]);
        std::fs::write(dir.join("later.txt"), "later\n").unwrap();
        let work = Workdir::host(&dir);
        save_repo(&work, REF, &Limits::default()).unwrap();

        let fresh = Workdir::host(f.clone_named("fresh"));
        let restored = restore_repo(&fresh, REF).unwrap();
        assert!(matches!(restored, Restored::Applied { files: 2 }), "{restored:?}");
        let root = fresh.host_path().unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "two\n");
        assert_eq!(std::fs::read_to_string(root.join("later.txt")).unwrap(), "later\n");
        assert_eq!(fresh.git(&["status", "--porcelain"]).unwrap(), "?? later.txt\n?? new.txt");

        assert!(delete_ref(&fresh, REF).unwrap());
        assert!(remote_ref(&fresh, REF).unwrap().is_none());
        assert!(!delete_ref(&fresh, REF).unwrap());
        assert_eq!(restore_repo(&fresh, REF).unwrap(), Restored::Nothing);
    }

    #[test]
    fn a_checkpoint_that_conflicts_says_so_and_leaves_the_checkout_clean() {
        let f = Fixture::new();
        let dir = f.clone_named("w");
        std::fs::write(dir.join("a.txt"), "mine\n").unwrap();
        let work = Workdir::host(&dir);
        save_repo(&work, REF, &Limits::default()).unwrap();

        // Someone else changed the same line on the branch.
        let other = f.clone_named("other");
        std::fs::write(other.join("a.txt"), "theirs\n").unwrap();
        run(&other, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "theirs"]);
        run(&other, &["push", "-q", "origin", "HEAD:refs/heads/main"]);

        let fresh = Workdir::host(f.clone_named("fresh"));
        let err = restore_repo(&fresh, REF).unwrap_err().to_string();
        assert!(err.contains("does not apply") && err.contains("still on origin"), "{err}");
        assert_eq!(fresh.git(&["status", "--porcelain"]).unwrap(), "");
        assert!(remote_ref(&fresh, REF).unwrap().is_some());
    }

    #[test]
    fn a_large_file_or_a_binary_is_refused_by_name_and_nothing_is_pushed() {
        let f = Fixture::new();
        let dir = f.clone_named("w");
        std::fs::write(dir.join("big.log"), "x".repeat(300)).unwrap();
        let mut zip = vec![0u8; 200];
        zip[0] = b'P';
        std::fs::write(dir.join("build.zip"), zip).unwrap();
        std::fs::write(dir.join("ok.txt"), "fine\n").unwrap();
        let work = Workdir::host(&dir);
        let limits = Limits { file_bytes: 250, binary_bytes: 100, total_bytes: 10_000 };

        let err = save_repo(&work, REF, &limits).unwrap_err().to_string();
        assert!(err.contains("big.log") && err.contains("build.zip") && !err.contains("ok.txt"), "{err}");
        assert!(remote_ref(&work, REF).unwrap().is_none());

        let tight = Limits { file_bytes: 250, binary_bytes: 1_000, total_bytes: 100 };
        std::fs::remove_file(dir.join("big.log")).unwrap();
        let err = save_repo(&work, REF, &tight).unwrap_err().to_string();
        assert!(err.contains("add up to"), "{err}");
    }
}
