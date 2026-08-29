//! Transcript repository — per-agent monotonic turns with prompt/response pairing.

use anyhow::Result;
use rusqlite::{Connection, params};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptTurn {
    pub id: String,
    pub agent_id: String,
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

    fn next_sequence(&self, agent_id: &str) -> Result<i64, TranscriptRepoError> {
        let sequence: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM transcript_turns WHERE agent_id = ?1",
            params![agent_id],
            |row| row.get(0),
        )?;
        Ok(sequence)
    }

    /// Insert prompt turn with **incomplete** status; returns allocated sequence.
    pub fn insert_prompt(
        &self,
        id: &str,
        agent_id: &str,
        content: &str,
    ) -> Result<i64, TranscriptRepoError> {
        let sequence = self.next_sequence(agent_id)?;
        self.conn.execute(
            "INSERT INTO transcript_turns (id, agent_id, sequence, kind, prompt_status, content)
             VALUES (?1, ?2, ?3, 'prompt', 'incomplete', ?4)",
            params![id, agent_id, sequence, content],
        )?;
        Ok(sequence)
    }

    /// Paired: sent prompt + agent **processing** status.
    pub fn send_prompt(
        &self,
        id: &str,
        agent_id: &str,
        content: &str,
    ) -> Result<i64, TranscriptRepoError> {
        let sequence = self.insert_prompt(id, agent_id, content)?;
        self.conn.execute(
            "UPDATE agents SET runtime_status = 'processing' WHERE id = ?1",
            params![agent_id],
        )?;
        Ok(sequence)
    }

    /// Insert response linked to originating prompt.
    pub fn insert_response(
        &self,
        id: &str,
        agent_id: &str,
        content: &str,
        originating_prompt_id: &str,
    ) -> Result<i64, TranscriptRepoError> {
        let sequence = self.next_sequence(agent_id)?;
        self.conn.execute(
            "INSERT INTO transcript_turns (
                id, agent_id, sequence, kind, prompt_status, content, originating_prompt_id
             ) VALUES (?1, ?2, ?3, 'response', NULL, ?4, ?5)",
            params![id, agent_id, sequence, content, originating_prompt_id],
        )?;
        Ok(sequence)
    }

    /// Paired: completed response + agent **waiting** + prompt **complete**.
    pub fn complete_response(
        &self,
        response_id: &str,
        agent_id: &str,
        content: &str,
        prompt_id: &str,
    ) -> Result<(), TranscriptRepoError> {
        self.insert_response(response_id, agent_id, content, prompt_id)?;
        self.conn.execute(
            "UPDATE agents SET runtime_status = 'waiting' WHERE id = ?1",
            params![agent_id],
        )?;
        let updated = self.conn.execute(
            "UPDATE transcript_turns SET prompt_status = 'complete' WHERE id = ?1",
            params![prompt_id],
        )?;
        if updated == 0 {
            return Err(TranscriptRepoError::NotFound);
        }
        Ok(())
    }

    /// Mark all incomplete prompts for an agent as **interrupted** (relaunch path).
    pub fn mark_incomplete_prompts_interrupted(
        &self,
        agent_id: &str,
    ) -> Result<usize, TranscriptRepoError> {
        let updated = self.conn.execute(
            "UPDATE transcript_turns
             SET prompt_status = 'interrupted'
             WHERE agent_id = ?1 AND kind = 'prompt' AND prompt_status = 'incomplete'",
            params![agent_id],
        )?;
        Ok(updated)
    }

    pub fn list_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<TranscriptTurn>, TranscriptRepoError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent_id, sequence, kind, prompt_status, content, originating_prompt_id
             FROM transcript_turns WHERE agent_id = ?1 ORDER BY sequence",
        )?;
        let rows = stmt
            .query_map(params![agent_id], row_to_turn)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn row_to_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<TranscriptTurn> {
    Ok(TranscriptTurn {
        id: row.get(0)?,
        agent_id: row.get(1)?,
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
    use crate::fleet::repos::agent::{AgentRepo, NewAgent};
    use crate::fleet::repos::task::{FleetTask, TaskRepo};
    use crate::fleet::repos::{cleanup_test_dir, test_writer_conn};

    fn seed_agent(conn: &Connection) -> String {
        let task_id = uuid::Uuid::new_v4().to_string();
        let agent_id = uuid::Uuid::new_v4().to_string();
        TaskRepo::new(conn)
            .insert(&FleetTask::new(&task_id, "T", "t"))
            .unwrap();
        AgentRepo::new(conn)
            .insert(&NewAgent {
                id: agent_id.clone(),
                task_id,
                env_type: "local".into(),
                mode: "agent".into(),
            })
            .unwrap();
        agent_id
    }

    #[test]
    fn monotonic_sequence_ordering() {
        let (dir, conn) = test_writer_conn();
        let agent_id = seed_agent(&conn);
        let repo = TranscriptRepo::new(&conn);
        let p1 = uuid::Uuid::new_v4().to_string();
        let p2 = uuid::Uuid::new_v4().to_string();
        assert_eq!(repo.insert_prompt(&p1, &agent_id, "one").unwrap(), 1);
        assert_eq!(repo.insert_prompt(&p2, &agent_id, "two").unwrap(), 2);

        let turns = repo.list_for_agent(&agent_id).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].sequence, 1);
        assert_eq!(turns[1].sequence, 2);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn send_prompt_sets_processing() {
        let (dir, conn) = test_writer_conn();
        let agent_id = seed_agent(&conn);
        let prompt_id = uuid::Uuid::new_v4().to_string();
        TranscriptRepo::new(&conn)
            .send_prompt(&prompt_id, &agent_id, "go")
            .unwrap();
        let status: String = conn
            .query_row(
                "SELECT runtime_status FROM agents WHERE id = ?1",
                params![agent_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "processing");
        cleanup_test_dir(&dir);
    }

    #[test]
    fn mark_incomplete_interrupted_on_relaunch() {
        let (dir, conn) = test_writer_conn();
        let agent_id = seed_agent(&conn);
        let repo = TranscriptRepo::new(&conn);
        let incomplete = uuid::Uuid::new_v4().to_string();
        let complete_prompt = uuid::Uuid::new_v4().to_string();
        let response = uuid::Uuid::new_v4().to_string();
        repo.insert_prompt(&incomplete, &agent_id, "pending").unwrap();
        repo.send_prompt(&complete_prompt, &agent_id, "done").unwrap();
        repo.complete_response(&response, &agent_id, "ok", &complete_prompt)
            .unwrap();

        let marked = repo.mark_incomplete_prompts_interrupted(&agent_id).unwrap();
        assert_eq!(marked, 1);

        let turns = repo.list_for_agent(&agent_id).unwrap();
        let incomplete_turn = turns.iter().find(|t| t.id == incomplete).unwrap();
        assert_eq!(incomplete_turn.prompt_status.as_deref(), Some("interrupted"));
        let complete_turn = turns.iter().find(|t| t.id == complete_prompt).unwrap();
        assert_eq!(complete_turn.prompt_status.as_deref(), Some("complete"));
        cleanup_test_dir(&dir);
    }
}
