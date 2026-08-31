//! Obligations repository.

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use uuid::Uuid;

pub const KIND_REQUIREMENT: &str = "requirement";
pub const KIND_CONSTRAINT: &str = "constraint";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObligationCounts {
    pub requirements: usize,
    pub constraints: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeObligation {
    pub id: Uuid,
    pub node_id: Uuid,
    pub kind: String,
    pub ordinal: i32,
    pub section: Option<String>,
    pub body: String,
}

pub struct ObligationRepo<'a> {
    conn: &'a Connection,
}

impl<'a> ObligationRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn insert(&self, row: &NodeObligation) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO node_obligations (id, node_id, kind, ordinal, section, body, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                uuid_to_blob(row.id),
                uuid_to_blob(row.node_id),
                row.kind,
                row.ordinal,
                row.section,
                row.body,
                now
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, id: Uuid) -> Result<Option<NodeObligation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, kind, ordinal, section, body FROM node_obligations WHERE id = ?1",
        )?;
        let row = stmt
            .query_row(params![uuid_to_blob(id)], map_obligation)
            .optional()?;
        Ok(row)
    }

    pub fn list_for_node(&self, node_id: Uuid) -> Result<Vec<NodeObligation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, kind, ordinal, section, body FROM node_obligations
             WHERE node_id = ?1 ORDER BY kind, ordinal",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_obligation)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_ids_for_kind(&self, node_id: Uuid, kind: &str) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM node_obligations WHERE node_id = ?1 AND kind = ?2 ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id), kind], |row| {
                let blob: Vec<u8> = row.get(0)?;
                blob_to_uuid_sql(&blob)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn update_body(&self, id: Uuid, body: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE node_obligations SET body = ?1, updated_at = ?2 WHERE id = ?3",
            params![body, now_ms(), uuid_to_blob(id)],
        )?;
        if n == 0 {
            anyhow::bail!("obligation not found");
        }
        Ok(())
    }

    pub fn delete(&self, id: Uuid) -> Result<Option<NodeObligation>> {
        let Some(row) = self.get(id)? else {
            return Ok(None);
        };
        self.conn.execute(
            "DELETE FROM node_obligations WHERE id = ?1",
            params![uuid_to_blob(id)],
        )?;
        self.rewrite_ordinals(row.node_id, &row.kind)?;
        Ok(Some(row))
    }

    /// Insert `id` into `kind` at `index` (0-based), shifting later items.
    pub fn insert_at(
        &self,
        id: Uuid,
        node_id: Uuid,
        kind: &str,
        index: usize,
        body: &str,
    ) -> Result<()> {
        let mut ids = self.list_ids_for_kind(node_id, kind)?;
        let index = index.min(ids.len());
        let temp_ordinal = 10_000 + ids.len() as i32;
        self.insert(&NodeObligation {
            id,
            node_id,
            kind: kind.to_string(),
            ordinal: temp_ordinal,
            section: None,
            body: body.to_string(),
        })?;
        ids.insert(index, id);
        self.write_ordinals(node_id, kind, &ids)?;
        Ok(())
    }

    pub fn reorder(&self, id: Uuid, delta: i32) -> Result<()> {
        let row = self.get(id)?.context("obligation not found")?;
        let mut ids = self.list_ids_for_kind(row.node_id, &row.kind)?;
        let Some(pos) = ids.iter().position(|item| *item == id) else {
            return Ok(());
        };
        let new_pos = pos as i32 + delta;
        if new_pos < 0 || new_pos as usize >= ids.len() {
            return Ok(());
        }
        ids.swap(pos, new_pos as usize);
        self.write_ordinals(row.node_id, &row.kind, &ids)?;
        Ok(())
    }

    pub fn counts_for_list(&self, list_id: Uuid) -> Result<HashMap<Uuid, ObligationCounts>> {
        let mut stmt = self.conn.prepare(
            "SELECT o.node_id, o.kind, COUNT(*)
             FROM node_obligations o
             INNER JOIN outline_entries e ON e.node_id = o.node_id
             WHERE e.list_id = ?1
             GROUP BY o.node_id, o.kind",
        )?;
        let mut out: HashMap<Uuid, ObligationCounts> = HashMap::new();
        let rows = stmt.query_map(params![uuid_to_blob(list_id)], |row| {
            let blob: Vec<u8> = row.get(0)?;
            Ok((
                blob_to_uuid_sql(&blob)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? as usize,
            ))
        })?;
        for row in rows {
            let (node_id, kind, count) = row?;
            let entry = out.entry(node_id).or_default();
            match kind.as_str() {
                KIND_REQUIREMENT => entry.requirements = count,
                KIND_CONSTRAINT => entry.constraints = count,
                _ => {}
            }
        }
        Ok(out)
    }

    pub fn list_global_adopted(&self) -> Result<Vec<NodeObligation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, slug, kind, ordinal, body FROM global_obligations
             WHERE adopted = 1 ORDER BY slug, kind, ordinal",
        )?;
        let rows = stmt
            .query_map([], |row| {
                let id_blob: Vec<u8> = row.get(0)?;
                Ok(NodeObligation {
                    id: blob_to_uuid_sql(&id_blob)?,
                    node_id: Uuid::nil(),
                    kind: row.get(2)?,
                    ordinal: row.get(3)?,
                    section: Some(row.get::<_, String>(1)?),
                    body: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn rewrite_ordinals(&self, node_id: Uuid, kind: &str) -> Result<()> {
        let ids = self.list_ids_for_kind(node_id, kind)?;
        self.write_ordinals(node_id, kind, &ids)
    }

    fn write_ordinals(&self, node_id: Uuid, kind: &str, ids: &[Uuid]) -> Result<()> {
        let now = now_ms();
        for (i, id) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE node_obligations SET ordinal = ?1, updated_at = ?2
                 WHERE id = ?3 AND node_id = ?4 AND kind = ?5",
                params![
                    -(i as i32 + 1),
                    now,
                    uuid_to_blob(*id),
                    uuid_to_blob(node_id),
                    kind
                ],
            )?;
        }
        for (i, id) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE node_obligations SET ordinal = ?1 WHERE id = ?2",
                params![i as i32 + 1, uuid_to_blob(*id)],
            )?;
        }
        Ok(())
    }
}

fn map_obligation(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeObligation> {
    let id_blob: Vec<u8> = row.get(0)?;
    let node_blob: Vec<u8> = row.get(1)?;
    Ok(NodeObligation {
        id: blob_to_uuid_sql(&id_blob)?,
        node_id: blob_to_uuid_sql(&node_blob)?,
        kind: row.get(2)?,
        ordinal: row.get(3)?,
        section: row.get(4)?,
        body: row.get(5)?,
    })
}
