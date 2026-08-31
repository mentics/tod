//! Launch-time reattach orchestration for agents and shell sessions.

use crate::fleet::repos::agent_config::AgentConfigRepo;
use crate::fleet::repos::shell::ShellRepo;
use crate::fleet::runtime::GuestLivenessCheck;
use crate::fleet::writer::{FleetMutation, FleetWriter};
use anyhow::Result;
use rusqlite::Connection;

pub type HostVerifyFn = fn(u32, u64) -> bool;

/// Outcome counters from a reattach pass (for tests and diagnostics).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReattachReport {
    pub agents_live: usize,
    pub agents_not_running: usize,
    pub shells_cleared: usize,
}

/// Run reattach for all agents/shells with stored reconnect identity.
pub fn reattach_on_launch(
    conn: &Connection,
    writer: &FleetWriter,
    guest: &dyn GuestLivenessCheck,
    host_verify: HostVerifyFn,
) -> Result<ReattachReport> {
    let mut report = ReattachReport::default();
    let agent_repo = AgentConfigRepo::new(conn);
    let shell_repo = ShellRepo::new(conn);

    let agents = agent_repo.list_with_reconnect()?;
    for agent in agents {
        let Some(identity) = agent.reconnect else {
            continue;
        };
        if host_verify(identity.pid, identity.birth_token) && guest.guest_alive(&agent) {
            writer.enqueue(FleetMutation::UpdateAgentRuntimeStatus {
                id: agent.id.clone(),
                runtime_status: guest.live_runtime_status(&agent).to_string(),
            })?;
            report.agents_live += 1;
        } else {
            mark_agent_not_running(writer, &agent.id)?;
            report.agents_not_running += 1;
        }
    }

    let shells = shell_repo.list_with_reconnect()?;
    for shell in shells {
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

fn mark_agent_not_running(writer: &FleetWriter, agent_id: &str) -> Result<()> {
    writer.enqueue(FleetMutation::UpdateAgentRuntimeStatus {
        id: agent_id.to_string(),
        runtime_status: "not_running".to_string(),
    })?;
    writer.enqueue(FleetMutation::ClearAgentReconnect {
        id: agent_id.to_string(),
    })?;
    writer.enqueue(FleetMutation::MarkAgentPromptsInterrupted {
        agent_id: agent_id.to_string(),
    })?;
    Ok(())
}

/// Remove agents whose worktree path no longer exists (local/devcontainer relaunch hook).
pub fn remove_agents_with_missing_worktrees(
    conn: &Connection,
    writer: &FleetWriter,
    notices: &crate::fleet::notices::FleetNoticeHooks,
) -> Result<usize> {
    let repo = AgentConfigRepo::new(conn);
    let mut removed = 0usize;
    for agent in repo.list_all()? {
        let Some(path) = agent.worktree_path.as_deref() else {
            continue;
        };
        if agent.env_type != "local" && agent.env_type != "devcontainer" {
            continue;
        }
        if !std::path::Path::new(path).exists() {
            notices.on_worktree_missing(&agent.id);
            writer.enqueue(FleetMutation::DeleteAgent {
                id: agent.id.clone(),
            })?;
            notices.on_agent_auto_deleted(&agent.id);
            removed += 1;
        }
    }
    if removed > 0 {
        writer.flush()?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::reconnect_identity::ReconnectIdentity;
    use crate::fleet::repos::agent_config::{AgentConfigRepo as AgentRepo, NewAgentConfig as NewAgent};
    use crate::fleet::repos::task::{FleetTask, TaskRepo};
    use crate::fleet::repos::{cleanup_test_dir, test_writer_conn};
    use crate::fleet::runtime::NoopGuestLiveness;
    use crate::fleet::writer::FleetWriter;
    use std::time::Duration;

    fn always_fail_verify(_pid: u32, _birth: u64) -> bool {
        false
    }

    fn always_pass_verify(_pid: u32, _birth: u64) -> bool {
        true
    }

    #[test]
    fn failed_reattach_persists_not_running_without_notification() {
        let (dir, conn) = test_writer_conn();
        let path = dir.join("tod.db");
        let writer = FleetWriter::open_with_debounce(&path, Duration::from_millis(10), crate::fleet::command_log::CommandLog::shared()).unwrap();
        let task_id = uuid::Uuid::new_v4().to_string();
        let agent_id = uuid::Uuid::new_v4().to_string();
        TaskRepo::new(&conn)
            .insert(&FleetTask::new(&task_id, "T", "t"))
            .unwrap();
        AgentRepo::new(&conn)
            .insert(&NewAgent {
                id: agent_id.clone(),
                node_id: task_id,
                env_type: "local".into(),
                mode: "agent".into(),
                work_directory: None,
                use_worktree: false,
            })
            .unwrap();
        AgentRepo::new(&conn)
            .update_runtime_status(&agent_id, "processing")
            .unwrap();
        AgentRepo::new(&conn)
            .update_reconnect(
                &agent_id,
                ReconnectIdentity {
                    pid: 9999,
                    birth_token: 1,
                },
            )
            .unwrap();

        let report = reattach_on_launch(&conn, &writer, &NoopGuestLiveness, always_fail_verify)
            .unwrap();
        assert_eq!(report.agents_not_running, 1);

        let agent = AgentRepo::new(&conn).get(&agent_id).unwrap().unwrap();
        assert_eq!(agent.runtime_status, "not_running");
        assert!(agent.reconnect.is_none());

        let notification_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM notifications", [], |row| row.get(0))
            .unwrap();
        assert_eq!(notification_count, 0);

        writer.shutdown().unwrap();
        cleanup_test_dir(&dir);
    }

    #[test]
    fn successful_reattach_persists_live_status() {
        let (dir, conn) = test_writer_conn();
        let path = dir.join("tod.db");
        let writer = FleetWriter::open_with_debounce(&path, Duration::from_millis(10), crate::fleet::command_log::CommandLog::shared()).unwrap();
        let task_id = uuid::Uuid::new_v4().to_string();
        let agent_id = uuid::Uuid::new_v4().to_string();
        TaskRepo::new(&conn)
            .insert(&FleetTask::new(&task_id, "T", "t"))
            .unwrap();
        AgentRepo::new(&conn)
            .insert(&NewAgent {
                id: agent_id.clone(),
                node_id: task_id,
                env_type: "local".into(),
                mode: "agent".into(),
                work_directory: None,
                use_worktree: false,
            })
            .unwrap();
        AgentRepo::new(&conn)
            .update_runtime_status(&agent_id, "processing")
            .unwrap();
        let identity = reconnect_identity::record(std::process::id()).expect("current pid");
        AgentRepo::new(&conn)
            .update_reconnect(&agent_id, identity)
            .unwrap();

        let report = reattach_on_launch(&conn, &writer, &NoopGuestLiveness, always_pass_verify)
            .unwrap();
        assert_eq!(report.agents_live, 1);

        let agent = AgentRepo::new(&conn).get(&agent_id).unwrap().unwrap();
        assert_eq!(agent.runtime_status, "waiting");

        writer.shutdown().unwrap();
        cleanup_test_dir(&dir);
    }
}
