//! Pushing the node's branch: at the end of every session, so a lost
//! sandbox loses no code (design: "Where data lives"). Credentials come from
//! the sandbox's proxy.

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

/// Pushes the checked-out branch to `origin`, setting it as the upstream.
/// Nothing to do (detached, or no `origin`) is not an error.
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
    git(workspace, &["push", "--quiet", "-u", "origin", &format!("HEAD:refs/heads/{branch}")])?;
    Ok(())
}
