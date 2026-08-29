//! Agent repository — create, status, worktree, reconnect identity, removal cascade.

use crate::fleet::reconnect_identity::ReconnectIdentity;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetAgent {
    pub id: String,
    pub task_id: String,
    pub env_type: String,
    pub mode: String,
    pub runtime_status: String,
    pub worktree_path: Option<String>,
    pub reconnect: Option<ReconnectIdentity>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NewAgent {
    pub id: String,
    pub task_id: String,
    pub env_type: String,
    pub mode: String,
}

#[derive(Debug, Error)]
pub enum AgentRepoError {
    #[error("agent not found")]
    NotFound,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct AgentRepo<'a> {
    conn: &'a Connection,
}

impl<'a> AgentRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Create agent with immutable env_type and initial **starting** status.
    pub fn insert(&self, agent: &NewAgent) -> Result<(), AgentRepoError> {
        self.conn.execute(
            "INSERT INTO agents (id, task_id, env_type, mode, runtime_status)
             VALUES (?1, ?2, ?3, ?4, 'starting')",
            params![agent.id, agent.task_id, agent.env_type, agent.mode],
        )?;
        Ok(())
    }

    pub fn update_runtime_status(
        &self,
        id: &str,
        runtime_status: &str,
    ) -> Result<(), AgentRepoError> {
        let updated = self.conn.execute(
            "UPDATE agents SET runtime_status = ?2 WHERE id = ?1",
            params![id, runtime_status],
        )?;
        if updated == 0 {
            return Err(AgentRepoError::NotFound);
        }
        Ok(())
    }

    pub fn update_worktree(
        &self,
        id: &str,
        worktree_path: Option<&str>,
    ) -> Result<(), AgentRepoError> {
        let updated = self.conn.execute(
            "UPDATE agents SET worktree_path = ?2 WHERE id = ?1",
            params![id, worktree_path],
        )?;
        if updated == 0 {
            return Err(AgentRepoError::NotFound);
        }
        Ok(())
    }

    pub fn update_reconnect(
        &self,
        id: &str,
        identity: ReconnectIdentity,
    ) -> Result<(), AgentRepoError> {
        let updated = self.conn.execute(
            "UPDATE agents SET reconnect_pid = ?2, reconnect_birth_token = ?3 WHERE id = ?1",
            params![id, identity.pid as i64, identity.birth_token as i64],
        )?;
        if updated == 0 {
            return Err(AgentRepoError::NotFound);
        }
        Ok(())
    }

    pub fn list_all(&self) -> Result<Vec<FleetAgent>, AgentRepoError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, env_type, mode, runtime_status, worktree_path,
                    reconnect_pid, reconnect_birth_token
             FROM agents ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], row_to_agent)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_for_task(&self, task_id: &str) -> Result<Vec<FleetAgent>, AgentRepoError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, env_type, mode, runtime_status, worktree_path,
                    reconnect_pid, reconnect_birth_token
             FROM agents WHERE task_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![task_id], row_to_agent)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_with_reconnect(&self) -> Result<Vec<FleetAgent>, AgentRepoError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, env_type, mode, runtime_status, worktree_path,
                    reconnect_pid, reconnect_birth_token
             FROM agents
             WHERE reconnect_pid IS NOT NULL AND reconnect_birth_token IS NOT NULL
             ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], row_to_agent)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn clear_reconnect(&self, id: &str) -> Result<(), AgentRepoError> {
        let updated = self.conn.execute(
            "UPDATE agents SET reconnect_pid = NULL, reconnect_birth_token = NULL WHERE id = ?1",
            params![id],
        )?;
        if updated == 0 {
            return Err(AgentRepoError::NotFound);
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<FleetAgent>, AgentRepoError> {
        self.conn
            .query_row(
                "SELECT id, task_id, env_type, mode, runtime_status, worktree_path,
                        reconnect_pid, reconnect_birth_token
                 FROM agents WHERE id = ?1",
                params![id],
                row_to_agent,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Hard-delete agent and linked shells, transcripts, and notifications in one transaction.
    pub fn delete_cascade(&self, id: &str) -> Result<(), AgentRepoError> {
        let notification_ids: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT notification_id FROM notification_agents WHERE agent_id = ?1",
            )?;
            stmt.query_map(params![id], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?
        };

        for notification_id in &notification_ids {
            self.conn.execute(
                "DELETE FROM notification_agents WHERE notification_id = ?1",
                params![notification_id],
            )?;
            self.conn
                .execute("DELETE FROM notifications WHERE id = ?1", params![notification_id])?;
        }

        self.conn
            .execute("DELETE FROM transcript_turns WHERE agent_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM shell_sessions WHERE agent_id = ?1", params![id])?;
        let deleted = self.conn.execute("DELETE FROM agents WHERE id = ?1", params![id])?;
        if deleted == 0 {
            return Err(AgentRepoError::NotFound);
        }
        Ok(())
    }
}

fn row_to_agent(row: &rusqlite::Row<'_>) -> rusqlite::Result<FleetAgent> {
    let pid: Option<i64> = row.get(6)?;
    let birth: Option<i64> = row.get(7)?;
    let reconnect = match (pid, birth) {
        (Some(pid), Some(birth)) => Some(ReconnectIdentity {
            pid: pid as u32,
            birth_token: birth as u64,
        }),
        _ => None,
    };
    Ok(FleetAgent {
        id: row.get(0)?,
        task_id: row.get(1)?,
        env_type: row.get(2)?,
        mode: row.get(3)?,
        runtime_status: row.get(4)?,
        worktree_path: row.get(5)?,
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

    fn seed_task_agent(conn: &Connection) -> (String, String) {
        let task_id = uuid::Uuid::new_v4().to_string();
        let agent_id = uuid::Uuid::new_v4().to_string();
        TaskRepo::new(conn)
            .insert(&FleetTask::new(&task_id, "Task", "task"))
            .unwrap();
        AgentRepo::new(conn)
            .insert(&NewAgent {
                id: agent_id.clone(),
                task_id: task_id.clone(),
                env_type: "local".into(),
                mode: "agent".into(),
            })
            .unwrap();
        (task_id, agent_id)
    }

    #[test]
    fn create_starts_in_starting_status() {
        let (dir, conn) = test_writer_conn();
        let (_, agent_id) = seed_task_agent(&conn);
        let agent = AgentRepo::new(&conn).get(&agent_id).unwrap().unwrap();
        assert_eq!(agent.runtime_status, "starting");
        assert_eq!(agent.env_type, "local");
        cleanup_test_dir(&dir);
    }

    #[test]
    fn delete_cascade_removes_children() {
        let (dir, conn) = test_writer_conn();
        let (_, agent_id) = seed_task_agent(&conn);
        let shell_id = uuid::Uuid::new_v4().to_string();
        ShellRepo::new(&conn)
            .create(&shell_id, &agent_id, Some(ReconnectIdentity { pid: 1, birth_token: 2 }))
            .unwrap();
        let prompt_id = uuid::Uuid::new_v4().to_string();
        TranscriptRepo::new(&conn)
            .insert_prompt(&prompt_id, &agent_id, "hello")
            .unwrap();
        let notification_id = uuid::Uuid::new_v4().to_string();
        NotificationRepo::new(&conn)
            .create(&notification_id, "blocked", None, &[agent_id.clone()])
            .unwrap();

        AgentRepo::new(&conn).delete_cascade(&agent_id).unwrap();

        assert!(AgentRepo::new(&conn).get(&agent_id).unwrap().is_none());
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM shell_sessions WHERE agent_id = ?1",
                params![agent_id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM transcript_turns WHERE agent_id = ?1",
                params![agent_id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM notifications WHERE id = ?1",
                params![notification_id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        cleanup_test_dir(&dir);
    }
}
