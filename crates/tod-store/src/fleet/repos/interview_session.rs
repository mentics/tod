//! Interview session repository (fleet DB).

use crate::outline::uuid_blob::{blob_to_uuid_sql, ms_to_datetime, now_ms, uuid_to_blob};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterviewSessionStatus {
    Active,
    Archived,
    Complete,
}

impl InterviewSessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
            Self::Complete => "complete",
        }
    }

    pub fn from_str(value: &str) -> Result<Self> {
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
    pub id: Uuid,
    pub node_id: Uuid,
    pub agent_config_id: Option<String>,
    pub display_name: String,
    pub status: InterviewSessionStatus,
    pub phase: String,
    pub session_id: Option<String>,
    pub scratchpad_path: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NewInterviewSession {
    pub node_id: Uuid,
    pub agent_config_id: Option<String>,
    pub display_name: String,
    pub phase: String,
}

pub struct InterviewSessionRepo<'a> {
    conn: &'a Connection,
}

impl<'a> InterviewSessionRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn insert_with_id(
        &self,
        id: Uuid,
        new_session: NewInterviewSession,
        status: InterviewSessionStatus,
    ) -> Result<InterviewSession> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO interview_sessions
             (id, node_id, agent_config_id, display_name, status, phase, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                uuid_to_blob(id),
                uuid_to_blob(new_session.node_id),
                new_session.agent_config_id,
                new_session.display_name,
                status.as_str(),
                new_session.phase,
                now,
            ],
        )?;
        self.get(id)?
            .ok_or_else(|| anyhow::anyhow!("inserted session not found"))
    }

    pub fn insert(
        &self,
        new_session: NewInterviewSession,
        status: InterviewSessionStatus,
    ) -> Result<InterviewSession> {
        self.insert_with_id(Uuid::new_v4(), new_session, status)
    }

    pub fn update_scaffolding(
        &self,
        id: Uuid,
        session_id: Option<&str>,
        scratchpad_path: Option<&str>,
    ) -> Result<InterviewSession> {
        let now = now_ms();
        self.conn.execute(
            "UPDATE interview_sessions
             SET session_id = COALESCE(?2, session_id),
                 scratchpad_path = COALESCE(?3, scratchpad_path),
                 updated_at = ?4
             WHERE id = ?1",
            params![uuid_to_blob(id), session_id, scratchpad_path, now],
        )?;
        self.get(id)?
            .ok_or_else(|| anyhow::anyhow!("session {id} not found"))
    }

    pub fn set_status(&self, id: Uuid, status: InterviewSessionStatus) -> Result<InterviewSession> {
        let now = now_ms();
        self.conn.execute(
            "UPDATE interview_sessions SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![uuid_to_blob(id), status.as_str(), now],
        )?;
        self.get(id)?
            .ok_or_else(|| anyhow::anyhow!("session {id} not found"))
    }

    pub fn list_all(&self) -> Result<Vec<InterviewSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, agent_config_id, display_name, status, phase, session_id, scratchpad_path, created_at, updated_at
             FROM interview_sessions ORDER BY updated_at DESC",
        )?;
        let rows = stmt
            .query_map([], row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_by_status(&self, status: InterviewSessionStatus) -> Result<Vec<InterviewSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, agent_config_id, display_name, status, phase, session_id, scratchpad_path, created_at, updated_at
             FROM interview_sessions WHERE status = ?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt
            .query_map([status.as_str()], row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get(&self, id: Uuid) -> Result<Option<InterviewSession>> {
        self.conn
            .query_row(
                "SELECT id, node_id, agent_config_id, display_name, status, phase, session_id, scratchpad_path, created_at, updated_at
                 FROM interview_sessions WHERE id = ?1",
                params![uuid_to_blob(id)],
                row_to_session,
            )
            .optional()
            .context("failed to query interview session")
    }

    pub fn list_for_node(&self, node_id: Uuid) -> Result<Vec<InterviewSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, agent_config_id, display_name, status, phase, session_id, scratchpad_path, created_at, updated_at
             FROM interview_sessions WHERE node_id = ?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], row_to_session)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<InterviewSession> {
    let id_blob: Vec<u8> = row.get(0)?;
    let node_blob: Vec<u8> = row.get(1)?;
    Ok(InterviewSession {
        id: blob_to_uuid_sql(&id_blob).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, e.into())
        })?,
        node_id: blob_to_uuid_sql(&node_blob).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Blob, e.into())
        })?,
        agent_config_id: row.get(2)?,
        display_name: row.get(3)?,
        status: InterviewSessionStatus::from_str(row.get::<_, String>(4)?.as_str()).map_err(
            |e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, e.into()),
        )?,
        phase: row.get(5)?,
        session_id: row.get(6)?,
        scratchpad_path: row.get(7)?,
        created_at: ms_to_datetime(row.get(8)?),
        updated_at: ms_to_datetime(row.get(9)?),
    })
}
