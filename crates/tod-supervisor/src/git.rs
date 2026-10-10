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

/// Pushes the checked-out branch to `origin`, setting it as the upstream.
/// Nothing to do (detached, or no `origin`) is not an error; a default
/// branch is.
pub fn push_branch(workspace: &Path) -> Result<()> {
    let branch = git(workspace, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "HEAD" {
        tracing::info!("detached HEAD: nothing to push");
        return Ok(());
    }
    if git(workspace, &["remote"])?.lines().all(|r| r != "origin") {
        tracing::info!("no origin: nothing to push");
        return Ok(());
    }
    let default = git(workspace, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .ok()
        .map(|r| r.strip_prefix("origin/").unwrap_or(&r).to_string());
    if is_protected(&branch, default.as_deref()) {
        bail!("refusing to push {branch}: it is a default branch, not the node's own");
    }
    git(workspace, &["push", "--quiet", "-u", "origin", &format!("HEAD:refs/heads/{branch}")])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_protected;

    #[test]
    fn default_and_conventional_branches_are_protected() {
        assert!(is_protected("main", None));
        assert!(is_protected("release", Some("release")));
        assert!(!is_protected("task/fix-login", Some("main")));
    }
}
