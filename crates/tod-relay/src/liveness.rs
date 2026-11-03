//! The relay as the one owner of the supervisor's liveness.
//!
//! Two things the supervisor cannot do for itself, both read from its state
//! directory (`/var/lib/tod-supervisor/<node>/`), which the supervisor keeps:
//!
//! - `supervisor.lock` holds the pid of the running supervisor (it is
//!   `flock`ed for the whole wake, `tod_supervisor::lock`). A supervisor the
//!   relay did not start is **untracked**: the process API's fallback in a
//!   wake schedule (`tod_sandbox::node::wake_command`) runs it directly when
//!   the relay was down, under a 120 s process timeout, and with no relay to
//!   hold the sandbox for it, release its holds, or signal it. The relay
//!   **adopts** it: ends it and starts one of its own, which it tracks. The
//!   node's state is the replica, so nothing is lost; the dead-man schedule
//!   (`tod_supervisor::waits::arm_retry`) stays.
//! - `progress` is touched whenever the supervisor's work moves
//!   (`tod_supervisor::progress`). One it started that has made none for
//!   [`STALL_LIMIT`] is wedged, and is killed: its holds are released as for
//!   any exit, and the dead-man wake starts the next.
//!
//! The decisions are pure ([`decide`]) so they are tested without processes.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// The lock file's name (`tod_supervisor::lock::LOCK_FILE`).
pub const LOCK_FILE: &str = "supervisor.lock";
/// The progress marker's name (`tod_supervisor::progress::PROGRESS_FILE`).
pub const PROGRESS_FILE: &str = "progress";

/// The file the supervisor writes beside its progress marker: its agent
/// hang limit in minutes (`tod_supervisor::progress::HANG_LIMIT_FILE`).
pub const HANG_LIMIT_FILE: &str = "hang-limit-minutes";
/// The agent hang limit the supervisor uses unless `TOD_HANG_MINUTES` says
/// otherwise; assumed when the file is absent (an older supervisor).
pub const DEFAULT_HANG_LIMIT: Duration = Duration::from_secs(15 * 60);
/// The supervisor stops claiming to work (no hold renewal, no heartbeat) at
/// its hang limit plus this (`tod_supervisor`'s `STALL_MARGIN`).
const SUPERVISOR_MARGIN: Duration = Duration::from_secs(10 * 60);
/// What the relay waits beyond that, so the supervisor's own gate acts first.
const RELAY_SLACK: Duration = Duration::from_secs(15 * 60);

/// How long a supervisor with this agent hang limit may go without marking
/// progress before the relay kills it: the limit plus the supervisor's own
/// margin plus the relay's slack (40 minutes at the default 15).
pub fn stall_limit(hang_limit: Duration) -> Duration {
    hang_limit + SUPERVISOR_MARGIN + RELAY_SLACK
}

/// The stall limit when the supervisor recorded no hang limit.
pub const STALL_LIMIT: Duration = Duration::from_secs(40 * 60);

/// A supervisor named by a state directory's lock file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub pid: u32,
    /// Time since the progress marker was last touched; `None` when there
    /// is no marker.
    pub progress_age: Option<Duration>,
    /// The agent hang limit the supervisor recorded; `None` when it has not.
    pub hang_limit: Option<Duration>,
}

/// Looks through `root`'s subdirectories (one per node; a sandbox has one)
/// for a live supervisor: a lock file naming a pid that `is_supervisor`
/// says is one. A missing root is no supervisor; anything else that cannot
/// be read is an error, not "none".
pub fn find(root: &Path, now: SystemTime, is_supervisor: &dyn Fn(u32) -> bool) -> std::io::Result<Option<Found>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let dir = entry?.path();
        let text = match std::fs::read_to_string(dir.join(LOCK_FILE)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        // Written just after the lock is taken: briefly empty.
        let Some(pid) = text.trim().parse::<u32>().ok().filter(|p| is_supervisor(*p)) else { continue };
        let progress_age = match std::fs::metadata(dir.join(PROGRESS_FILE)).and_then(|m| m.modified()) {
            Ok(at) => Some(now.duration_since(at).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let hang_limit = match std::fs::read_to_string(dir.join(HANG_LIMIT_FILE)) {
            Ok(text) => Some(Duration::from_secs(
                text.trim().parse::<u64>().map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{HANG_LIMIT_FILE} in {}: {e}", dir.display()))
                })? * 60,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        return Ok(Some(Found { pid, progress_age, hang_limit }));
    }
    Ok(None)
}

/// What a look at the supervisor comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// The supervisor the relay started has made no progress for `age`.
    KillStalled { pid: u32, age: Duration },
    /// A supervisor the relay did not start is running: replace it.
    Adopt { pid: u32 },
}

/// `tracked` is the supervisor the relay started and is still alive, with
/// how long ago it started; `found` is what the state directory names.
/// `limit` is the stall limit used when `found` records no hang limit.
pub fn decide(tracked: Option<(u32, Duration)>, found: Option<&Found>, limit: Duration) -> Action {
    match (tracked, found) {
        (Some((pid, since_start)), found) => {
            // A marker older than this supervisor is from an earlier one:
            // time since its own start is the most that can have passed.
            let age = match found.filter(|f| f.pid == pid).and_then(|f| f.progress_age) {
                Some(age) => age.min(since_start),
                None => since_start,
            };
            let limit = found.filter(|f| f.pid == pid).and_then(|f| f.hang_limit).map_or(limit, stall_limit);
            if age > limit { Action::KillStalled { pid, age } } else { Action::Nothing }
        }
        (None, Some(found)) => Action::Adopt { pid: found.pid },
        (None, None) => Action::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_secs(600);

    fn found(pid: u32, age: Option<u64>) -> Found {
        Found { pid, progress_age: age.map(Duration::from_secs), hang_limit: None }
    }

    #[test]
    fn a_tracked_supervisor_with_recent_progress_is_left_alone() {
        let started = Duration::from_secs(5000);
        assert_eq!(decide(Some((7, started)), Some(&found(7, Some(30))), LIMIT), Action::Nothing);
    }

    #[test]
    fn a_tracked_supervisor_with_no_progress_for_the_limit_is_killed() {
        let started = Duration::from_secs(5000);
        assert_eq!(
            decide(Some((7, started)), Some(&found(7, Some(601))), LIMIT),
            Action::KillStalled { pid: 7, age: Duration::from_secs(601) }
        );
    }

    /// A marker left by an earlier supervisor must not condemn a new one
    /// before it has had the time to mark anything.
    #[test]
    fn a_marker_from_before_this_supervisor_started_does_not_count() {
        let just_started = Duration::from_secs(3);
        assert_eq!(decide(Some((7, just_started)), Some(&found(7, Some(90_000))), LIMIT), Action::Nothing);
        // No marker at all: the time since it started is all there is.
        assert_eq!(decide(Some((7, just_started)), Some(&found(7, None)), LIMIT), Action::Nothing);
        assert_eq!(decide(Some((7, just_started)), None, LIMIT), Action::Nothing);
        assert_eq!(
            decide(Some((7, Duration::from_secs(700))), Some(&found(7, None)), LIMIT),
            Action::KillStalled { pid: 7, age: Duration::from_secs(700) }
        );
    }

    #[test]
    fn a_supervisor_the_relay_did_not_start_is_adopted() {
        assert_eq!(decide(None, Some(&found(42, Some(1))), LIMIT), Action::Adopt { pid: 42 });
        assert_eq!(decide(None, None, LIMIT), Action::Nothing);
    }

    #[test]
    fn the_stall_limit_follows_the_recorded_hang_limit() {
        assert_eq!(stall_limit(DEFAULT_HANG_LIMIT), STALL_LIMIT);
        assert_eq!(stall_limit(Duration::from_secs(60 * 60)), Duration::from_secs(85 * 60));
        let started = Duration::from_secs(100_000);
        let mut f = found(7, Some(50 * 60));
        assert!(matches!(decide(Some((7, started)), Some(&f), STALL_LIMIT), Action::KillStalled { .. }));
        f.hang_limit = Some(Duration::from_secs(60 * 60));
        assert_eq!(decide(Some((7, started)), Some(&f), STALL_LIMIT), Action::Nothing);
        f.progress_age = Some(Duration::from_secs(86 * 60));
        assert!(matches!(decide(Some((7, started)), Some(&f), STALL_LIMIT), Action::KillStalled { .. }));
    }

    #[test]
    fn find_reads_the_recorded_hang_limit() {
        let root = state_root("hang");
        let node = root.join("n");
        std::fs::create_dir_all(&node).unwrap();
        std::fs::write(node.join(LOCK_FILE), "5").unwrap();
        std::fs::write(node.join(HANG_LIMIT_FILE), "30").unwrap();
        let f = find(&root, SystemTime::now(), &|_| true).unwrap().unwrap();
        assert_eq!(f.hang_limit, Some(Duration::from_secs(30 * 60)));
        std::fs::write(node.join(HANG_LIMIT_FILE), "soon").unwrap();
        assert!(find(&root, SystemTime::now(), &|_| true).is_err(), "a garbled file is an error, not the default");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn state_root(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("tod-relay-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn find_reads_the_lock_and_the_progress_marker() {
        let root = state_root("find");
        assert_eq!(find(&root.join("absent"), SystemTime::now(), &|_| true).unwrap(), None);
        let node = root.join("node-1");
        std::fs::create_dir_all(&node).unwrap();
        assert_eq!(find(&root, SystemTime::now(), &|_| true).unwrap(), None, "no lock file yet");
        std::fs::write(node.join(LOCK_FILE), "").unwrap();
        assert_eq!(find(&root, SystemTime::now(), &|_| true).unwrap(), None, "pid not written yet");
        std::fs::write(node.join(LOCK_FILE), "1234").unwrap();
        assert_eq!(find(&root, SystemTime::now(), &|_| false).unwrap(), None, "no such supervisor alive");
        let f = find(&root, SystemTime::now(), &|p| p == 1234).unwrap().unwrap();
        assert_eq!(f, Found { pid: 1234, progress_age: None, hang_limit: None });
        std::fs::write(node.join(PROGRESS_FILE), "1").unwrap();
        let later = SystemTime::now() + Duration::from_secs(120);
        let f = find(&root, later, &|p| p == 1234).unwrap().unwrap();
        assert!(f.progress_age.unwrap() >= Duration::from_secs(119), "{f:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
