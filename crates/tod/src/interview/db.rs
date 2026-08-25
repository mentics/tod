use crate::interview::paths::TodPaths;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

const SCHEMA_VERSION: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterviewSessionStatus {
    Active,
    Archived,
    Complete,
}

impl InterviewSessionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
            Self::Complete => "complete",
        }
    }

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "archived" => Ok(Self::Archived),
            "complete" => Ok(Self::Complete),
            other => anyhow::bail!("unknown interview session status: {other}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterviewSession {
    pub id: i64,
    pub display_name: String,
    pub status: InterviewSessionStatus,
    pub entity_path: Option<String>,
    pub phase: Option<String>,
    pub session_id: Option<String>,
    pub scratchpad_path: Option<String>,
    pub transcript_path: Option<String>,
    pub config_path: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewInterviewSession {
    pub display_name: String,
    pub entity_path: String,
    pub phase: String,
}

pub struct SessionStore {
    conn: Connection,
}

impl SessionStore {
    pub fn open(paths: &TodPaths) -> Result<Self> {
        paths.ensure_config_dir()?;
        Self::open_at(&paths.sqlite_path())
    }

    pub fn open_at(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create SQLite parent dir {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open SQLite database at {}", path.display()))?;
        // Background kickoff sync opens a second connection while the UI holds one.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn insert_session(
        &self,
        display_name: &str,
        status: InterviewSessionStatus,
    ) -> Result<InterviewSession> {
        self.insert_session_with_metadata(NewInterviewSession {
            display_name: display_name.to_string(),
            entity_path: String::new(),
            phase: String::new(),
        }, status)
    }

    pub fn insert_session_with_metadata(
        &self,
        new_session: NewInterviewSession,
        status: InterviewSessionStatus,
    ) -> Result<InterviewSession> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO interview_sessions
             (display_name, status, entity_path, phase, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                new_session.display_name,
                status.as_str(),
                new_session.entity_path,
                new_session.phase,
                now,
                now
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.get_session(id)?
            .ok_or_else(|| anyhow::anyhow!("inserted session {id} not found"))
    }

    pub fn update_session_scaffolding(
        &self,
        id: i64,
        session_id: Option<&str>,
        scratchpad_path: Option<&str>,
        transcript_path: Option<&str>,
        config_path: Option<&str>,
    ) -> Result<InterviewSession> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE interview_sessions
             SET session_id = COALESCE(?2, session_id),
                 scratchpad_path = COALESCE(?3, scratchpad_path),
                 transcript_path = COALESCE(?4, transcript_path),
                 config_path = COALESCE(?5, config_path),
                 updated_at = ?6
             WHERE id = ?1",
            params![id, session_id, scratchpad_path, transcript_path, config_path, now],
        )?;
        self.get_session(id)?
            .ok_or_else(|| anyhow::anyhow!("session {id} not found"))
    }

    pub fn update_session_paths(
        &self,
        id: i64,
        scratchpad_path: Option<&str>,
        transcript_path: Option<&str>,
        config_path: Option<&str>,
    ) -> Result<InterviewSession> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE interview_sessions
             SET scratchpad_path = COALESCE(?2, scratchpad_path),
                 transcript_path = COALESCE(?3, transcript_path),
                 config_path = COALESCE(?4, config_path),
                 updated_at = ?5
             WHERE id = ?1",
            params![id, scratchpad_path, transcript_path, config_path, now],
        )?;
        self.get_session(id)?
            .ok_or_else(|| anyhow::anyhow!("session {id} not found"))
    }

    pub fn set_status(&self, id: i64, status: InterviewSessionStatus) -> Result<InterviewSession> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE interview_sessions SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, status.as_str(), now],
        )?;
        self.get_session(id)?
            .ok_or_else(|| anyhow::anyhow!("session {id} not found"))
    }

    pub fn list_sessions(&self) -> Result<Vec<InterviewSession>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, display_name, status, entity_path, phase, session_id,
                        scratchpad_path, transcript_path, config_path,
                        created_at, updated_at
                 FROM interview_sessions
                 ORDER BY updated_at DESC",
            )?;
        let rows = stmt
            .query_map([], row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_by_status(&self, status: InterviewSessionStatus) -> Result<Vec<InterviewSession>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, display_name, status, entity_path, phase, session_id,
                        scratchpad_path, transcript_path, config_path,
                        created_at, updated_at
                 FROM interview_sessions
                 WHERE status = ?1
                 ORDER BY updated_at DESC",
            )?;
        let rows = stmt
            .query_map([status.as_str()], row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_session(&self, id: i64) -> Result<Option<InterviewSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, display_name, status, entity_path, phase, session_id,
                    scratchpad_path, transcript_path, config_path,
                    created_at, updated_at
             FROM interview_sessions
             WHERE id = ?1",
        )?;
        stmt.query_row([id], row_to_session)
            .optional()
            .context("failed to query interview session")
    }
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<InterviewSession> {
    let created_at: String = row.get(9)?;
    let updated_at: String = row.get(10)?;
    Ok(InterviewSession {
        id: row.get(0)?,
        display_name: row.get(1)?,
        status: match row.get::<_, String>(2)?.as_str() {
            "active" => InterviewSessionStatus::Active,
            "archived" => InterviewSessionStatus::Archived,
            "complete" => InterviewSessionStatus::Complete,
            other => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    format!("unknown interview session status: {other}").into(),
                ))
            }
        },
        entity_path: row.get(3)?,
        phase: row.get(4)?,
        session_id: row.get(5)?,
        scratchpad_path: row.get(6)?,
        transcript_path: row.get(7)?,
        config_path: row.get(8)?,
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e)))?,
        updated_at: DateTime::parse_from_rfc3339(&updated_at)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(e)))?,
    })
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY
        );",
    )?;

    let current: Option<i64> = conn
        .query_row(
            "SELECT version FROM schema_migrations ORDER BY version DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;

    if current.unwrap_or(0) < SCHEMA_VERSION {
        if current.unwrap_or(0) < 1 {
            apply_v1_schema(conn)?;
        }
        if current.unwrap_or(0) < 2 {
            apply_v2_schema(conn)?;
        }
        conn.execute(
            "INSERT OR REPLACE INTO schema_migrations (version) VALUES (?1)",
            [SCHEMA_VERSION],
        )?;
    }

    Ok(())
}

fn apply_v2_schema(conn: &Connection) -> Result<()> {
    let mut columns = conn.prepare("PRAGMA table_info(interview_sessions)")?;
    let names: Vec<String> = columns
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !names.iter().any(|n| n == "entity_path") {
        conn.execute("ALTER TABLE interview_sessions ADD COLUMN entity_path TEXT", [])?;
    }
    if !names.iter().any(|n| n == "phase") {
        conn.execute("ALTER TABLE interview_sessions ADD COLUMN phase TEXT", [])?;
    }
    if !names.iter().any(|n| n == "session_id") {
        conn.execute("ALTER TABLE interview_sessions ADD COLUMN session_id TEXT", [])?;
    }
    Ok(())
}

fn apply_v1_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS interview_sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            display_name TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('active', 'archived', 'complete')),
            scratchpad_path TEXT,
            transcript_path TEXT,
            config_path TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::paths::TodPaths;
    use std::fs;

    #[test]
    fn session_crud_and_migrations() {
        let dir = std::env::temp_dir().join(format!("tod-db-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let paths = TodPaths::from_repo_root(dir.clone());
        let store = SessionStore::open(&paths).unwrap();

        let session = store
            .insert_session("Design interview", InterviewSessionStatus::Active)
            .unwrap();
        assert_eq!(session.display_name, "Design interview");
        assert_eq!(session.status, InterviewSessionStatus::Active);

        let updated = store
            .update_session_scaffolding(
                session.id,
                Some("design-interview-2026-08-23-1330"),
                Some("/scratchpad"),
                Some("/history/transcript.md"),
                Some("/config.md"),
            )
            .unwrap();
        assert_eq!(updated.scratchpad_path.as_deref(), Some("/scratchpad"));

        let archived = store
            .set_status(session.id, InterviewSessionStatus::Archived)
            .unwrap();
        assert_eq!(archived.status, InterviewSessionStatus::Archived);

        let active = store.list_by_status(InterviewSessionStatus::Active).unwrap();
        assert!(active.is_empty());

        let _ = fs::remove_dir_all(dir);
    }
}
