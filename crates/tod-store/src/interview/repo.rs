//! Obligation history and identifier resolution.

use super::types::*;
use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

const SNAPSHOT_COLUMNS: &str = "rev, node_id, entity_id, op, actor, at, prior";

/// A change-log row that kept an obligation's prior row.
const IS_SNAPSHOT: &str =
    "entity = 'obligation' AND op IN ('delete', 'update') AND prior IS NOT NULL";

pub struct InterviewRepo<'a> {
    conn: &'a Connection,
}

impl<'a> InterviewRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// The obligation row a change-log entry kept, if `rev` is an obligation
    /// delete or edit still within retention.
    pub fn obligation_snapshot(&self, rev: i64) -> Result<Option<ObligationSnapshot>> {
        self.conn
            .query_row(
                &format!("SELECT {SNAPSHOT_COLUMNS} FROM interview_changes WHERE rev = ?1 AND {IS_SNAPSHOT}"),
                params![rev],
                map_snapshot,
            )
            .optional()
            .context("query obligation snapshot")
    }

    /// Obligations deleted from `node_id` that can still be restored — each
    /// one's latest deletion, for obligations that exist nowhere now — newest
    /// first. Restoring them in this order puts them back where they were.
    pub fn deleted_obligations(&self, node_id: Uuid) -> Result<Vec<ObligationSnapshot>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SNAPSHOT_COLUMNS} FROM interview_changes c
             WHERE c.node_id = ?1 AND c.op = 'delete' AND {IS_SNAPSHOT}
               AND NOT EXISTS (SELECT 1 FROM node_obligations o WHERE o.id = c.entity_id)
               AND c.rev = (SELECT MAX(d.rev) FROM interview_changes d
                            WHERE d.entity_id = c.entity_id AND d.op = 'delete' AND d.prior IS NOT NULL)
             ORDER BY c.rev DESC"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_snapshot)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every kept earlier version of one obligation (edits and deletions),
    /// newest first.
    pub fn obligation_history(&self, obligation_id: Uuid) -> Result<Vec<ObligationSnapshot>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SNAPSHOT_COLUMNS} FROM interview_changes
             WHERE entity_id = ?1 AND {IS_SNAPSHOT} ORDER BY rev DESC"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(obligation_id)], map_snapshot)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Resolve an obligation id given in full or as a unique hex prefix
    /// (hyphens ignored), such as the 8-character ids shown to agents.
    pub fn resolve_obligation_id(&self, raw: &str) -> Result<Uuid> {
        self.resolve_id_in_table(raw, "node_obligations", "obligation")
    }

    /// Like [`Self::resolve_obligation_id`], but a prefix may also name an
    /// obligation that is gone and only survives in the change log.
    pub fn resolve_obligation_id_with_history(&self, raw: &str) -> Result<Uuid> {
        self.resolve_id_in_table(raw, "node_obligations", "obligation")
            .or_else(|_| {
                self.resolve_id_in_query(
                    raw,
                    "SELECT DISTINCT entity_id FROM interview_changes
                     WHERE entity = 'obligation' AND prior IS NOT NULL AND hex(entity_id) LIKE ?1 || '%' LIMIT 2",
                    "obligation",
                )
            })
    }

    /// Resolve a plan step id given in full or as a unique hex prefix, same
    /// convention as [`Self::resolve_obligation_id`].
    pub fn resolve_plan_step_id(&self, raw: &str) -> Result<Uuid> {
        self.resolve_id_in_table(raw, "node_plan_steps", "plan step")
    }

    fn resolve_id_in_table(&self, raw: &str, table: &str, noun: &str) -> Result<Uuid> {
        self.resolve_id_in_query(
            raw,
            &format!("SELECT id FROM {table} WHERE hex(id) LIKE ?1 || '%' LIMIT 2"),
            noun,
        )
    }

    /// `sql` selects matching ids for a hex prefix bound as `?1`.
    fn resolve_id_in_query(&self, raw: &str, sql: &str, noun: &str) -> Result<Uuid> {
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
        let mut stmt = self.conn.prepare(sql)?;
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

fn map_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<ObligationSnapshot> {
    let node: Vec<u8> = row.get(1)?;
    let entity_id: Vec<u8> = row.get(2)?;
    Ok(ObligationSnapshot {
        rev: row.get(0)?,
        node_id: blob_to_uuid_sql(&node)?,
        obligation_id: blob_to_uuid_sql(&entity_id)?,
        op: row.get(3)?,
        actor: row.get(4)?,
        at: row.get(5)?,
        prior: json_col(row, 6)?.ok_or(rusqlite::Error::InvalidColumnType(
            6,
            "prior".into(),
            rusqlite::types::Type::Null,
        ))?,
    })
}
