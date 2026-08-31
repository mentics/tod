//! Node repository — nodes, capabilities, lifecycle, fields.

use crate::outline::types::{Capability, Node, NodeKind};
use crate::outline::uuid_blob::{blob_to_uuid_sql, ms_to_datetime, now_ms, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub struct NodeRepo<'a> {
    conn: &'a Connection,
}

impl<'a> NodeRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn insert(&self, node: &Node) -> Result<()> {
        let ref_blob = node.ref_target_id.map(uuid_to_blob);
        self.conn.execute(
            "INSERT INTO nodes (id, slug, title, kind, ref_target_id, slug_manual, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                uuid_to_blob(node.id),
                node.slug,
                node.title,
                node.kind.as_str(),
                ref_blob,
                i32::from(node.slug_manual),
                node.created_at.timestamp_millis(),
                node.updated_at.timestamp_millis(),
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, id: Uuid) -> Result<Option<Node>> {
        self.conn
            .query_row(
                "SELECT id, slug, title, kind, ref_target_id, slug_manual, created_at, updated_at
                 FROM nodes WHERE id = ?1",
                params![uuid_to_blob(id)],
                row_to_node,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_by_slug(&self, slug: &str) -> Result<Option<Node>> {
        self.conn
            .query_row(
                "SELECT id, slug, title, kind, ref_target_id, slug_manual, created_at, updated_at
                 FROM nodes WHERE lower(slug) = lower(?1)",
                params![slug],
                row_to_node,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn create_normal(&self, slug: &str, title: &str) -> Result<Node> {
        self.create_with_id(Uuid::new_v4(), slug, title)
    }

    pub fn create_with_id(&self, id: Uuid, slug: &str, title: &str) -> Result<Node> {
        let now = now_ms();
        let node = Node {
            id,
            slug: slug.to_string(),
            title: title.to_string(),
            kind: NodeKind::Normal,
            ref_target_id: None,
            slug_manual: false,
            created_at: ms_to_datetime(now),
            updated_at: ms_to_datetime(now),
        };
        self.insert(&node)?;
        Ok(node)
    }

    pub fn create_reference(&self, slug: &str, title: &str, target: Uuid) -> Result<Node> {
        let now = now_ms();
        let node = Node {
            id: Uuid::new_v4(),
            slug: slug.to_string(),
            title: title.to_string(),
            kind: NodeKind::Reference,
            ref_target_id: Some(target),
            slug_manual: false,
            created_at: ms_to_datetime(now),
            updated_at: ms_to_datetime(now),
        };
        self.insert(&node)?;
        Ok(node)
    }

    pub fn update_title(&self, id: Uuid, title: &str) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "UPDATE nodes SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![uuid_to_blob(id), title, now],
        )?;
        Ok(())
    }

    /// Regenerate slug from title / linked issue when the user has not set one manually.
    pub fn sync_auto_slug(&self, node_id: Uuid) -> Result<Option<String>> {
        let Some(node) = self.get(node_id)? else {
            return Ok(None);
        };
        if node.slug_manual {
            return Ok(None);
        }
        let ticket_id = self.get_ticket_id(node_id)?;
        let ticket = ticket_id.as_deref();
        let base = crate::outline::slug::derive_node_slug(&node.title, ticket);
        let slug = crate::outline::slug::allocate_unique_slug(self.conn, &base, Some(node_id))?;
        if slug == node.slug {
            return Ok(None);
        }
        let now = now_ms();
        self.conn.execute(
            "UPDATE nodes SET slug = ?2, updated_at = ?3 WHERE id = ?1",
            params![uuid_to_blob(node_id), slug, now],
        )?;
        Ok(Some(slug))
    }

    pub fn list_capabilities(&self, node_id: Uuid) -> Result<Vec<Capability>> {
        let mut stmt = self.conn.prepare(
            "SELECT capability FROM node_capabilities WHERE node_id = ?1 ORDER BY capability",
        )?;
        let caps = stmt
            .query_map(params![uuid_to_blob(node_id)], |row| {
                let s: String = row.get(0)?;
                Ok(Capability::parse(&s).unwrap_or(Capability::Spec))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(caps)
    }

    pub fn enable_capability(&self, node_id: Uuid, cap: Capability) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, ?2, ?3)",
            params![uuid_to_blob(node_id), cap.as_str(), now_ms()],
        )?;
        match cap {
            Capability::Lifecycle => {
                if self.get_lifecycle(node_id)?.is_none() {
                    self.set_lifecycle(node_id, "proposed")?;
                }
            }
            Capability::Agent => {
                let has_fields = self
                    .conn
                    .query_row(
                        "SELECT 1 FROM node_fields WHERE node_id = ?1",
                        params![uuid_to_blob(node_id)],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if !has_fields {
                    self.set_fields(node_id, None, None, None, &[], &[], &[])?;
                }
            }
            Capability::Spec => {}
        }
        Ok(())
    }

    pub fn enable_capabilities(&self, node_id: Uuid, caps: &[Capability]) -> Result<()> {
        for cap in caps {
            self.enable_capability(node_id, *cap)?;
        }
        Ok(())
    }

    pub fn set_lifecycle(&self, node_id: Uuid, state: &str) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO node_lifecycle (node_id, state, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET state = excluded.state, updated_at = excluded.updated_at",
            params![uuid_to_blob(node_id), state, now],
        )?;
        Ok(())
    }

    pub fn get_lifecycle(&self, node_id: Uuid) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT state FROM node_lifecycle WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_fields(
        &self,
        node_id: Uuid,
        repo: Option<&str>,
        branch: Option<&str>,
        notes: Option<&str>,
        tags: &[String],
        linked_issues: &[String],
        linked_prs: &[String],
    ) -> Result<()> {
        let now = now_ms();
        let tags_json = serde_json::to_string(tags)?;
        let issues_json = serde_json::to_string(linked_issues)?;
        let prs_json = serde_json::to_string(linked_prs)?;
        self.conn.execute(
            "INSERT INTO node_fields (node_id, repo, branch, notes, tags, linked_issues, linked_prs, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(node_id) DO UPDATE SET
               repo = excluded.repo, branch = excluded.branch, notes = excluded.notes,
               tags = excluded.tags, linked_issues = excluded.linked_issues,
               linked_prs = excluded.linked_prs, updated_at = excluded.updated_at",
            params![
                uuid_to_blob(node_id),
                repo,
                branch,
                notes,
                tags_json,
                issues_json,
                prs_json,
                now
            ],
        )?;
        Ok(())
    }

    pub fn get_tags(&self, node_id: Uuid) -> Result<Vec<String>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT tags FROM node_fields WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()?;
        Ok(raw
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default())
    }

    pub fn get_ticket_id(&self, node_id: Uuid) -> Result<Option<String>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT linked_issues FROM node_fields WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()?;
        let issues: Vec<String> = raw
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Ok(issues.into_iter().next())
    }

    pub fn get_repo(&self, node_id: Uuid) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT repo FROM node_fields WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn disable_capability_archive(
        &self,
        node_id: Uuid,
        cap: Capability,
        payload: &str,
    ) -> Result<()> {
        let archive_id = Uuid::new_v4();
        self.conn.execute(
            "INSERT INTO capability_archives (id, node_id, capability, archived_at, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                uuid_to_blob(archive_id),
                uuid_to_blob(node_id),
                cap.as_str(),
                now_ms(),
                payload
            ],
        )?;
        self.delete_capability_data(node_id, cap)?;
        self.conn.execute(
            "DELETE FROM node_capabilities WHERE node_id = ?1 AND capability = ?2",
            params![uuid_to_blob(node_id), cap.as_str()],
        )?;
        Ok(())
    }

    fn delete_capability_data(&self, node_id: Uuid, cap: Capability) -> Result<()> {
        let blob = uuid_to_blob(node_id);
        match cap {
            Capability::Spec => {
                self.conn.execute(
                    "DELETE FROM node_obligations WHERE node_id = ?1",
                    params![blob],
                )?;
                self.conn.execute(
                    "DELETE FROM node_extra_content WHERE node_id = ?1",
                    params![blob],
                )?;
                self.conn.execute(
                    "DELETE FROM interview_transcripts WHERE node_id = ?1",
                    params![blob],
                )?;
                self.conn.execute(
                    "DELETE FROM node_media_links WHERE node_id = ?1",
                    params![blob],
                )?;
            }
            Capability::Lifecycle => {
                self.conn.execute(
                    "DELETE FROM node_lifecycle WHERE node_id = ?1",
                    params![blob],
                )?;
            }
            Capability::Agent => {
                self.conn
                    .execute("DELETE FROM node_fields WHERE node_id = ?1", params![blob])?;
            }
        }
        Ok(())
    }
}

fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<Node> {
    let id_blob: Vec<u8> = row.get(0)?;
    let ref_blob: Option<Vec<u8>> = row.get(4)?;
    Ok(Node {
        id: blob_to_uuid_sql(&id_blob)?,
        slug: row.get(1)?,
        title: row.get(2)?,
        kind: NodeKind::parse(row.get::<_, String>(3)?.as_str()).unwrap_or(NodeKind::Normal),
        ref_target_id: ref_blob.as_deref().map(blob_to_uuid_sql).transpose()?,
        slug_manual: row.get::<_, i32>(5)? != 0,
        created_at: ms_to_datetime(row.get(6)?),
        updated_at: ms_to_datetime(row.get(7)?),
    })
}
