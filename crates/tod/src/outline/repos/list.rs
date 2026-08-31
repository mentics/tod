//! List repository.

use crate::outline::types::OutlineList;
use crate::outline::uuid_blob::{blob_to_uuid_sql, ms_to_datetime, now_ms, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub struct ListRepo<'a> {
    conn: &'a Connection,
}

impl<'a> ListRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn list_all(&self) -> Result<Vec<OutlineList>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, slug, title, created_at, updated_at FROM lists ORDER BY lower(title)",
        )?;
        let rows = stmt
            .query_map([], |row| {
                let id_blob: Vec<u8> = row.get(0)?;
                Ok(OutlineList {
                    id: blob_to_uuid_sql(&id_blob)?,
                    slug: row.get(1)?,
                    title: row.get(2)?,
                    created_at: ms_to_datetime(row.get(3)?),
                    updated_at: ms_to_datetime(row.get(4)?),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get(&self, id: Uuid) -> Result<Option<OutlineList>> {
        self.conn
            .query_row(
                "SELECT id, slug, title, created_at, updated_at FROM lists WHERE id = ?1",
                params![uuid_to_blob(id)],
                |row| {
                    let id_blob: Vec<u8> = row.get(0)?;
                    Ok(OutlineList {
                        id: blob_to_uuid_sql(&id_blob)?,
                        slug: row.get(1)?,
                        title: row.get(2)?,
                        created_at: ms_to_datetime(row.get(3)?),
                        updated_at: ms_to_datetime(row.get(4)?),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_by_slug(&self, slug: &str) -> Result<Option<OutlineList>> {
        self.conn
            .query_row(
                "SELECT id, slug, title, created_at, updated_at FROM lists WHERE lower(slug) = lower(?1)",
                params![slug],
                |row| {
                    let id_blob: Vec<u8> = row.get(0)?;
                    Ok(OutlineList {
                        id: blob_to_uuid_sql(&id_blob)?,
                        slug: row.get(1)?,
                        title: row.get(2)?,
                        created_at: ms_to_datetime(row.get(3)?),
                        updated_at: ms_to_datetime(row.get(4)?),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn insert(&self, list: &OutlineList) -> Result<()> {
        self.conn.execute(
            "INSERT INTO lists (id, slug, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                uuid_to_blob(list.id),
                list.slug,
                list.title,
                list.created_at.timestamp_millis(),
                list.updated_at.timestamp_millis(),
            ],
        )?;
        Ok(())
    }

    pub fn create(&self, slug: &str, title: &str) -> Result<OutlineList> {
        let now = now_ms();
        let list = OutlineList {
            id: Uuid::new_v4(),
            slug: slug.to_string(),
            title: title.to_string(),
            created_at: ms_to_datetime(now),
            updated_at: ms_to_datetime(now),
        };
        self.insert(&list)?;
        Ok(list)
    }
}
