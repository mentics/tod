//! What the node is waiting on (design: "The supervisor and waiting").
//!
//! The agent records waits through `tod-cli wait` (`tod_store::waits`); they
//! reach the local copy with the next pull. [`check`] settles every pending
//! wait whose time has come and says whether any is still open:
//!
//! - `until`: due means satisfied.
//! - `event`: satisfied by whoever sees the event (`tod-cli wait satisfy`);
//!   reaching its deadline expires it, so the agent looks for itself.
//! - `check`: when due, its command runs in the workspace; exit 0 satisfies
//!   it, anything else moves it `every_secs` later.
//!
//! [`schedule_wake`] asks a [`Scheduler`] to wake the sandbox at the soonest
//! pending `due_at`. Writes go through `InterviewCommand`, so they are
//! logged and synced like every other change.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;
use tod_core::scheduler::Scheduler;
use tod_store::fleet::FleetStore;
use tod_store::interview::InterviewCommand;
use tod_store::waits::{KIND_CHECK, KIND_EVENT, KIND_UNTIL, Wait, WaitRepo};
use uuid::Uuid;

/// The actor the supervisor's own writes are recorded as.
pub const ACTOR: &str = "supervisor";

/// Whether `node` is waiting, and on what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitStatus {
    /// Nothing open, or everything satisfied: work.
    Clear,
    /// Sleep: `reason` for the log.
    Waiting { reason: String },
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn pending(fleet: &FleetStore, node: Uuid) -> Result<Vec<Wait>> {
    fleet.read(|conn| WaitRepo::new(conn).list_pending_for_node(node))
}

fn run(fleet: &FleetStore, command: InterviewCommand) -> Result<()> {
    fleet.interview(ACTOR, command).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

fn set_state(fleet: &FleetStore, wait: &Wait, state: &str) -> Result<()> {
    tracing::info!(wait = %wait.id, kind = %wait.kind, state, "wait settled");
    run(fleet, InterviewCommand::SetWaitState { wait_id: wait.id, state: state.into() })
}

/// Runs a `check` wait's command in `workspace`; whether it exited 0.
fn command_succeeds(command: &str, workspace: &Path) -> bool {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    if workspace.is_dir() {
        cmd.current_dir(workspace);
    }
    match cmd.stdin(std::process::Stdio::null()).output() {
        Ok(out) => out.status.success(),
        Err(err) => {
            tracing::warn!("running wait check `{command}`: {err}");
            false
        }
    }
}

fn describe(wait: &Wait) -> String {
    match wait.kind.as_str() {
        KIND_UNTIL => format!("until {}", wait.due_at),
        KIND_EVENT => format!("event {}", wait.match_spec),
        KIND_CHECK => format!("check `{}`", wait.match_spec),
        other => other.to_string(),
    }
}

/// Settles what is due and reports what is left.
pub fn check(fleet: &FleetStore, node: Uuid, workspace: &Path) -> Result<WaitStatus> {
    let now = now_ms();
    let mut open = Vec::new();
    for wait in pending(fleet, node)? {
        if wait.due_at > now {
            open.push(describe(&wait));
            continue;
        }
        match wait.kind.as_str() {
            KIND_UNTIL => set_state(fleet, &wait, "satisfied")?,
            KIND_EVENT => set_state(fleet, &wait, "expired")?,
            KIND_CHECK => {
                if command_succeeds(&wait.match_spec, workspace) {
                    set_state(fleet, &wait, "satisfied")?;
                } else {
                    let every = wait.every_secs.unwrap_or(300).max(1);
                    run(fleet, InterviewCommand::RescheduleWait { wait_id: wait.id, due_at: now + every * 1000 })?;
                    open.push(describe(&wait));
                }
            }
            other => {
                tracing::warn!(kind = other, "unknown wait kind; expiring it");
                set_state(fleet, &wait, "expired")?;
            }
        }
    }
    Ok(if open.is_empty() { WaitStatus::Clear } else { WaitStatus::Waiting { reason: open.join(", ") } })
}

/// Cancels every wait still pending on `node`: the user asked it to run now
/// (a new session), and the run records afresh whatever it still waits on.
pub fn cancel_pending(fleet: &FleetStore, node: Uuid) -> Result<()> {
    for wait in pending(fleet, node)? {
        set_state(fleet, &wait, tod_store::waits::WAIT_CANCELLED)?;
    }
    Ok(())
}

/// Asks `scheduler` to wake `sandbox` at `node`'s soonest pending wait, if any.
/// The schedule's id is that wait's, so rescheduling replaces it.
pub fn schedule_wake(fleet: &FleetStore, node: Uuid, scheduler: &dyn Scheduler, sandbox: &str) -> Result<Option<i64>> {
    Ok(schedule_next(fleet, node, scheduler, sandbox)?.map(|(_, at)| at))
}

fn schedule_next(fleet: &FleetStore, node: Uuid, scheduler: &dyn Scheduler, sandbox: &str) -> Result<Option<(Uuid, i64)>> {
    let waits = pending(fleet, node)?;
    let Some(next) = waits.iter().min_by_key(|w| w.due_at) else {
        return Ok(None);
    };
    scheduler
        .schedule(next.id, sandbox, next.due_at)
        .with_context(|| format!("scheduling the wake for wait {}", next.id))?;
    tracing::info!(wait = %next.id, at = next.due_at, "wake scheduled");
    Ok(Some((next.id, next.due_at)))
}

/// The file in the supervisor's state directory naming the wait whose wake
/// is scheduled, so a wake left over (its wait was satisfied another way, or
/// a sooner one was scheduled instead) can be cancelled.
pub const SCHEDULED_FILE: &str = "scheduled-wake";

fn load_scheduled(state_dir: &Path) -> Option<Uuid> {
    let raw = std::fs::read_to_string(state_dir.join(SCHEDULED_FILE)).ok()?;
    Uuid::parse_str(raw.trim()).ok()
}

fn save_scheduled(state_dir: &Path, id: Option<Uuid>) -> Result<()> {
    let path = state_dir.join(SCHEDULED_FILE);
    match id {
        Some(id) => std::fs::write(&path, id.to_string()).with_context(|| format!("write {}", path.display())),
        None => match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("remove {}", path.display()))
            }
            _ => Ok(()),
        },
    }
}

/// Makes the scheduled wake match the node's waits: schedules the soonest
/// (see [`schedule_wake`]), and cancels the one scheduled before it when
/// that was for another wait, or when nothing is left to wait on. A
/// cancel that fails is only logged: a stale wake is a harmless poke.
/// Returns the wait and time scheduled.
pub fn reconcile_wake(
    fleet: &FleetStore,
    node: Uuid,
    scheduler: &dyn Scheduler,
    sandbox: &str,
    state_dir: &Path,
) -> Result<Option<(Uuid, i64)>> {
    let before = load_scheduled(state_dir);
    let next = schedule_next(fleet, node, scheduler, sandbox)?;
    if let Some(old) = before.filter(|old| Some(*old) != next.map(|(id, _)| id)) {
        match scheduler.cancel(old) {
            Ok(()) => tracing::info!(wait = %old, "stale wake cancelled"),
            Err(err) => tracing::warn!(wait = %old, "cancelling a stale wake: {err:#}"),
        }
    }
    save_scheduled(state_dir, next.map(|(id, _)| id))?;
    Ok(next)
}

/// How long after a wake began its dead-man retry fires if nothing replaced
/// it: the run is still going then (a poke of a running supervisor is
/// harmless) or it died, and this wake is what finds out.
pub const DEAD_MAN_MS: i64 = 10 * 60 * 1000;

/// The first failure's backoff, and the longest.
const BACKOFF_FIRST_MS: i64 = 60_000;
const BACKOFF_MAX_MS: i64 = 30 * 60_000;

/// How long to wait after the `failures`-th consecutive failure (1 for the
/// first): a minute, doubling, at most half an hour.
pub fn backoff_ms(failures: u32) -> i64 {
    let doublings = failures.saturating_sub(1).min(16);
    (BACKOFF_FIRST_MS << doublings).min(BACKOFF_MAX_MS)
}

/// Replaces whatever wake is scheduled with a retry wake at `at_ms`, under
/// the node's own id (a wait's id names a wait's wake). Run before work, so
/// that no window exists in which nothing would wake the node; an orderly
/// end replaces it with the real one (`reconcile_wake`) or drops it
/// (`cancel_scheduled`). A failure to schedule is returned.
pub fn arm_retry(scheduler: &dyn Scheduler, node: Uuid, sandbox: &str, state_dir: &Path, at_ms: i64) -> Result<()> {
    scheduler.schedule(node, sandbox, at_ms).context("scheduling the retry wake")?;
    if let Some(old) = load_scheduled(state_dir).filter(|old| *old != node) {
        match scheduler.cancel(old) {
            Ok(()) => tracing::info!(wait = %old, "replaced wake cancelled"),
            Err(err) => tracing::warn!(wait = %old, "cancelling a replaced wake: {err:#}"),
        }
    }
    save_scheduled(state_dir, Some(node))?;
    tracing::info!(at = at_ms, "retry wake scheduled");
    Ok(())
}

/// File counting consecutive wakes that ended in an error.
pub const WAKE_ERRORS_FILE: &str = "wake-errors";
/// File counting consecutive failed agent runs, across wakes.
pub const AGENT_FAILURES_FILE: &str = "agent-failures";

pub fn load_count(state_dir: &Path, file: &str) -> u32 {
    std::fs::read_to_string(state_dir.join(file)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

pub fn save_count(state_dir: &Path, file: &str, n: u32) -> Result<()> {
    let path = state_dir.join(file);
    if n == 0 {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("remove {}", path.display()))
            }
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(state_dir).with_context(|| format!("create {}", state_dir.display()))?;
    std::fs::write(&path, n.to_string()).with_context(|| format!("write {}", path.display()))
}

/// A wake ended in an error: schedules its retry with backoff (growing
/// with consecutive errors) through `scheduler`. Returns the time.
pub fn retry_after_error(
    scheduler: &dyn Scheduler,
    node: Uuid,
    sandbox: &str,
    state_dir: &Path,
    now_ms: i64,
) -> Result<i64> {
    let errors = load_count(state_dir, WAKE_ERRORS_FILE) + 1;
    let at = now_ms + backoff_ms(errors);
    arm_retry(scheduler, node, sandbox, state_dir, at)?;
    save_count(state_dir, WAKE_ERRORS_FILE, errors)?;
    Ok(at)
}

/// Records an `until` wait on `node`: the node sleeps until then.
pub fn record_until(fleet: &FleetStore, node: Uuid, at_ms: i64) -> Result<()> {
    run(fleet, InterviewCommand::RecordWait { node_id: node, wait: tod_store::waits::NewWait::until(at_ms) })
}

/// Cancels the wake scheduled for this node, if any: it has work now, and
/// will schedule what it needs when it stops.
pub fn cancel_scheduled(scheduler: &dyn Scheduler, state_dir: &Path) -> Result<()> {
    if let Some(old) = load_scheduled(state_dir) {
        match scheduler.cancel(old) {
            Ok(()) => tracing::info!(wait = %old, "scheduled wake cancelled"),
            Err(err) => tracing::warn!(wait = %old, "cancelling the scheduled wake: {err:#}"),
        }
        save_scheduled(state_dir, None)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct TmpDir(std::path::PathBuf);
    impl TmpDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("tod-waits-test-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<(Uuid, i64)>>,
        cancels: Mutex<Vec<Uuid>>,
    }
    impl Scheduler for Fake {
        fn schedule(&self, id: Uuid, _: &str, at: i64) -> Result<()> {
            self.calls.lock().unwrap().push((id, at));
            Ok(())
        }
        fn cancel(&self, id: Uuid) -> Result<()> {
            self.cancels.lock().unwrap().push(id);
            Ok(())
        }
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff_ms(1), 60_000);
        assert_eq!(backoff_ms(2), 120_000);
        assert_eq!(backoff_ms(3), 240_000);
        assert_eq!(backoff_ms(40), 30 * 60_000);
    }

    #[test]
    fn an_error_leaves_a_retry_schedule_that_backs_off_further_each_time() {
        let dir = TmpDir::new();
        let node = Uuid::new_v4();
        let old = Uuid::new_v4();
        save_scheduled(dir.path(), Some(old)).unwrap();
        let fake = Fake::default();
        assert_eq!(retry_after_error(&fake, node, "sb", dir.path(), 1_000).unwrap(), 61_000);
        assert_eq!(retry_after_error(&fake, node, "sb", dir.path(), 1_000).unwrap(), 121_000);
        assert_eq!(*fake.calls.lock().unwrap(), vec![(node, 61_000), (node, 121_000)]);
        // The wait's wake it replaced is cancelled, once; the retry is on record.
        assert_eq!(*fake.cancels.lock().unwrap(), vec![old]);
        assert_eq!(load_scheduled(dir.path()), Some(node));
    }

    #[test]
    fn an_orderly_end_clears_the_retry() {
        let dir = TmpDir::new();
        let node = Uuid::new_v4();
        let fake = Fake::default();
        arm_retry(&fake, node, "sb", dir.path(), 5).unwrap();
        cancel_scheduled(&fake, dir.path()).unwrap();
        assert_eq!(*fake.cancels.lock().unwrap(), vec![node]);
        assert_eq!(load_scheduled(dir.path()), None);
    }
}
