//! What a freshly made checkout's branch started from ([`crate::branch_starts`]).
//!
//! [`capture`] runs once a node's worktree or sandbox is made. For the
//! checkout and each initialized submodule it reports the branch tip, but only
//! where that tip is provably a birth commit: the branch is new (no
//! `origin/<branch>`, so nothing was pushed to it before) and sits at the
//! repository's default branch tip or at the commit the parent pins. Anything
//! else (a branch with work already, a branch that was pushed) yields no
//! record, and the lost-work rule then proves nothing for it.

use crate::fleet::worktree::new_branch_base;
use crate::fleet::workdir::Workdir;
use anyhow::{Context, Result};
use std::collections::BTreeMap;

/// `rev-parse --verify -q spec`: `None` when git says there is no such thing
/// (exit status 1), an error for any other failure.
fn rev(repo: &Workdir, spec: &str) -> Result<Option<String>> {
    let out = repo
        .git_output(&["rev-parse", "--verify", "-q", spec])
        .with_context(|| format!("run git rev-parse {spec} in {repo}"))?;
    if out.status.success() {
        return Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_string()));
    }
    if out.status.code() == Some(1) {
        return Ok(None);
    }
    anyhow::bail!("git rev-parse {spec} in {repo} failed: {}", String::from_utf8_lossy(&out.stderr).trim())
}

/// The birth commit of `branch` in `repo`, when it is one. `pinned` is the
/// commit the parent repository records for it (a submodule).
fn birth(repo: &Workdir, branch: &str, pinned: Option<&str>) -> Result<Option<String>> {
    if rev(repo, &format!("refs/remotes/origin/{branch}^{{commit}}"))?.is_some() {
        return Ok(None);
    }
    let Some(tip) = rev(repo, &format!("refs/heads/{branch}^{{commit}}"))? else {
        return Ok(None);
    };
    let mut candidates: Vec<String> = pinned.map(str::to_string).into_iter().collect();
    if let Some(base) = new_branch_base(repo)
        && let Some(base_tip) = rev(repo, &format!("{base}^{{commit}}"))?
    {
        candidates.push(base_tip);
    }
    Ok(candidates.contains(&tip).then_some(tip))
}

/// The commit a parent's HEAD records for the submodule at `rel`.
fn gitlink(parent: &Workdir, rel: &str) -> Result<Option<String>> {
    let listing = parent.git(&["ls-tree", "HEAD", "--", rel])?;
    Ok(listing.split_whitespace().nth(2).map(str::to_string))
}

/// The birth commits of the node's `branch` in the checkout at `dir` (`.`) and
/// in each initialized submodule (recursively, by path), for those where the
/// branch is provably new.
pub fn capture(dir: &Workdir, branch: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    if let Some(sha) = birth(dir, branch, None)? {
        out.insert(".".to_string(), sha);
    }
    if dir.git(&["ls-files", "--", ".gitmodules"])?.is_empty() {
        return Ok(out);
    }
    let subs: Vec<String> = dir
        .git(&["submodule", "foreach", "--recursive", "--quiet", "echo \"$displaypath\""])?
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    for rel in &subs {
        // The nearest listed ancestor is the repository that records it.
        let parent_rel = subs.iter().filter(|p| rel.starts_with(&format!("{p}/"))).max_by_key(|p| p.len());
        let (parent, in_parent) = match parent_rel {
            Some(p) => (dir.join(p), rel[p.len() + 1..].to_string()),
            None => (dir.clone(), rel.clone()),
        };
        let pinned = gitlink(&parent, &in_parent)?;
        if let Some(sha) = birth(&dir.join(rel), branch, pinned.as_deref())? {
            out.insert(rel.clone(), sha);
        }
    }
    Ok(out)
}
