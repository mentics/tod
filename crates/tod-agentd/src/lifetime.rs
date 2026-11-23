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
/// How long with no client and no run a daemon that a newer build has
/// outdated may go on, even for a root that declares databases (the broker's
/// poll is no reason to keep an old build alive: nothing restarts it, and it
/// polls the orchestrator on old rules).
pub const STALE_GRACE: Duration = Duration::from_secs(30 * 60);
/// Build copies kept besides the running one.
pub const KEEP_BUILDS: usize = 3;

pub fn idle_limit() -> Duration {
    std::env::var(IDLE_ENV).ok().and_then(|v| v.parse().ok()).map(Duration::from_secs).unwrap_or(DEFAULT_IDLE)
}

/// Why the daemon should exit now, if it should.
///
/// - the data root directory is gone: always;
/// - no client (app or `tod-cli`) since `last_client` for `idle`, and no run
///   in progress or scheduled locally, and the daemon is not serving the
///   database broker for a root that declares databases;
/// - `stale` (a newer build exists, [`stale_reason`]) and no client for
///   [`STALE_GRACE`] and no active or scheduled run: whatever the broker does.
pub fn should_exit(
    now: Instant,
    last_client: Instant,
    has_work: bool,
    polls_broker: bool,
    root_exists: bool,
    stale: Option<&str>,
    idle: Duration,
) -> Option<String> {
    if !root_exists {
        return Some("its data root no longer exists".into());
    }
    if has_work {
        return None;
    }
    let quiet = now.saturating_duration_since(last_client);
    if let Some(why) = stale
        && quiet >= STALE_GRACE
    {
        return Some(format!("{why}, and no client for {} s and no active or scheduled runs", quiet.as_secs()));
    }
    if polls_broker {
        return None;
    }
    (quiet >= idle).then(|| format!("no client for {} s, no active or scheduled runs, and no database broker to serve", quiet.as_secs()))
}

/// Why `running` (this daemon's own executable) is outdated, if it is: a
/// newer build copy sits beside it in `dir` (a later start for this root made
/// it), or the program it was started from (`program_dir/tod-agentd`, which a
/// rebuild or an update replaces) is newer than it.
pub fn stale_reason(dir: &Path, running: &Path, program_dir: Option<&Path>) -> Option<String> {
    let mine = std::fs::metadata(running).ok()?.modified().ok()?;
    let same = |p: &Path| p == running || p.canonicalize().ok() == running.canonicalize().ok();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.filter_map(|e| e.ok()) {
            let path = e.path();
            if !e.file_name().to_string_lossy().starts_with("tod-agentd-") || same(&path) {
                continue;
            }
            if e.metadata().ok().and_then(|m| m.modified().ok()).is_some_and(|t| t > mine) {
                return Some(format!("a newer build ({}) is installed for this data root", e.file_name().to_string_lossy()));
            }
        }
    }
    let source = program_dir?.join(if cfg!(windows) { "tod-agentd.exe" } else { "tod-agentd" });
    let newer = std::fs::metadata(&source).ok()?.modified().ok()? > mine + Duration::from_secs(1);
    newer.then(|| format!("{} is a newer build than this one", source.display()))
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
        assert!(should_exit(now, now, true, true, false, None, IDLE).is_some());
    }

    #[test]
    fn stays_while_a_client_is_recent_a_run_is_going_or_the_broker_is_polled() {
        let last = Instant::now();
        let later = last + Duration::from_secs(1000);
        assert!(should_exit(last + Duration::from_secs(50), last, false, false, true, None, IDLE).is_none());
        assert!(should_exit(later, last, true, false, true, None, IDLE).is_none());
        assert!(should_exit(later, last, false, true, true, None, IDLE).is_none());
    }

    #[test]
    fn exits_when_idle_with_nothing_to_do() {
        let last = Instant::now();
        let reason = should_exit(last + Duration::from_secs(100), last, false, false, true, None, IDLE).expect("idle long enough");
        assert!(reason.contains("no client"), "{reason}");
    }

    #[test]
    fn a_stale_build_goes_after_the_grace_even_while_it_serves_the_broker() {
        let last = Instant::now();
        let stale = Some("a newer build exists");
        let before = last + STALE_GRACE - Duration::from_secs(1);
        let after = last + STALE_GRACE;
        assert!(should_exit(before, last, false, true, true, stale, IDLE).is_none());
        let reason = should_exit(after, last, false, true, true, stale, IDLE).expect("stale and unused");
        assert!(reason.contains("newer build"), "{reason}");
        assert!(should_exit(after, last, true, true, true, stale, IDLE).is_none(), "never with a run going");
        assert!(should_exit(after, last, false, true, true, None, IDLE).is_none(), "a current build keeps serving");
    }

    #[test]
    fn a_newer_copy_or_a_newer_program_makes_a_daemon_stale() {
        let dir = tempfile::tempdir().unwrap();
        let program = tempfile::tempdir().unwrap();
        let base = std::time::SystemTime::now() - Duration::from_secs(10_000);
        let make = |p: &Path, at: u64| {
            std::fs::File::create(p).unwrap().set_modified(base + Duration::from_secs(at)).unwrap();
        };
        let mine = dir.path().join("tod-agentd-mine");
        make(&mine, 100);
        make(&program.path().join("tod-agentd"), 100);
        assert!(stale_reason(dir.path(), &mine, Some(program.path())).is_none(), "nothing newer");
        make(&dir.path().join("tod-agentd-older"), 50);
        assert!(stale_reason(dir.path(), &mine, Some(program.path())).is_none(), "an older copy is not newer");
        make(&program.path().join("tod-agentd"), 200);
        assert!(stale_reason(dir.path(), &mine, Some(program.path())).unwrap().contains("newer build"));
        make(&program.path().join("tod-agentd"), 100);
        make(&dir.path().join("tod-agentd-newer"), 300);
        assert!(stale_reason(dir.path(), &mine, Some(program.path())).unwrap().contains("tod-agentd-newer"));
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
