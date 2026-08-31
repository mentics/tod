//! Agent run repository — ephemeral agent process instances tied to a config.

use crate::fleet::reconnect_identity::ReconnectIdentity;
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRun {
    pub id: String,
    pub agent_config_id: String,
    pub run_number: i64,
    pub runtime_status: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub reconnect: Option<ReconnectIdentity>,
    pub run_kind: String,
}

const RUN_SELECT: &str = "SELECT id, agent_config_id, run_number, runtime_status, started_at, ended_at,
                    reconnect_pid, reconnect_birth_token, run_kind";

#[derive(Debug, Error)]
pub enum AgentRunRepoError {
    #[error("agent run not found")]
    NotFound,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

pub struct AgentRunRepo<'a> {
    conn: &'a Connection,
}

impl<'a> AgentRunRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn next_run_number(&self, config_id: &str) -> Result<i64, AgentRunRepoError> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(run_number), 0) + 1 FROM agent_runs WHERE agent_config_id = ?1",
            params![config_id],
            |row| row.get(0),
        )?;
        Ok(n)
    }

    /// Create a new run; returns run id `{config-id}-run-{n}`.
    pub fn create_run(
        &self,
        config_id: &str,
        runtime_status: &str,
        run_kind: &str,
    ) -> Result<String, AgentRunRepoError> {
        let run_number = self.next_run_number(config_id)?;
        let run_id = format!("{config_id}-run-{run_number}");
        let now_ms: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO agent_runs (id, agent_config_id, run_number, runtime_status, started_at, run_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![run_id, config_id, run_number, runtime_status, now_ms, run_kind],
        )?;
        Ok(run_id)
    }

    pub fn list_for_config(&self, config_id: &str) -> Result<Vec<AgentRun>, AgentRunRepoError> {
        let sql = format!(
            "{RUN_SELECT}
             FROM agent_runs
             WHERE agent_config_id = ?1
             ORDER BY run_number DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![config_id], row_to_run)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_interactive_for_config(
        &self,
        config_id: &str,
    ) -> Result<Vec<AgentRun>, AgentRunRepoError> {
        let sql = format!(
            "{RUN_SELECT}
             FROM agent_runs
             WHERE agent_config_id = ?1 AND run_kind = 'interactive'
             ORDER BY run_number DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![config_id], row_to_run)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn latest_auto_run(&self, config_id: &str) -> Result<Option<AgentRun>, AgentRunRepoError> {
        let sql = format!(
            "{RUN_SELECT}
             FROM agent_runs
             WHERE agent_config_id = ?1 AND run_kind = 'auto'
             ORDER BY run_number DESC
             LIMIT 1"
        );
        self.conn
            .query_row(&sql, params![config_id], row_to_run)
            .optional()
            .map_err(Into::into)
    }

    pub fn latest_run(&self, config_id: &str) -> Result<Option<AgentRun>, AgentRunRepoError> {
        let sql = format!(
            "{RUN_SELECT}
             FROM agent_runs
             WHERE agent_config_id = ?1
             ORDER BY run_number DESC
             LIMIT 1"
        );
        self.conn
            .query_row(&sql, params![config_id], row_to_run)
            .optional()
            .map_err(Into::into)
    }

    pub fn get(&self, id: &str) -> Result<Option<AgentRun>, AgentRunRepoError> {
        let sql = format!("{RUN_SELECT} FROM agent_runs WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], row_to_run)
            .optional()
            .map_err(Into::into)
    }

    pub fn update_runtime_status(
        &self,
        id: &str,
        runtime_status: &str,
    ) -> Result<(), AgentRunRepoError> {
        let updated = self.conn.execute(
            "UPDATE agent_runs SET runtime_status = ?2 WHERE id = ?1",
            params![id, runtime_status],
        )?;
        if updated == 0 {
            return Err(AgentRunRepoError::NotFound);
        }
        Ok(())
    }

    pub fn update_reconnect(
        &self,
        id: &str,
        identity: ReconnectIdentity,
    ) -> Result<(), AgentRunRepoError> {
        let updated = self.conn.execute(
            "UPDATE agent_runs SET reconnect_pid = ?2, reconnect_birth_token = ?3 WHERE id = ?1",
            params![id, identity.pid as i64, identity.birth_token as i64],
        )?;
        if updated == 0 {
            return Err(AgentRunRepoError::NotFound);
        }
        Ok(())
    }

    pub fn clear_reconnect(&self, id: &str) -> Result<(), AgentRunRepoError> {
        let updated = self.conn.execute(
            "UPDATE agent_runs SET reconnect_pid = NULL, reconnect_birth_token = NULL WHERE id = ?1",
            params![id],
        )?;
        if updated == 0 {
            return Err(AgentRunRepoError::NotFound);
        }
        Ok(())
    }

    pub fn end_run(&self, id: &str) -> Result<(), AgentRunRepoError> {
        let now_ms: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let updated = self.conn.execute(
            "UPDATE agent_runs SET runtime_status = 'not_running', ended_at = ?2 WHERE id = ?1",
            params![id, now_ms],
        )?;
        if updated == 0 {
            return Err(AgentRunRepoError::NotFound);
        }
        Ok(())
    }

    /// Hard-delete a run and its transcript turns.
    pub fn delete_run(&self, id: &str) -> Result<(), AgentRunRepoError> {
        self.conn.execute(
            "DELETE FROM transcript_turns WHERE agent_run_id = ?1",
            params![id],
        )?;
        let deleted = self
            .conn
            .execute("DELETE FROM agent_runs WHERE id = ?1", params![id])?;
        if deleted == 0 {
            return Err(AgentRunRepoError::NotFound);
        }
        Ok(())
    }
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRun> {
    let pid: Option<i64> = row.get(6)?;
    let birth: Option<i64> = row.get(7)?;
    let reconnect = match (pid, birth) {
        (Some(pid), Some(birth)) => Some(ReconnectIdentity {
            pid: pid as u32,
            birth_token: birth as u64,
        }),
        _ => None,
    };
    Ok(AgentRun {
        id: row.get(0)?,
        agent_config_id: row.get(1)?,
        run_number: row.get(2)?,
        runtime_status: row.get(3)?,
        started_at: row.get(4)?,
        ended_at: row.get(5)?,
        reconnect,
        run_kind: row.get(8)?,
    })
}
