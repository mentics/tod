//! Interview reads.

use super::types::*;
use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use uuid::Uuid;

const QUESTION_COLUMNS: &str = "id, node_id, session_id, seq, phase, author, status, covers, context,
    question, intent, recommend, options, proposal, answer_option, answer_text, answer_edited_text,
    applied, processed_at, processed_summary, withdrawn_by, withdrawn_reason, created_at,
    answered_at, updated_at";

const MEMORY_COLUMNS: &str =
    "id, node_id, seq, kind, phase, status, author, question_seq, body, created_at, updated_at";

const AGENT_SESSION_COLUMNS: &str = "id, node_id, interview_session_id, phase, role, lane,
    agent_session_id, synced_rev, est_tokens, snapshot_tokens, turns, state, created_at, last_turn_at";

pub struct InterviewRepo<'a> {
    conn: &'a Connection,
}

impl<'a> InterviewRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Questions for a node, oldest first, optionally limited to `statuses`.
    pub fn list_questions(&self, node_id: Uuid, statuses: &[&str]) -> Result<Vec<InterviewQuestion>> {
        let mut sql = format!(
            "SELECT {QUESTION_COLUMNS} FROM interview_questions WHERE node_id = ?1"
        );
        if !statuses.is_empty() {
            let marks = (0..statuses.len())
                .map(|i| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(&format!(" AND status IN ({marks})"));
        }
        sql.push_str(" ORDER BY seq");
        let mut values: Vec<rusqlite::types::Value> = vec![uuid_to_blob(node_id).into()];
        values.extend(statuses.iter().map(|s| rusqlite::types::Value::from(s.to_string())));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(values), map_question)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_question(&self, node_id: Uuid, seq: i64) -> Result<Option<InterviewQuestion>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {QUESTION_COLUMNS} FROM interview_questions WHERE node_id = ?1 AND seq = ?2"
                ),
                params![uuid_to_blob(node_id), seq],
                map_question,
            )
            .optional()
            .context("query interview question")
    }

    pub fn get_question_by_id(&self, id: Uuid) -> Result<Option<InterviewQuestion>> {
        self.conn
            .query_row(
                &format!("SELECT {QUESTION_COLUMNS} FROM interview_questions WHERE id = ?1"),
                params![uuid_to_blob(id)],
                map_question,
            )
            .optional()
            .context("query interview question")
    }

    /// Answered questions not yet processed by an answer processor.
    pub fn unprocessed_answers(&self, node_id: Uuid) -> Result<Vec<InterviewQuestion>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {QUESTION_COLUMNS} FROM interview_questions
             WHERE node_id = ?1 AND status = 'answered' AND processed_at IS NULL ORDER BY seq"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_question)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Open or deferred questions whose proposal updates, deletes, or replaces
    /// an obligation that no longer exists — accepting them can no longer work.
    pub fn stale_proposal_questions(&self, node_id: Uuid) -> Result<Vec<i64>> {
        let obligations = crate::outline::repos::ObligationRepo::new(self.conn);
        let mut stale = Vec::new();
        for q in self.list_questions(node_id, &[STATUS_OPEN, STATUS_DEFERRED])? {
            let Some(proposal) = &q.proposal else {
                continue;
            };
            let mut targets = proposal.id.iter().chain(proposal.replaces.iter());
            let missing = targets.try_fold(false, |missing, raw| -> Result<bool> {
                Ok(missing
                    || match Uuid::parse_str(raw) {
                        Ok(id) => obligations.get(id)?.is_none(),
                        Err(_) => true,
                    })
            })?;
            if missing {
                stale.push(q.seq);
            }
        }
        Ok(stale)
    }

    pub fn list_memory(
        &self,
        node_id: Uuid,
        kind: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<MemoryNote>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MEMORY_COLUMNS} FROM interview_memory
             WHERE node_id = ?1 AND (?2 IS NULL OR kind = ?2) AND (?3 IS NULL OR status = ?3)
             ORDER BY seq"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id), kind, status], map_memory)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_memory(&self, node_id: Uuid, seq: i64) -> Result<Option<MemoryNote>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {MEMORY_COLUMNS} FROM interview_memory WHERE node_id = ?1 AND seq = ?2"
                ),
                params![uuid_to_blob(node_id), seq],
                map_memory,
            )
            .optional()
            .context("query interview memory")
    }

    pub fn get_memory_by_id(&self, id: Uuid) -> Result<Option<MemoryNote>> {
        self.conn
            .query_row(
                &format!("SELECT {MEMORY_COLUMNS} FROM interview_memory WHERE id = ?1"),
                params![uuid_to_blob(id)],
                map_memory,
            )
            .optional()
            .context("query interview memory")
    }

    /// Head of the change log (0 when empty).
    pub fn head_rev(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COALESCE(MAX(rev), 0) FROM interview_changes", [], |r| r.get(0))?)
    }

    /// Changes after `rev` owned by any of `node_ids`, excluding `exclude_actor`.
    pub fn changes_since(
        &self,
        node_ids: &[Uuid],
        rev: i64,
        exclude_actor: &str,
    ) -> Result<Vec<ChangeRow>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let marks = (0..node_ids.len())
            .map(|i| format!("?{}", i + 3))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT rev, node_id, entity, entity_id, op, fields, actor FROM interview_changes
             WHERE rev > ?1 AND actor != ?2 AND node_id IN ({marks}) ORDER BY rev"
        );
        let mut values: Vec<rusqlite::types::Value> = vec![rev.into(), exclude_actor.to_string().into()];
        values.extend(node_ids.iter().map(|id| rusqlite::types::Value::from(uuid_to_blob(*id))));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(values), |row| {
                let node: Vec<u8> = row.get(1)?;
                let entity_id: Vec<u8> = row.get(3)?;
                let fields: Option<String> = row.get(5)?;
                Ok(ChangeRow {
                    rev: row.get(0)?,
                    node_id: blob_to_uuid_sql(&node)?,
                    entity: row.get(2)?,
                    entity_id: blob_to_uuid_sql(&entity_id)?,
                    op: row.get(4)?,
                    fields: fields
                        .map(|f| {
                            f.split(',')
                                .filter(|s| !s.is_empty())
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                    actor: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Whether `entity_id` changed after `rev` by someone other than `actor`.
    pub fn changed_by_other_since(&self, entity_id: Uuid, rev: i64, actor: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM interview_changes WHERE entity_id = ?1 AND rev > ?2 AND actor != ?3",
            params![uuid_to_blob(entity_id), rev, actor],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn get_agent_session(&self, id: Uuid) -> Result<Option<AgentSessionRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {AGENT_SESSION_COLUMNS} FROM interview_agent_sessions WHERE id = ?1"
                ),
                params![uuid_to_blob(id)],
                map_agent_session,
            )
            .optional()
            .context("query interview agent session")
    }

    /// Live agent sessions for a node, phase, and role, lowest lane first.
    pub fn live_agent_sessions(
        &self,
        node_id: Uuid,
        phase: &str,
        role: Role,
    ) -> Result<Vec<AgentSessionRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {AGENT_SESSION_COLUMNS} FROM interview_agent_sessions
             WHERE node_id = ?1 AND phase = ?2 AND role = ?3 AND state = 'live' ORDER BY lane"
        ))?;
        let rows = stmt
            .query_map(
                params![uuid_to_blob(node_id), phase, role.as_str()],
                map_agent_session,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// `(question_maker_state, exhausted_reason)` for an interview session.
    pub fn question_maker_state(&self, session_id: Uuid) -> Result<Option<(String, Option<String>)>> {
        self.conn
            .query_row(
                "SELECT question_maker_state, exhausted_reason FROM interview_sessions WHERE id = ?1",
                params![uuid_to_blob(session_id)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("query question maker state")
    }

    /// Resolve an obligation id given in full or as a unique hex prefix
    /// (hyphens ignored), such as the 8-character ids shown to agents.
    pub fn resolve_obligation_id(&self, raw: &str) -> Result<Uuid> {
        self.resolve_id_in_table(raw, "node_obligations", "obligation")
    }

    /// Resolve a plan step id given in full or as a unique hex prefix, same
    /// convention as [`Self::resolve_obligation_id`].
    pub fn resolve_plan_step_id(&self, raw: &str) -> Result<Uuid> {
        self.resolve_id_in_table(raw, "node_plan_steps", "plan step")
    }

    fn resolve_id_in_table(&self, raw: &str, table: &str, noun: &str) -> Result<Uuid> {
        if let Ok(id) = Uuid::parse_str(raw.trim()) {
            return Ok(id);
        }
        let prefix: String = raw
            .trim()
            .chars()
            .filter(|c| *c != '-')
            .collect::<String>()
            .to_ascii_uppercase();
        if prefix.len() < 4 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            anyhow::bail!("`{raw}` is not a {noun} id");
        }
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT id FROM {table} WHERE hex(id) LIKE ?1 || '%' LIMIT 2"))?;
        let ids = stmt
            .query_map(params![prefix], |row| {
                let blob: Vec<u8> = row.get(0)?;
                blob_to_uuid_sql(&blob)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        match ids.as_slice() {
            [id] => Ok(*id),
            [] => anyhow::bail!("no {noun} matches `{raw}`"),
            _ => anyhow::bail!("`{raw}` matches more than one {noun}; use more characters"),
        }
    }
}

/// The 8-character id form shown to agents.
pub fn short_id(id: Uuid) -> String {
    id.simple().to_string()[..8].to_string()
}

fn opt_uuid(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<Uuid>> {
    let blob: Option<Vec<u8>> = row.get(idx)?;
    blob.map(|b| blob_to_uuid_sql(&b)).transpose()
}

fn json_col<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    idx: usize,
) -> rusqlite::Result<Option<T>> {
    let text: Option<String> = row.get(idx)?;
    text.map(|t| {
        serde_json::from_str(&t).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, e.into())
        })
    })
    .transpose()
}

fn map_question(row: &rusqlite::Row<'_>) -> rusqlite::Result<InterviewQuestion> {
    let id: Vec<u8> = row.get(0)?;
    let node: Vec<u8> = row.get(1)?;
    Ok(InterviewQuestion {
        id: blob_to_uuid_sql(&id)?,
        node_id: blob_to_uuid_sql(&node)?,
        session_id: opt_uuid(row, 2)?,
        seq: row.get(3)?,
        phase: row.get(4)?,
        author: row.get(5)?,
        status: row.get(6)?,
        covers: json_col(row, 7)?.unwrap_or_default(),
        context: row.get(8)?,
        question: row.get(9)?,
        intent: row.get(10)?,
        recommend: row.get(11)?,
        options: json_col(row, 12)?.unwrap_or_default(),
        proposal: json_col(row, 13)?,
        answer_option: row.get(14)?,
        answer_text: row.get(15)?,
        answer_edited_text: row.get(16)?,
        applied: json_col(row, 17)?,
        processed_at: row.get(18)?,
        processed_summary: row.get(19)?,
        withdrawn_by: row.get(20)?,
        withdrawn_reason: row.get(21)?,
        created_at: row.get(22)?,
        answered_at: row.get(23)?,
        updated_at: row.get(24)?,
    })
}

fn map_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryNote> {
    let id: Vec<u8> = row.get(0)?;
    let node: Vec<u8> = row.get(1)?;
    Ok(MemoryNote {
        id: blob_to_uuid_sql(&id)?,
        node_id: blob_to_uuid_sql(&node)?,
        seq: row.get(2)?,
        kind: row.get(3)?,
        phase: row.get(4)?,
        status: row.get(5)?,
        author: row.get(6)?,
        question_seq: row.get(7)?,
        body: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn map_agent_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentSessionRow> {
    let id: Vec<u8> = row.get(0)?;
    let node: Vec<u8> = row.get(1)?;
    let role: String = row.get(4)?;
    let state: String = row.get(11)?;
    Ok(AgentSessionRow {
        id: blob_to_uuid_sql(&id)?,
        node_id: blob_to_uuid_sql(&node)?,
        interview_session_id: opt_uuid(row, 2)?,
        phase: row.get(3)?,
        role: Role::parse(&role).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                format!("unknown role {role}").into(),
            )
        })?,
        lane: row.get(5)?,
        agent_session_id: row.get(6)?,
        synced_rev: row.get(7)?,
        est_tokens: row.get(8)?,
        snapshot_tokens: row.get(9)?,
        turns: row.get(10)?,
        live: state == "live",
        created_at: row.get(12)?,
        last_turn_at: row.get(13)?,
    })
}
