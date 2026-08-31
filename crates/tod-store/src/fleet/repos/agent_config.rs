//! Agent config repository — persistent environment configuration per task.

use crate::fleet::reconnect_identity::ReconnectIdentity;
use crate::fleet::repos::agent_run::AgentRunRepo;
use crate::outline::uuid_blob::{blob_to_uuid, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use uuid::Uuid;

/// Persistent agent environment configuration (historical, rarely deleted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    pub id: String,
    /// Node UUID string this config belongs to.
    pub node_id: String,
    pub env_type: String,
    pub mode: String,
    pub work_directory: Option<String>,
    pub use_worktree: bool,
    pub worktree_path: Option<String>,
    pub worktree_lease_id: Option<String>,
    pub worktree_lease_holder: Option<String>,
}

/// Config plus runtime status from the latest agent run (for list/display).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfigRow {
    pub id: String,
    pub node_id: String,
    pub env_type: String,
    pub mode: String,
    pub work_directory: Option<String>,
    pub use_worktree: bool,
    pub worktree_path: Option<String>,
    pub worktree_lease_id: Option<String>,
    pub worktree_lease_holder: Option<String>,
    pub runtime_status: String,
    pub active_run_id: Option<String>,
    pub reconnect: Option<ReconnectIdentity>,
}

/// Back-compat alias used across fleet runtime and views.
pub type FleetAgent = AgentConfigRow;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NewAgentConfig {
    pub id: String,
    pub node_id: String,
    pub env_type: String,
    pub mode: String,
    pub work_directory: Option<String>,
    pub use_worktree: bool,
}

/// Back-compat alias for writer mutations.
pub type NewAgent = NewAgentConfig;

#[derive(Debug, Error)]
pub enum AgentConfigRepoError {
    #[error("agent config not found")]
    NotFound,
    #[error(transparent)]
    Run(#[from] crate::fleet::repos::agent_run::AgentRunRepoError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type AgentRepoError = AgentConfigRepoError;

pub struct AgentConfigRepo<'a> {
    conn: &'a Connection,
}

/// Back-compat alias.
pub type AgentRepo<'a> = AgentConfigRepo<'a>;

impl<'a> AgentConfigRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn resolve_node_blob(node_id: &str) -> Result<Vec<u8>, AgentConfigRepoError> {
        if let Ok(uuid) = Uuid::parse_str(node_id) {
            return Ok(uuid_to_blob(uuid));
        }
        Err(AgentConfigRepoError::Other(anyhow::anyhow!(
            "invalid node id (expected UUID): {node_id}"
        )))
    }

    const SELECT_ROW: &'static str = "
        SELECT c.id, c.node_id, c.env_type, c.mode, c.work_directory, c.use_worktree, c.worktree_path,
               c.worktree_lease_id, c.worktree_lease_holder,
               COALESCE(r.runtime_status, 'not_running'), r.id, r.reconnect_pid, r.reconnect_birth_token
        FROM agent_configs c
        LEFT JOIN agent_runs r ON r.id = (
            SELECT id FROM agent_runs
            WHERE agent_config_id = c.id
            ORDER BY run_number DESC
            LIMIT 1
        )
    ";

    pub fn insert(&self, config: &NewAgentConfig) -> Result<(), AgentConfigRepoError> {
        let node_blob = Self::resolve_node_blob(&config.node_id)?;
        let now_ms: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO agent_configs (id, node_id, env_type, mode, work_directory, use_worktree, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                config.id,
                node_blob,
                config.env_type,
                config.mode,
                config.work_directory,
                i32::from(config.use_worktree),
                now_ms,
            ],
        )?;
        Ok(())
    }

    pub fn update_fields(
        &self,
        id: &str,
        env_type: &str,
        mode: &str,
        work_directory: Option<&str>,
        use_worktree: bool,
    ) -> Result<(), AgentConfigRepoError> {
        let updated = self.conn.execute(
            "UPDATE agent_configs SET env_type = ?2, mode = ?3, work_directory = ?4, use_worktree = ?5
             WHERE id = ?1",
            params![id, env_type, mode, work_directory, i32::from(use_worktree)],
        )?;
        if updated == 0 {
            return Err(AgentConfigRepoError::NotFound);
        }
        Ok(())
    }

    pub fn update_worktree(
        &self,
        id: &str,
        worktree_path: Option<&str>,
    ) -> Result<(), AgentConfigRepoError> {
        let updated = self.conn.execute(
            "UPDATE agent_configs SET worktree_path = ?2 WHERE id = ?1",
            params![id, worktree_path],
        )?;
        if updated == 0 {
            return Err(AgentConfigRepoError::NotFound);
        }
        Ok(())
    }

    pub fn update_worktree_details(
        &self,
        id: &str,
        worktree_path: Option<&str>,
        worktree_lease_id: Option<&str>,
        worktree_lease_holder: Option<&str>,
    ) -> Result<(), AgentConfigRepoError> {
        let updated = self.conn.execute(
            "UPDATE agent_configs SET worktree_path = ?2, worktree_lease_id = ?3, worktree_lease_holder = ?4
             WHERE id = ?1",
            params![id, worktree_path, worktree_lease_id, worktree_lease_holder],
        )?;
        if updated == 0 {
            return Err(AgentConfigRepoError::NotFound);
        }
        Ok(())
    }

    /// Interview-mode agent for a task node, if any.
    pub fn find_interview_for_node(
        &self,
        node_id: &str,
    ) -> Result<Option<AgentConfigRow>, AgentConfigRepoError> {
        let node_blob = Self::resolve_node_blob(node_id)?;
        let sql = format!(
            "{} WHERE c.node_id = ?1 AND c.mode = 'interview' ORDER BY c.created_at LIMIT 1",
            Self::SELECT_ROW
        );
        self.conn
            .query_row(&sql, params![node_blob], row_to_config_row)
            .optional()
            .map_err(Into::into)
    }

    /// Existing worktree path for another task with the same repo + branch.
    pub fn resolve_shared_worktree_path(
        &self,
        repo: &str,
        branch: &str,
    ) -> Result<Option<String>, AgentConfigRepoError> {
        let branch_key = if branch.is_empty() { "" } else { branch };
        self.conn
            .query_row(
                "SELECT c.worktree_path FROM agent_configs c
                 INNER JOIN node_fields nf ON nf.node_id = c.node_id
                 WHERE c.use_worktree = 1
                   AND c.worktree_path IS NOT NULL AND c.worktree_path != ''
                   AND nf.repo = ?1
                   AND COALESCE(nf.branch, '') = ?2
                 LIMIT 1",
                params![repo, branch_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_all(&self) -> Result<Vec<AgentConfigRow>, AgentConfigRepoError> {
        let sql = format!("{} ORDER BY c.id", Self::SELECT_ROW);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], row_to_config_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_for_node(
        &self,
        node_id: &str,
    ) -> Result<Vec<AgentConfigRow>, AgentConfigRepoError> {
        let node_blob = Self::resolve_node_blob(node_id)?;
        let sql = format!("{} WHERE c.node_id = ?1 ORDER BY c.id", Self::SELECT_ROW);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![node_blob], row_to_config_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_for_task(
        &self,
        task_id: &str,
    ) -> Result<Vec<AgentConfigRow>, AgentConfigRepoError> {
        self.list_for_node(task_id)
    }

    pub fn list_with_reconnect(&self) -> Result<Vec<AgentConfigRow>, AgentConfigRepoError> {
        let sql = format!(
            "{} WHERE r.reconnect_pid IS NOT NULL AND r.reconnect_birth_token IS NOT NULL ORDER BY c.id",
            Self::SELECT_ROW
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], row_to_config_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get(&self, id: &str) -> Result<Option<AgentConfigRow>, AgentConfigRepoError> {
        let sql = format!("{} WHERE c.id = ?1", Self::SELECT_ROW);
        self.conn
            .query_row(&sql, params![id], row_to_config_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn get_config(&self, id: &str) -> Result<Option<AgentConfig>, AgentConfigRepoError> {
        self.conn
            .query_row(
                "SELECT id, node_id, env_type, mode, work_directory, use_worktree, worktree_path,
                        worktree_lease_id, worktree_lease_holder
                 FROM agent_configs WHERE id = ?1",
                params![id],
                row_to_config,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Runtime status updates go through the active agent run.
    pub fn update_runtime_status(
        &self,
        config_id: &str,
        runtime_status: &str,
    ) -> Result<(), AgentConfigRepoError> {
        let run_repo = AgentRunRepo::new(self.conn);
        if let Some(run) = run_repo.latest_run(config_id)? {
            run_repo.update_runtime_status(&run.id, runtime_status)?;
            return Ok(());
        }
        run_repo.create_run(config_id, runtime_status, "auto")?;
        Ok(())
    }

    pub fn update_reconnect(
        &self,
        config_id: &str,
        identity: ReconnectIdentity,
    ) -> Result<(), AgentConfigRepoError> {
        let run_repo = AgentRunRepo::new(self.conn);
        let run = run_repo
            .latest_run(config_id)?
            .ok_or(AgentConfigRepoError::NotFound)?;
        run_repo.update_reconnect(&run.id, identity)?;
        Ok(())
    }

    pub fn clear_reconnect(&self, config_id: &str) -> Result<(), AgentConfigRepoError> {
        let run_repo = AgentRunRepo::new(self.conn);
        let run = run_repo
            .latest_run(config_id)?
            .ok_or(AgentConfigRepoError::NotFound)?;
        run_repo.clear_reconnect(&run.id)?;
        Ok(())
    }

    pub fn delete_cascade(&self, id: &str) -> Result<(), AgentConfigRepoError> {
        let notification_ids: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT notification_id FROM notification_agents WHERE agent_config_id = ?1",
            )?;
            stmt.query_map(params![id], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?
        };

        for notification_id in &notification_ids {
            self.conn.execute(
                "DELETE FROM notification_agents WHERE notification_id = ?1",
                params![notification_id],
            )?;
            self.conn.execute(
                "DELETE FROM notifications WHERE id = ?1",
                params![notification_id],
            )?;
        }

        let run_ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM agent_runs WHERE agent_config_id = ?1")?;
            stmt.query_map(params![id], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for run_id in &run_ids {
            self.conn.execute(
                "DELETE FROM transcript_turns WHERE agent_run_id = ?1",
                params![run_id],
            )?;
        }
        self.conn.execute(
            "DELETE FROM agent_runs WHERE agent_config_id = ?1",
            params![id],
        )?;
        self.conn.execute(
            "DELETE FROM shell_sessions WHERE agent_config_id = ?1",
            params![id],
        )?;
        let deleted = self
            .conn
            .execute("DELETE FROM agent_configs WHERE id = ?1", params![id])?;
        if deleted == 0 {
            return Err(AgentConfigRepoError::NotFound);
        }
        Ok(())
    }
}

fn row_to_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentConfig> {
    let node_blob: Vec<u8> = row.get(1)?;
    let node_id = blob_to_uuid(&node_blob)
        .map(|u| u.to_string())
        .unwrap_or_default();
    let use_worktree: i32 = row.get(5)?;
    Ok(AgentConfig {
        id: row.get(0)?,
        node_id,
        env_type: row.get(2)?,
        mode: row.get(3)?,
        work_directory: row.get(4)?,
        use_worktree: use_worktree != 0,
        worktree_path: row.get(6)?,
        worktree_lease_id: row.get(7)?,
        worktree_lease_holder: row.get(8)?,
    })
}

fn row_to_config_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentConfigRow> {
    let node_blob: Vec<u8> = row.get(1)?;
    let node_id = blob_to_uuid(&node_blob)
        .map(|u| u.to_string())
        .unwrap_or_default();
    let use_worktree: i32 = row.get(5)?;
    let pid: Option<i64> = row.get(11)?;
    let birth: Option<i64> = row.get(12)?;
    let reconnect = match (pid, birth) {
        (Some(pid), Some(birth)) => Some(ReconnectIdentity {
            pid: pid as u32,
            birth_token: birth as u64,
        }),
        _ => None,
    };
    Ok(AgentConfigRow {
        id: row.get(0)?,
        node_id,
        env_type: row.get(2)?,
        mode: row.get(3)?,
        work_directory: row.get(4)?,
        use_worktree: use_worktree != 0,
        worktree_path: row.get(6)?,
        worktree_lease_id: row.get(7)?,
        worktree_lease_holder: row.get(8)?,
        runtime_status: row.get(9)?,
        active_run_id: row.get(10)?,
        reconnect,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::notification::NotificationRepo;
    use crate::fleet::repos::shell::ShellRepo;
    use crate::fleet::repos::task::{FleetTask, TaskRepo};
    use crate::fleet::repos::transcript::TranscriptRepo;
    use crate::fleet::repos::{cleanup_test_dir, test_writer_conn};

    fn seed_node_config(conn: &Connection) -> (String, String) {
        let task_id = uuid::Uuid::new_v4().to_string();
        let config_id = uuid::Uuid::new_v4().to_string();
        TaskRepo::new(conn)
            .insert(&FleetTask::new(&task_id, "Task", "task"))
            .unwrap();
        AgentConfigRepo::new(conn)
            .insert(&NewAgentConfig {
                id: config_id.clone(),
                node_id: task_id.clone(),
                env_type: "local".into(),
                mode: "agent".into(),
                work_directory: None,
                use_worktree: false,
            })
            .unwrap();
        (task_id, config_id)
    }

    #[test]
    fn new_config_has_no_run_until_started() {
        let (dir, conn) = test_writer_conn();
        let (_, config_id) = seed_node_config(&conn);
        let row = AgentConfigRepo::new(&conn)
            .get(&config_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.runtime_status, "not_running");
        cleanup_test_dir(&dir);
    }

    #[test]
    fn delete_cascade_removes_children() {
        let (dir, conn) = test_writer_conn();
        let (_, config_id) = seed_node_config(&conn);
        let run = crate::fleet::repos::agent_run::AgentRunRepo::new(&conn)
            .create_run(&config_id, "waiting", "auto")
            .unwrap();
        let shell_id = uuid::Uuid::new_v4().to_string();
        ShellRepo::new(&conn)
            .create(
                &shell_id,
                &config_id,
                Some(ReconnectIdentity {
                    pid: 1,
                    birth_token: 2,
                }),
            )
            .unwrap();
        let prompt_id = uuid::Uuid::new_v4().to_string();
        TranscriptRepo::new(&conn)
            .insert_prompt(&prompt_id, &run, "hello")
            .unwrap();
        let notification_id = uuid::Uuid::new_v4().to_string();
        NotificationRepo::new(&conn)
            .create(&notification_id, "blocked", None, &[config_id.clone()])
            .unwrap();

        AgentConfigRepo::new(&conn)
            .delete_cascade(&config_id)
            .unwrap();

        assert!(
            AgentConfigRepo::new(&conn)
                .get(&config_id)
                .unwrap()
                .is_none()
        );
        cleanup_test_dir(&dir);
    }
}
