//! Every agent session tod has started, whatever started it — a conversation,
//! an interview agent, a gate check, a fleet run — recorded as soon as the
//! agent reports the session's id, so its transcript can be read from the
//! platform's own record (and kept here once read) long after it ended.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::outline::uuid_blob::now_ms;

/// A session as the agent reported it when it started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAgentSession {
    pub agent_session_id: String,
    /// `AgentPlatform` label (`claude`, `cursor`).
    pub platform: String,
    /// tod's key for the session (`conversation-<id>`, `gate-check-<id>`, a
    /// run id, …): what its live traffic is filed under.
    pub session_key: String,
    pub title: String,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSession {
    pub agent_session_id: String,
    /// `None` for sessions recorded before tod kept the platform (interview
    /// agents): the reader tries each platform.
    pub platform: Option<String>,
    pub session_key: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub started_at: i64,
    pub cached_transcript: Option<String>,
    pub transcript_fingerprint: Option<String>,
}

pub struct AgentSessionRepo<'a> {
    conn: &'a Connection,
}

const SELECT: &str = "SELECT agent_session_id, platform, session_key, title, cwd, started_at,
                             cached_transcript, transcript_fingerprint
                      FROM agent_sessions";

impl<'a> AgentSessionRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Record a session. A resumed session is reported again: it keeps when
    /// it started and what is already stored, and takes the newer details.
    pub fn record(&self, session: &NewAgentSession) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO agent_sessions
                 (agent_session_id, platform, session_key, title, cwd, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(agent_session_id) DO UPDATE SET
                 platform = excluded.platform,
                 session_key = excluded.session_key,
                 title = COALESCE(NULLIF(excluded.title, ''), title),
                 cwd = excluded.cwd",
            params![
                session.agent_session_id,
                session.platform,
                session.session_key,
                session.title,
                session.cwd,
                now_ms(),
            ],
        )?;
        Ok(())
    }

    /// Every session, newest first.
    pub fn list(&self) -> anyhow::Result<Vec<AgentSession>> {
        let mut stmt = self.conn.prepare(&format!(
            "{SELECT} ORDER BY started_at DESC, agent_session_id"
        ))?;
        let rows = stmt.query_map([], row_to_session)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every session, newest first, without its stored transcript (which
    /// can be megabytes): for listing them. Read one with [`Self::get`].
    pub fn list_without_transcripts(&self) -> anyhow::Result<Vec<AgentSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT agent_session_id, platform, session_key, title, cwd, started_at,
                    NULL, transcript_fingerprint
             FROM agent_sessions ORDER BY started_at DESC, agent_session_id",
        )?;
        let rows = stmt.query_map([], row_to_session)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get(&self, agent_session_id: &str) -> anyhow::Result<Option<AgentSession>> {
        Ok(self
            .conn
            .query_row(
                &format!("{SELECT} WHERE agent_session_id = ?1"),
                [agent_session_id],
                row_to_session,
            )
            .optional()?)
    }

    pub fn cache_transcript(
        &self,
        agent_session_id: &str,
        platform: &str,
        transcript: &str,
        fingerprint: Option<&str>,
    ) -> anyhow::Result<()> {
        let updated = self.conn.execute(
            "UPDATE agent_sessions
             SET cached_transcript = ?2, transcript_fingerprint = ?3,
                 platform = COALESCE(platform, ?4)
             WHERE agent_session_id = ?1",
            params![agent_session_id, transcript, fingerprint, platform],
        )?;
        anyhow::ensure!(updated == 1, "agent session {agent_session_id} not found");
        Ok(())
    }
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentSession> {
    Ok(AgentSession {
        agent_session_id: row.get(0)?,
        platform: row.get(1)?,
        session_key: row.get(2)?,
        title: row.get(3)?,
        cwd: row.get(4)?,
        started_at: row.get(5)?,
        cached_transcript: row.get(6)?,
        transcript_fingerprint: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::test_writer_conn;

    fn session(id: &str, title: &str) -> NewAgentSession {
        NewAgentSession {
            agent_session_id: id.into(),
            platform: "claude".into(),
            session_key: format!("key-{id}"),
            title: title.into(),
            cwd: "/work".into(),
        }
    }

    #[test]
    fn a_resumed_session_keeps_its_start_and_stored_transcript() {
        let (dir, conn) = test_writer_conn();
        let repo = AgentSessionRepo::new(&conn);
        repo.record(&session("s1", "First")).unwrap();
        let started = repo.get("s1").unwrap().unwrap().started_at;
        repo.cache_transcript("s1", "claude", "stored", Some("fp"))
            .unwrap();

        repo.record(&session("s1", "")).unwrap();
        let s1 = repo.get("s1").unwrap().unwrap();
        assert_eq!(s1.started_at, started);
        assert_eq!(s1.title.as_deref(), Some("First"));
        assert_eq!(s1.cached_transcript.as_deref(), Some("stored"));
        assert_eq!(s1.transcript_fingerprint.as_deref(), Some("fp"));
        assert_eq!(repo.list().unwrap().len(), 1);
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }
}
