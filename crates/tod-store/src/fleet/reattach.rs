//! Launch-time reattach orchestration for agent runs and shell sessions.

use crate::fleet::repos::agent_run::AgentRunRepo;
use crate::fleet::repos::node_files::NodeFilesRepo;
use crate::fleet::repos::shell::ShellRepo;
use crate::fleet::runtime::GuestLivenessCheck;
use crate::fleet::writer::{FleetMutation, FleetWriter};
use anyhow::Result;
use rusqlite::Connection;
use tod_agent::RunLocation;

pub type HostVerifyFn = fn(u32, u64) -> bool;

/// Outcome counters from a reattach pass (for tests and diagnostics).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReattachReport {
    pub agents_live: usize,
    pub agents_not_running: usize,
    pub shells_cleared: usize,
}

/// Run reattach for all unended agent runs and shells with stored reconnect identity.
///
/// Terminal-launched agent runs are left to `prune_stale_terminal_agent_runs`,
/// which tracks them like shells.
pub fn reattach_on_launch(
    conn: &Connection,
    writer: &FleetWriter,
    guest: &dyn GuestLivenessCheck,
    host_verify: HostVerifyFn,
) -> Result<ReattachReport> {
    let mut report = ReattachReport::default();

    for run in AgentRunRepo::new(conn).list_unended()? {
        // Terminal-located runs are tracked like shells (PID + state file,
        // see `fleet/terminal/`), not via the pid+birth-token reconnect
        // identity this loop checks. Dev container / cloud VM locations will
        // eventually get their own `RunLocationOps` liveness check here too.
        if run.location == RunLocation::Terminal {
            continue;
        }
        let Some(identity) = run.reconnect else {
            // No reconnect identity means this status can't have survived a
            // fresh process start (coding-agent runs live only in the tod
            // process that launched them) — clear any stale active status
            // left over from a crash or force-quit.
            if is_active_status(&run.runtime_status) {
                mark_run_not_running(writer, &run.id)?;
                report.agents_not_running += 1;
            }
            continue;
        };
        if host_verify(identity.pid, identity.birth_token) && guest.guest_alive(&run) {
            writer.enqueue(FleetMutation::UpdateAgentRunRuntimeStatus {
                run_id: run.id.clone(),
                runtime_status: guest.live_runtime_status(&run).to_string(),
            })?;
            report.agents_live += 1;
        } else {
            mark_run_not_running(writer, &run.id)?;
            report.agents_not_running += 1;
        }
    }

    for shell in ShellRepo::new(conn).list_with_reconnect()? {
        let Some(identity) = shell.reconnect else {
            continue;
        };
        if !host_verify(identity.pid, identity.birth_token) {
            writer.enqueue(FleetMutation::ClearShellReconnect {
                id: shell.id.clone(),
            })?;
            report.shells_cleared += 1;
        }
    }

    if report.agents_live > 0 || report.agents_not_running > 0 || report.shells_cleared > 0 {
        writer.flush()?;
    }

    Ok(report)
}

fn is_active_status(status: &str) -> bool {
    matches!(status, "starting" | "processing" | "waiting" | "blocked")
}

fn mark_run_not_running(writer: &FleetWriter, run_id: &str) -> Result<()> {
    writer.enqueue(FleetMutation::UpdateAgentRunRuntimeStatus {
        run_id: run_id.to_string(),
        runtime_status: "not_running".to_string(),
    })?;
    writer.enqueue(FleetMutation::ClearAgentRunReconnect {
        run_id: run_id.to_string(),
    })?;
    Ok(())
}

/// Clear recorded worktrees whose directory no longer exists, so the Files
/// capability offers "Set up worktree" again.
pub fn clear_missing_worktrees(
    conn: &Connection,
    writer: &FleetWriter,
    notices: &crate::fleet::notices::FleetNoticeHooks,
) -> Result<usize> {
    let mut cleared = 0usize;
    for files in NodeFilesRepo::new(conn).list_with_worktree()? {
        let Some(path) = files.worktree_path() else {
            continue;
        };
        if std::path::Path::new(path).exists() {
            continue;
        }
        notices.on_worktree_missing(&files.node_id);
        writer.enqueue(FleetMutation::UpdateNodeWorktree {
            node_id: files.node_id.clone(),
            worktree_path: None,
            worktree_lease_id: None,
            worktree_lease_holder: None,
        })?;
        cleared += 1;
    }
    if cleared > 0 {
        writer.flush()?;
    }
    Ok(cleared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::reconnect_identity;
    use crate::fleet::reconnect_identity::ReconnectIdentity;
    use crate::fleet::repos::{cleanup_test_dir, seed_node, test_writer_conn};
    use crate::fleet::runtime::NoopGuestLiveness;
    use crate::fleet::writer::FleetWriter;
    use std::time::Duration;

    fn always_fail_verify(_pid: u32, _birth: u64) -> bool {
        false
    }

    fn always_pass_verify(_pid: u32, _birth: u64) -> bool {
        true
    }

    fn open_writer(dir: &std::path::Path) -> FleetWriter {
        FleetWriter::open_with_debounce(
            dir.join("tod.db"),
            Duration::from_millis(10),
            crate::fleet::command_log::CommandLog::shared(),
        )
        .unwrap()
    }

    fn seed_run(conn: &Connection, status: &str) -> String {
        let node_id = seed_node(conn);
        let run_id = AgentRunRepo::new(conn)
            .create_run(&node_id, status, "auto")
            .unwrap();
        run_id
    }

    #[test]
    fn failed_reattach_persists_not_running_without_notification() {
        let (dir, conn) = test_writer_conn();
        let writer = open_writer(&dir);
        let run_id = seed_run(&conn, "processing");
        AgentRunRepo::new(&conn)
            .update_reconnect(
                &run_id,
                ReconnectIdentity {
                    pid: 9999,
                    birth_token: 1,
                },
            )
            .unwrap();

        let report =
            reattach_on_launch(&conn, &writer, &NoopGuestLiveness, always_fail_verify).unwrap();
        assert_eq!(report.agents_not_running, 1);

        let run = AgentRunRepo::new(&conn).get(&run_id).unwrap().unwrap();
        assert_eq!(run.runtime_status, "not_running");
        assert!(run.reconnect.is_none());

        let notification_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM notifications", [], |row| row.get(0))
            .unwrap();
        assert_eq!(notification_count, 0);

        writer.shutdown().unwrap();
        cleanup_test_dir(&dir);
    }

    #[test]
    fn stale_status_without_reconnect_identity_is_cleared() {
        let (dir, conn) = test_writer_conn();
        let writer = open_writer(&dir);
        // Simulate a crash / force-quit: the run was left "waiting" with no
        // reconnect identity ever recorded.
        let run_id = seed_run(&conn, "waiting");

        let report =
            reattach_on_launch(&conn, &writer, &NoopGuestLiveness, always_fail_verify).unwrap();
        assert_eq!(report.agents_not_running, 1);
        assert_eq!(report.agents_live, 0);

        let run = AgentRunRepo::new(&conn).get(&run_id).unwrap().unwrap();
        assert_eq!(run.runtime_status, "not_running");

        writer.shutdown().unwrap();
        cleanup_test_dir(&dir);
    }

    #[test]
    fn successful_reattach_persists_live_status() {
        let (dir, conn) = test_writer_conn();
        let writer = open_writer(&dir);
        let run_id = seed_run(&conn, "processing");
        let identity = reconnect_identity::record(std::process::id()).expect("current pid");
        AgentRunRepo::new(&conn)
            .update_reconnect(&run_id, identity)
            .unwrap();

        let report =
            reattach_on_launch(&conn, &writer, &NoopGuestLiveness, always_pass_verify).unwrap();
        assert_eq!(report.agents_live, 1);

        let run = AgentRunRepo::new(&conn).get(&run_id).unwrap().unwrap();
        assert_eq!(run.runtime_status, "waiting");

        writer.shutdown().unwrap();
        cleanup_test_dir(&dir);
    }

    #[test]
    fn missing_worktree_is_cleared() {
        let (dir, conn) = test_writer_conn();
        let writer = open_writer(&dir);
        let node_id = seed_node(&conn);
        let missing = dir.join("no-such-worktree");
        NodeFilesRepo::new(&conn)
            .update_worktree(&node_id, Some(&missing.display().to_string()), None, None)
            .unwrap();
        let notices = crate::fleet::notices::FleetNoticeHooks::new();

        assert_eq!(clear_missing_worktrees(&conn, &writer, &notices).unwrap(), 1);
        let files = NodeFilesRepo::new(&conn).get(&node_id).unwrap().unwrap();
        assert!(files.worktree_path().is_none());
        assert!(files.use_worktree);
        assert_eq!(notices.worktree_missing_notices(), vec![node_id]);

        writer.shutdown().unwrap();
        cleanup_test_dir(&dir);
    }
}
