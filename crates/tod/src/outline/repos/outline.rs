//! Outline tree placement repository.

use crate::outline::types::OutlineEntry;
use crate::outline::uuid_blob::{blob_to_uuid_sql, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub struct OutlineRepo<'a> {
    conn: &'a Connection,
}

impl<'a> OutlineRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn insert(&self, entry: &OutlineEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO outline_entries (node_id, list_id, parent_id, ordinal, collapsed)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                uuid_to_blob(entry.node_id),
                uuid_to_blob(entry.list_id),
                entry.parent_id.map(uuid_to_blob),
                entry.ordinal,
                i32::from(entry.collapsed),
            ],
        )?;
        Ok(())
    }

    pub fn list_for_list(&self, list_id: Uuid) -> Result<Vec<OutlineEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, list_id, parent_id, ordinal, collapsed
             FROM outline_entries WHERE list_id = ?1
             ORDER BY COALESCE(parent_id, X'00'), ordinal",
        )?;
        let rows = stmt
            .query_map(params![uuid_to_blob(list_id)], row_to_entry)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn set_parent(&self, node_id: Uuid, parent_id: Option<Uuid>, ordinal: i32) -> Result<()> {
        self.conn.execute(
            "UPDATE outline_entries SET parent_id = ?2, ordinal = ?3 WHERE node_id = ?1",
            params![uuid_to_blob(node_id), parent_id.map(uuid_to_blob), ordinal],
        )?;
        Ok(())
    }

    pub fn set_collapsed(&self, node_id: Uuid, collapsed: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE outline_entries SET collapsed = ?2 WHERE node_id = ?1",
            params![uuid_to_blob(node_id), i32::from(collapsed)],
        )?;
        Ok(())
    }

    pub fn next_ordinal(&self, list_id: Uuid, parent_id: Option<Uuid>) -> Result<i32> {
        let max: Option<i32> = if let Some(parent) = parent_id {
            self.conn.query_row(
                "SELECT MAX(ordinal) FROM outline_entries WHERE list_id = ?1 AND parent_id = ?2",
                params![uuid_to_blob(list_id), uuid_to_blob(parent)],
                |row| row.get(0),
            )?
        } else {
            self.conn.query_row(
                "SELECT MAX(ordinal) FROM outline_entries WHERE list_id = ?1 AND parent_id IS NULL",
                params![uuid_to_blob(list_id)],
                |row| row.get(0),
            )?
        };
        Ok(max.unwrap_or(-1) + 1)
    }

    pub fn get_entry(&self, node_id: Uuid) -> Result<Option<OutlineEntry>> {
        self.conn
            .query_row(
                "SELECT node_id, list_id, parent_id, ordinal, collapsed FROM outline_entries WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                row_to_entry,
            )
            .optional()
            .map_err(Into::into)
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutlineEntry> {
    let node_blob: Vec<u8> = row.get(0)?;
    let list_blob: Vec<u8> = row.get(1)?;
    let parent_blob: Option<Vec<u8>> = row.get(2)?;
    Ok(OutlineEntry {
        node_id: blob_to_uuid_sql(&node_blob)?,
        list_id: blob_to_uuid_sql(&list_blob)?,
        parent_id: parent_blob
            .as_deref()
            .map(blob_to_uuid_sql)
            .transpose()?,
        ordinal: row.get(3)?,
        collapsed: row.get::<_, i32>(4)? != 0,
    })
}
