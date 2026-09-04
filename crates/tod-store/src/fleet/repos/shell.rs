//! Shell session repository — per-agent rows with reconnect identity.

use crate::fleet::reconnect_identity::ReconnectIdentity;
use anyhow::Result;
use rusqlite::{Connection, params};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSession {
    pub id: String,
    pub agent_id: String,
    pub label_number: u32,
    pub reconnect: Option<ReconnectIdentity>,
}

#[derive(Debug, Error)]
pub enum ShellRepoError {
    #[error("shell session not found")]
    NotFound,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct ShellRepo<'a> {
    conn: &'a Connection,
}

impl<'a> ShellRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn create(
        &self,
        id: &str,
        agent_id: &str,
        reconnect: Option<ReconnectIdentity>,
    ) -> Result<(), ShellRepoError> {
        let label_number = self.next_label_number(agent_id)?;
        let (pid, birth) = match reconnect {
            Some(id) => (Some(id.pid as i64), Some(id.birth_token as i64)),
            None => (None, None),
        };
        self.conn.execute(
            "INSERT INTO shell_sessions (id, agent_config_id, reconnect_pid, reconnect_birth_token, label_number)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, agent_id, pid, birth, label_number],
        )?;
        Ok(())
    }

    fn next_label_number(&self, agent_id: &str) -> Result<u32, ShellRepoError> {
        let next: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(label_number), 0) + 1 FROM shell_sessions WHERE agent_config_id = ?1",
            params![agent_id],
            |row| row.get(0),
        )?;
        Ok(next as u32)
    }

    pub fn list_all(&self) -> Result<Vec<ShellSession>, ShellRepoError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent_config_id, reconnect_pid, reconnect_birth_token, label_number
             FROM shell_sessions ORDER BY label_number, id",
        )?;
        let rows = stmt
            .query_map([], row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_with_reconnect(&self) -> Result<Vec<ShellSession>, ShellRepoError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent_config_id, reconnect_pid, reconnect_birth_token, label_number
             FROM shell_sessions
             WHERE reconnect_pid IS NOT NULL AND reconnect_birth_token IS NOT NULL
             ORDER BY label_number, id",
        )?;
        let rows = stmt
            .query_map([], row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn clear_reconnect(&self, id: &str) -> Result<(), ShellRepoError> {
        let updated = self.conn.execute(
            "UPDATE shell_sessions SET reconnect_pid = NULL, reconnect_birth_token = NULL
             WHERE id = ?1",
            params![id],
        )?;
        if updated == 0 {
            return Err(ShellRepoError::NotFound);
        }
        Ok(())
    }

    pub fn find(&self, id: &str) -> Result<Option<ShellSession>, ShellRepoError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent_config_id, reconnect_pid, reconnect_birth_token, label_number
             FROM shell_sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], row_to_session)?;
        Ok(rows.next().transpose()?)
    }

    pub fn list_for_agent(&self, agent_id: &str) -> Result<Vec<ShellSession>, ShellRepoError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent_config_id, reconnect_pid, reconnect_birth_token, label_number
             FROM shell_sessions WHERE agent_config_id = ?1 ORDER BY label_number, id",
        )?;
        let rows = stmt
            .query_map(params![agent_id], row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Dismiss = hard-delete shell session row.
    pub fn dismiss(&self, id: &str) -> Result<(), ShellRepoError> {
        let deleted = self
            .conn
            .execute("DELETE FROM shell_sessions WHERE id = ?1", params![id])?;
        if deleted == 0 {
            return Err(ShellRepoError::NotFound);
        }
        Ok(())
    }
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<ShellSession> {
    let pid: Option<i64> = row.get(2)?;
    let birth: Option<i64> = row.get(3)?;
    let reconnect = match (pid, birth) {
        (Some(pid), Some(birth)) => Some(ReconnectIdentity {
            pid: pid as u32,
            birth_token: birth as u64,
        }),
        _ => None,
    };
    Ok(ShellSession {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        reconnect,
        label_number: row.get::<_, i64>(4)? as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::agent_config::{AgentConfigRepo, NewAgentConfig};
    use crate::fleet::repos::task::{FleetTask, TaskRepo};
    use crate::fleet::repos::{cleanup_test_dir, test_writer_conn};

    fn seed_agent(conn: &Connection) -> String {
        let task_id = uuid::Uuid::new_v4().to_string();
        let agent_id = uuid::Uuid::new_v4().to_string();
        TaskRepo::new(conn)
            .insert(&FleetTask::new(&task_id, "T", "t"))
            .unwrap();
        AgentConfigRepo::new(conn)
            .insert(&NewAgentConfig {
                id: agent_id.clone(),
                node_id: task_id,
                env_type: "local".into(),
                mode: "agent".into(),
                work_directory: None,
                use_worktree: false,
            })
            .unwrap();
        agent_id
    }

    #[test]
    fn multiple_sessions_per_agent() {
        let (dir, conn) = test_writer_conn();
        let agent_id = seed_agent(&conn);
        let repo = ShellRepo::new(&conn);
        let s1 = uuid::Uuid::new_v4().to_string();
        let s2 = uuid::Uuid::new_v4().to_string();
        repo.create(
            &s1,
            &agent_id,
            Some(ReconnectIdentity {
                pid: 100,
                birth_token: 200,
            }),
        )
        .unwrap();
        repo.create(
            &s2,
            &agent_id,
            Some(ReconnectIdentity {
                pid: 101,
                birth_token: 201,
            }),
        )
        .unwrap();

        let sessions = repo.list_for_agent(&agent_id).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].label_number, 1);
        assert_eq!(sessions[1].label_number, 2);
        assert_ne!(sessions[0].id, sessions[1].id);

        repo.dismiss(&s1).unwrap();
        let remaining = repo.list_for_agent(&agent_id).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, s2);
        assert_eq!(remaining[0].label_number, 2);

        let s3 = uuid::Uuid::new_v4().to_string();
        repo.create(
            &s3,
            &agent_id,
            Some(ReconnectIdentity {
                pid: 102,
                birth_token: 202,
            }),
        )
        .unwrap();
        let sessions = repo.list_for_agent(&agent_id).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].label_number, 2);
        assert_eq!(sessions[1].label_number, 3);
        cleanup_test_dir(&dir);
    }
}
