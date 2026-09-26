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

/// Asks `scheduler` to wake `sandbox` at `node`'s soonest pending wait, if any.
/// The schedule's id is that wait's, so rescheduling replaces it.
pub fn schedule_wake(fleet: &FleetStore, node: Uuid, scheduler: &dyn Scheduler, sandbox: &str) -> Result<Option<i64>> {
    let waits = pending(fleet, node)?;
    let Some(next) = waits.iter().min_by_key(|w| w.due_at) else {
        return Ok(None);
    };
    scheduler
        .schedule(next.id, sandbox, next.due_at)
        .with_context(|| format!("scheduling the wake for wait {}", next.id))?;
    tracing::info!(wait = %next.id, at = next.due_at, "wake scheduled");
    Ok(Some(next.due_at))
}
