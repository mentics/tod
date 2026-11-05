//! Checkpointing the node's uncommitted work from inside its own sandbox
//! (`tod_store::fleet::checkpoint`: a commit on a temporary index, force-pushed
//! to `refs/tod/checkpoints/<node>/<repo>`), so the work survives the sandbox
//! however the sandbox ends: expired, pruned while idle, or cut off from the
//! app, the daemon, and this machine altogether. The supervisor is the one
//! process sure to be there while the node works; the app still checkpoints
//! too when a node turns to the user (`tod_core::checkpoint`), and the two
//! never share a temporary index.
//!
//! When: at every step boundary, at turn boundaries and during a turn once
//! [`EVERY`] has passed since the last, and once more before the run exits
//! (after the branch is pushed). A checkpoint during a turn runs on a thread
//! of its own, since [`tod_core::autopilot::StepHook::watch`] must not block;
//! only one runs at a time. A failure is a warning, which the problem layer
//! forwards to the user ([`crate::problems`]); it never stops the run.

use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tod_store::fleet::checkpoint::{Limits, Saved};
use tod_store::fleet::node_checkpoint::checkpoint_dir;
use tod_store::fleet::workdir::Workdir;
use uuid::Uuid;

/// How often a node that keeps working is checkpointed (the app's period).
pub const EVERY: Duration = Duration::from_millis(tod_core::checkpoint::PERIODIC_MS as u64);

pub struct Checkpointer {
    workspace: PathBuf,
    node: String,
    every: Duration,
    last: Option<Instant>,
    running: Option<JoinHandle<()>>,
}

impl Checkpointer {
    pub fn new(workspace: &Path, node: Uuid, every: Duration) -> Self {
        Self { workspace: workspace.to_path_buf(), node: node.to_string(), every, last: None, running: None }
    }

    /// Starts a checkpoint in the background when one is due (`force`, or
    /// [`Self::every`] since the last started) and none is running. Never
    /// blocks.
    pub fn tick(&mut self, force: bool) {
        if self.running.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        self.reap();
        if !force && self.last.is_some_and(|t| t.elapsed() < self.every) {
            return;
        }
        self.last = Some(Instant::now());
        let (workspace, node) = (self.workspace.clone(), self.node.clone());
        self.running = Some(std::thread::spawn(move || save(&workspace, &node)));
    }

    /// Checkpoints now, after any in flight: for the end of the run.
    pub fn now(&mut self) {
        self.reap();
        self.last = Some(Instant::now());
        save(&self.workspace, &self.node);
    }

    /// Waits for the one in flight, if any.
    fn reap(&mut self) {
        if let Some(handle) = self.running.take()
            && handle.join().is_err()
        {
            tracing::warn!("checkpointing the node's uncommitted work: the checkpoint thread panicked");
        }
    }
}

fn save(workspace: &Path, node: &str) {
    match checkpoint_dir(&Workdir::Host(workspace.to_path_buf()), node, &Limits::default()) {
        Ok(saved) => {
            for (repo, saved) in saved {
                if let Saved::Pushed { files, .. } = saved {
                    let repo = if repo.is_empty() { "the checkout".to_string() } else { repo };
                    tracing::info!("checkpointed {files} uncommitted file(s) of {repo}");
                }
            }
        }
        Err(err) => tracing::warn!("checkpointing the node's uncommitted work: {err:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn ls_remote(remote: &Path) -> String {
        let out = Command::new("git").arg("-C").arg(remote).args(["for-each-ref", "refs/tod"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// A checkout with an origin, as a node's sandbox has.
    fn checkout() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let remote = root.path().join("remote.git");
        let work = root.path().join("work");
        git(root.path(), &["init", "--quiet", "--bare", remote.to_str().unwrap()]);
        git(root.path(), &["init", "--quiet", "-b", "task/x", work.to_str().unwrap()]);
        std::fs::write(work.join("a.txt"), "one\n").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "-m", "one"]);
        git(&work, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&work, &["push", "--quiet", "origin", "task/x"]);
        (root, remote, work)
    }

    #[test]
    fn uncommitted_work_reaches_the_remote_from_the_tick_and_at_the_end() {
        let (_root, remote, work) = checkout();
        let node = Uuid::new_v4();
        let mut cp = Checkpointer::new(&work, node, Duration::from_secs(3600));
        std::fs::write(work.join("a.txt"), "two\n").unwrap();
        cp.tick(false);
        cp.reap();
        let refs = ls_remote(&remote);
        assert!(refs.contains(&format!("refs/tod/checkpoints/{node}")), "{refs}");

        // Not due again within the period, unless forced or at the end.
        let before = ls_remote(&remote);
        std::fs::write(work.join("b.txt"), "new\n").unwrap();
        cp.tick(false);
        assert!(cp.running.is_none());
        cp.now();
        assert_ne!(ls_remote(&remote), before, "the end of the run checkpoints whatever the period");
    }
}
