//! When the daemon stops on its own, and which of its build copies are kept.
//!
//! The daemon is started by `tod` and never by the operating system, so
//! without this nothing ever ends it: scratch data roots (a test sandbox, a
//! session's worktree) each left one running for good (`doc/agentd.md`,
//! "Lifetime").

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long with no client before the daemon may go (6 hours); the
/// environment variable [`IDLE_ENV`] overrides it, in seconds.
pub const DEFAULT_IDLE: Duration = Duration::from_secs(6 * 60 * 60);
pub const IDLE_ENV: &str = "TOD_AGENTD_IDLE_SECS";
/// How often the daemon checks.
pub const CHECK_EVERY: Duration = Duration::from_secs(60);
/// Build copies kept besides the running one.
pub const KEEP_BUILDS: usize = 3;

pub fn idle_limit() -> Duration {
    std::env::var(IDLE_ENV).ok().and_then(|v| v.parse().ok()).map(Duration::from_secs).unwrap_or(DEFAULT_IDLE)
}

/// Why the daemon should exit now, if it should.
///
/// - the data root directory is gone: always;
/// - no client (app or `tod-cli`) since `last_client` for `idle`, and no run
///   in progress, and the daemon is not holding the database broker's poll
///   for a root that declares databases (that is a feature: it answers while
///   the app is closed).
pub fn should_exit(
    now: Instant,
    last_client: Instant,
    runs: usize,
    polls_broker: bool,
    root_exists: bool,
    idle: Duration,
) -> Option<String> {
    if !root_exists {
        return Some("its data root no longer exists".into());
    }
    if runs > 0 || polls_broker {
        return None;
    }
    let quiet = now.saturating_duration_since(last_client);
    (quiet >= idle).then(|| format!("no client for {} s, no runs, and no database broker to serve", quiet.as_secs()))
}

/// Removes the build copies (`tod-agentd-<stamp>`) in `dir` other than
/// `running` and the `keep` newest. Returns what was removed; a copy that
/// cannot be removed is logged and left.
pub fn prune_builds(dir: &Path, running: Option<&Path>, keep: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut copies: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("tod-agentd-"))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    copies.sort_by(|a, b| b.cmp(a));
    let same = |p: &Path| running.is_some_and(|r| r == p || r.canonicalize().ok() == p.canonicalize().ok());
    let mut removed = Vec::new();
    for (_, path) in copies.into_iter().skip(keep) {
        if same(&path) {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!("tod-agentd: removed the old build copy {}", path.display());
                removed.push(path);
            }
            Err(err) => tracing::warn!("tod-agentd: could not remove the old build copy {}: {err}", path.display()),
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_secs(100);

    #[test]
    fn exits_when_the_root_is_gone_whatever_else_holds() {
        let now = Instant::now();
        assert!(should_exit(now, now, 3, true, false, IDLE).is_some());
    }

    #[test]
    fn stays_while_a_client_is_recent_a_run_is_going_or_the_broker_is_polled() {
        let last = Instant::now();
        let later = last + Duration::from_secs(1000);
        assert!(should_exit(last + Duration::from_secs(50), last, 0, false, true, IDLE).is_none());
        assert!(should_exit(later, last, 1, false, true, IDLE).is_none());
        assert!(should_exit(later, last, 0, true, true, IDLE).is_none());
    }

    #[test]
    fn exits_when_idle_with_nothing_to_do() {
        let last = Instant::now();
        let reason = should_exit(last + Duration::from_secs(100), last, 0, false, true, IDLE).expect("idle long enough");
        assert!(reason.contains("no client"), "{reason}");
    }

    #[test]
    fn prune_keeps_the_running_copy_and_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::time::SystemTime::now() - Duration::from_secs(10_000);
        let mut paths = Vec::new();
        for i in 0..6u64 {
            let p = dir.path().join(format!("tod-agentd-s{i}"));
            let f = std::fs::File::create(&p).unwrap();
            f.set_modified(base + Duration::from_secs(i * 100)).unwrap();
            paths.push(p);
        }
        std::fs::write(dir.path().join("agentd.lock"), b"").unwrap();
        // The oldest is the one running.
        let removed = prune_builds(dir.path(), Some(&paths[0]), 3);
        assert_eq!(removed.len(), 2, "{removed:?}");
        assert!(paths[0].exists(), "the running copy stays");
        for p in &paths[3..] {
            assert!(p.exists());
        }
        assert!(!paths[1].exists() && !paths[2].exists());
        assert!(dir.path().join("agentd.lock").exists());
    }
}
