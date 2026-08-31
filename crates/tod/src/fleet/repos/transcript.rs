//! Transcript repository — per agent-run monotonic turns with prompt/response pairing.

use crate::fleet::repos::agent_run::AgentRunRepo;
use rusqlite::{Connection, params};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptTurn {
    pub id: String,
    /// Individual agent run this turn belongs to.
    pub agent_run_id: String,
    pub sequence: i64,
    pub kind: String,
    pub prompt_status: Option<String>,
    pub content: String,
    pub originating_prompt_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum TranscriptRepoError {
    #[error("transcript turn not found")]
    NotFound,
    #[error(transparent)]
    Run(#[from] crate::fleet::repos::agent_run::AgentRunRepoError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct TranscriptRepo<'a> {
    conn: &'a Connection,
}

impl<'a> TranscriptRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn next_sequence(&self, agent_run_id: &str) -> Result<i64, TranscriptRepoError> {
        let sequence: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM transcript_turns WHERE agent_run_id = ?1",
            params![agent_run_id],
            |row| row.get(0),
        )?;
        Ok(sequence)
    }

    pub fn insert_prompt(
        &self,
        id: &str,
        agent_run_id: &str,
        content: &str,
    ) -> Result<i64, TranscriptRepoError> {
        let sequence = self.next_sequence(agent_run_id)?;
        self.conn.execute(
            "INSERT INTO transcript_turns (id, agent_run_id, sequence, kind, prompt_status, content)
             VALUES (?1, ?2, ?3, 'prompt', 'incomplete', ?4)",
            params![id, agent_run_id, sequence, content],
        )?;
        Ok(sequence)
    }

    pub fn send_prompt(
        &self,
        id: &str,
        agent_run_id: &str,
        content: &str,
    ) -> Result<i64, TranscriptRepoError> {
        let sequence = self.insert_prompt(id, agent_run_id, content)?;
        AgentRunRepo::new(self.conn).update_runtime_status(agent_run_id, "processing")?;
        Ok(sequence)
    }

    pub fn insert_response(
        &self,
        id: &str,
        agent_run_id: &str,
        content: &str,
        originating_prompt_id: &str,
    ) -> Result<i64, TranscriptRepoError> {
        let sequence = self.next_sequence(agent_run_id)?;
        self.conn.execute(
            "INSERT INTO transcript_turns (
                id, agent_run_id, sequence, kind, prompt_status, content, originating_prompt_id
             ) VALUES (?1, ?2, ?3, 'response', NULL, ?4, ?5)",
            params![id, agent_run_id, sequence, content, originating_prompt_id],
        )?;
        Ok(sequence)
    }

    pub fn complete_response(
        &self,
        response_id: &str,
        agent_run_id: &str,
        content: &str,
        prompt_id: &str,
    ) -> Result<(), TranscriptRepoError> {
        self.insert_response(response_id, agent_run_id, content, prompt_id)?;
        AgentRunRepo::new(self.conn).update_runtime_status(agent_run_id, "waiting")?;
        let updated = self.conn.execute(
            "UPDATE transcript_turns SET prompt_status = 'complete' WHERE id = ?1",
            params![prompt_id],
        )?;
        if updated == 0 {
            return Err(TranscriptRepoError::NotFound);
        }
        Ok(())
    }

    pub fn mark_incomplete_prompts_interrupted(
        &self,
        agent_run_id: &str,
    ) -> Result<usize, TranscriptRepoError> {
        let updated = self.conn.execute(
            "UPDATE transcript_turns
             SET prompt_status = 'interrupted'
             WHERE agent_run_id = ?1 AND kind = 'prompt' AND prompt_status = 'incomplete'",
            params![agent_run_id],
        )?;
        Ok(updated)
    }

    pub fn list_for_agent_run(
        &self,
        agent_run_id: &str,
    ) -> Result<Vec<TranscriptTurn>, TranscriptRepoError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent_run_id, sequence, kind, prompt_status, content, originating_prompt_id
             FROM transcript_turns WHERE agent_run_id = ?1 ORDER BY sequence",
        )?;
        let rows = stmt
            .query_map(params![agent_run_id], row_to_turn)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// List turns for all runs of an agent config (historical aggregate).
    pub fn list_for_config(
        &self,
        agent_config_id: &str,
    ) -> Result<Vec<TranscriptTurn>, TranscriptRepoError> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.agent_run_id, t.sequence, t.kind, t.prompt_status, t.content, t.originating_prompt_id
             FROM transcript_turns t
             INNER JOIN agent_runs r ON t.agent_run_id = r.id
             WHERE r.agent_config_id = ?1
             ORDER BY t.sequence",
        )?;
        let rows = stmt
            .query_map(params![agent_config_id], row_to_turn)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Back-compat alias.
    pub fn list_for_agent(
        &self,
        agent_run_id: &str,
    ) -> Result<Vec<TranscriptTurn>, TranscriptRepoError> {
        self.list_for_agent_run(agent_run_id)
    }
}

fn row_to_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<TranscriptTurn> {
    Ok(TranscriptTurn {
        id: row.get(0)?,
        agent_run_id: row.get(1)?,
        sequence: row.get(2)?,
        kind: row.get(3)?,
        prompt_status: row.get(4)?,
        content: row.get(5)?,
        originating_prompt_id: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::agent_config::{AgentConfigRepo, NewAgentConfig};
    use crate::fleet::repos::agent_run::AgentRunRepo;
    use crate::fleet::repos::task::{FleetTask, TaskRepo};
    use crate::fleet::repos::{cleanup_test_dir, test_writer_conn};

    fn seed_run(conn: &Connection) -> String {
        let task_id = uuid::Uuid::new_v4().to_string();
        let config_id = uuid::Uuid::new_v4().to_string();
        TaskRepo::new(conn)
            .insert(&FleetTask::new(&task_id, "T", "t"))
            .unwrap();
        AgentConfigRepo::new(conn)
            .insert(&NewAgentConfig {
                id: config_id.clone(),
                node_id: task_id,
                env_type: "local".into(),
                mode: "agent".into(),
                work_directory: None,
                use_worktree: false,
            })
            .unwrap();
        AgentRunRepo::new(conn)
            .create_run(&config_id, "waiting", "auto")
            .unwrap()
    }

    #[test]
    fn monotonic_sequence_ordering() {
        let (dir, conn) = test_writer_conn();
        let run_id = seed_run(&conn);
        let repo = TranscriptRepo::new(&conn);
        let p1 = uuid::Uuid::new_v4().to_string();
        let p2 = uuid::Uuid::new_v4().to_string();
        assert_eq!(repo.insert_prompt(&p1, &run_id, "one").unwrap(), 1);
        assert_eq!(repo.insert_prompt(&p2, &run_id, "two").unwrap(), 2);

        let turns = repo.list_for_agent_run(&run_id).unwrap();
        assert_eq!(turns.len(), 2);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn send_prompt_sets_processing() {
        let (dir, conn) = test_writer_conn();
        let run_id = seed_run(&conn);
        let prompt_id = uuid::Uuid::new_v4().to_string();
        TranscriptRepo::new(&conn)
            .send_prompt(&prompt_id, &run_id, "go")
            .unwrap();
        let status: String = conn
            .query_row(
                "SELECT runtime_status FROM agent_runs WHERE id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "processing");
        cleanup_test_dir(&dir);
    }
}
