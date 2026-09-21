//! Generator repository — generator config, managed nodes, data-source links.

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub struct GeneratorRepo<'a> {
    conn: &'a Connection,
}

/// Generator configuration row.
#[derive(Debug, Clone)]
pub struct GeneratorConfig {
    pub node_id: Uuid,
    pub data_source_type: String,
    pub config_json: String,
    pub last_refresh_status: Option<String>,
    pub last_refresh_error: Option<String>,
    pub last_refresh_at: Option<i64>,
}

/// Data-source link on a managed or copied-out node.
#[derive(Debug, Clone)]
pub struct ManagedNodeLink {
    pub node_id: Uuid,
    pub generator_node_id: Uuid,
    pub external_id: String,
    pub source_type: String,
    pub user_modified_fields: Vec<String>,
}

impl<'a> GeneratorRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    // ── Generator config ────────────────────────────────────────────────

    pub fn get_config(&self, node_id: Uuid) -> Result<Option<GeneratorConfig>> {
        self.conn
            .query_row(
                "SELECT node_id, data_source_type, config_json,
                        last_refresh_status, last_refresh_error, last_refresh_at
                 FROM node_generator_config WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| {
                    let id_blob: Vec<u8> = row.get(0)?;
                    Ok(GeneratorConfig {
                        node_id: blob_to_uuid_sql(&id_blob)?,
                        data_source_type: row.get(1)?,
                        config_json: row.get(2)?,
                        last_refresh_status: row.get(3)?,
                        last_refresh_error: row.get(4)?,
                        last_refresh_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_config(
        &self,
        node_id: Uuid,
        data_source_type: &str,
        config_json: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO node_generator_config (node_id, data_source_type, config_json)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET config_json = excluded.config_json",
            params![uuid_to_blob(node_id), data_source_type, config_json],
        )?;
        Ok(())
    }

    pub fn delete_config(&self, node_id: Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM node_generator_config WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
        )?;
        Ok(())
    }

    pub fn set_refresh_status(
        &self,
        node_id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE node_generator_config
             SET last_refresh_status = ?2, last_refresh_error = ?3, last_refresh_at = ?4
             WHERE node_id = ?1",
            params![uuid_to_blob(node_id), status, error, now_ms()],
        )?;
        Ok(())
    }

    /// Generators whose persisted refresh status is `status`.
    pub fn nodes_with_refresh_status(&self, status: &str) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id FROM node_generator_config WHERE last_refresh_status = ?1",
        )?;
        let rows = stmt.query_map(params![status], |row| {
            let id_blob: Vec<u8> = row.get(0)?;
            blob_to_uuid_sql(&id_blob)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    // ── Managed nodes ───────────────────────────────────────────────────

    pub fn set_managed(&self, node_id: Uuid, managed: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE nodes SET managed = ?2 WHERE id = ?1",
            params![uuid_to_blob(node_id), i32::from(managed)],
        )?;
        Ok(())
    }

    pub fn is_managed(&self, node_id: Uuid) -> Result<bool> {
        let val: i32 = self
            .conn
            .query_row(
                "SELECT managed FROM nodes WHERE id = ?1",
                params![uuid_to_blob(node_id)],
                |row| row.get(0),
            )
            .context("node not found")?;
        Ok(val != 0)
    }

    /// Check if a node is inside a generator subtree (i.e. has a generator ancestor).
    pub fn is_in_generator_subtree(&self, node_id: Uuid) -> Result<bool> {
        // Walk up the parent chain looking for a node with generator capability.
        let mut current = Some(node_id);
        while let Some(nid) = current {
            let has_gen: bool = self.conn.query_row(
                "SELECT EXISTS(
                        SELECT 1 FROM node_capabilities
                        WHERE node_id = ?1 AND capability = 'generator'
                    )",
                params![uuid_to_blob(nid)],
                |row| row.get(0),
            )?;
            if has_gen {
                return Ok(true);
            }
            // Get parent
            current = self
                .conn
                .query_row(
                    "SELECT parent_id FROM outline_entries WHERE node_id = ?1",
                    params![uuid_to_blob(nid)],
                    |row| {
                        let blob: Option<Vec<u8>> = row.get(0)?;
                        match blob {
                            Some(b) => Ok(Some(blob_to_uuid_sql(&b)?)),
                            None => Ok(None),
                        }
                    },
                )
                .optional()?
                .flatten();
        }
        Ok(false)
    }

    /// Find the generator node that is the root of the subtree containing `node_id`.
    pub fn find_generator_ancestor(&self, node_id: Uuid) -> Result<Option<Uuid>> {
        let mut current = Some(node_id);
        while let Some(nid) = current {
            let has_gen: bool = self.conn.query_row(
                "SELECT EXISTS(
                        SELECT 1 FROM node_capabilities
                        WHERE node_id = ?1 AND capability = 'generator'
                    )",
                params![uuid_to_blob(nid)],
                |row| row.get(0),
            )?;
            if has_gen {
                return Ok(Some(nid));
            }
            current = self
                .conn
                .query_row(
                    "SELECT parent_id FROM outline_entries WHERE node_id = ?1",
                    params![uuid_to_blob(nid)],
                    |row| {
                        let blob: Option<Vec<u8>> = row.get(0)?;
                        match blob {
                            Some(b) => Ok(Some(blob_to_uuid_sql(&b)?)),
                            None => Ok(None),
                        }
                    },
                )
                .optional()?
                .flatten();
        }
        Ok(None)
    }

    // ── Data-source links ───────────────────────────────────────────────

    pub fn set_link(
        &self,
        node_id: Uuid,
        generator_node_id: Uuid,
        external_id: &str,
        source_type: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO managed_node_links (node_id, generator_node_id, external_id, source_type, user_modified_fields)
             VALUES (?1, ?2, ?3, ?4, '[]')
             ON CONFLICT(node_id) DO UPDATE SET
                generator_node_id = excluded.generator_node_id,
                external_id = excluded.external_id,
                source_type = excluded.source_type",
            params![
                uuid_to_blob(node_id),
                uuid_to_blob(generator_node_id),
                external_id,
                source_type,
            ],
        )?;
        Ok(())
    }

    pub fn get_link(&self, node_id: Uuid) -> Result<Option<ManagedNodeLink>> {
        self.conn
            .query_row(
                "SELECT node_id, generator_node_id, external_id, source_type, user_modified_fields
                 FROM managed_node_links WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                row_to_link,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn delete_link(&self, node_id: Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM managed_node_links WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
        )?;
        Ok(())
    }

    /// Delete all data-source links that originated from a given generator node.
    pub fn clear_links_for_generator(&self, generator_node_id: Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM managed_node_links WHERE generator_node_id = ?1",
            params![uuid_to_blob(generator_node_id)],
        )?;
        Ok(())
    }

    /// List links on copied-out (non-managed) nodes matching a generator and
    /// external id — the set of copies a refresh should push field updates to.
    pub fn copy_links_for(
        &self,
        generator_node_id: Uuid,
        external_id: &str,
    ) -> Result<Vec<ManagedNodeLink>> {
        let mut stmt = self.conn.prepare(
            "SELECT l.node_id, l.generator_node_id, l.external_id, l.source_type, l.user_modified_fields
             FROM managed_node_links l
             JOIN nodes n ON n.id = l.node_id
             WHERE l.generator_node_id = ?1 AND l.external_id = ?2 AND n.managed = 0",
        )?;
        let links = stmt
            .query_map(
                params![uuid_to_blob(generator_node_id), external_id],
                row_to_link,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(links)
    }

    /// Clear the data-source link on any copied-out node (not the managed node
    /// itself, which reconciliation deletes separately) still referencing an
    /// external item that a generator's refresh determined no longer exists.
    /// The node keeps its title and content but stops receiving updates.
    pub fn clear_stale_copy_links(&self, generator_node_id: Uuid, external_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM managed_node_links
             WHERE generator_node_id = ?1 AND external_id = ?2
               AND node_id IN (SELECT id FROM nodes WHERE managed = 0)",
            params![uuid_to_blob(generator_node_id), external_id],
        )?;
        Ok(())
    }

    /// List all links for managed nodes under a generator.
    pub fn links_for_generator(&self, generator_node_id: Uuid) -> Result<Vec<ManagedNodeLink>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, generator_node_id, external_id, source_type, user_modified_fields
             FROM managed_node_links WHERE generator_node_id = ?1",
        )?;
        let links = stmt
            .query_map(params![uuid_to_blob(generator_node_id)], row_to_link)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(links)
    }

    /// List links by external id (across all generators).
    pub fn links_for_external_id(&self, external_id: &str) -> Result<Vec<ManagedNodeLink>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, generator_node_id, external_id, source_type, user_modified_fields
             FROM managed_node_links WHERE external_id = ?1",
        )?;
        let links = stmt
            .query_map(params![external_id], row_to_link)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(links)
    }

    /// Delete all managed child nodes under a generator node (recursive).
    pub fn delete_managed_children(&self, generator_node_id: Uuid) -> Result<()> {
        // Find all managed descendants by walking the outline tree from the generator.
        let managed_ids = self.collect_managed_descendants(generator_node_id)?;
        for id in &managed_ids {
            self.delete_node_row(*id)?;
        }
        Ok(())
    }

    /// Delete a single managed node and its managed descendants (reconciliation
    /// removal of an item no longer returned by the data source).
    pub fn delete_managed_node(&self, node_id: Uuid) -> Result<()> {
        let mut ids = self.collect_managed_descendants(node_id)?;
        ids.push(node_id);
        for id in &ids {
            self.delete_node_row(*id)?;
        }
        Ok(())
    }

    fn delete_node_row(&self, id: Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM managed_node_links WHERE node_id = ?1",
            params![uuid_to_blob(id)],
        )?;
        self.conn.execute(
            "DELETE FROM outline_entries WHERE node_id = ?1",
            params![uuid_to_blob(id)],
        )?;
        self.conn
            .execute("DELETE FROM nodes WHERE id = ?1", params![uuid_to_blob(id)])?;
        Ok(())
    }

    fn collect_managed_descendants(&self, parent_id: Uuid) -> Result<Vec<Uuid>> {
        let mut result = Vec::new();
        let mut stack = vec![parent_id];
        while let Some(pid) = stack.pop() {
            let mut stmt = self.conn.prepare(
                "SELECT e.node_id, n.managed FROM outline_entries e
                 JOIN nodes n ON n.id = e.node_id
                 WHERE e.parent_id = ?1",
            )?;
            let children: Vec<(Uuid, bool)> = stmt
                .query_map(params![uuid_to_blob(pid)], |row| {
                    let id_blob: Vec<u8> = row.get(0)?;
                    let managed: i32 = row.get(1)?;
                    Ok((blob_to_uuid_sql(&id_blob)?, managed != 0))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            for (child_id, is_managed) in children {
                if is_managed {
                    result.push(child_id);
                    stack.push(child_id);
                }
            }
        }
        Ok(result)
    }

    pub fn update_user_modified_fields(&self, node_id: Uuid, fields: &[String]) -> Result<()> {
        let json = serde_json::to_string(fields)?;
        self.conn.execute(
            "UPDATE managed_node_links SET user_modified_fields = ?2 WHERE node_id = ?1",
            params![uuid_to_blob(node_id), json],
        )?;
        Ok(())
    }

    /// A managed node is greyed out when a copy of the same external item
    /// exists outside any generator subtree (i.e. another link with the same
    /// `external_id` points at a non-managed node). Computed live from the
    /// links table, so it clears immediately when the last copy is deleted
    /// and survives config-change rebuilds (which preserve node ids for
    /// external ids that keep matching).
    pub fn is_greyed_out(&self, node_id: Uuid) -> Result<bool> {
        if !self.is_managed(node_id)? {
            return Ok(false);
        }
        let Some(link) = self.get_link(node_id)? else {
            return Ok(false);
        };
        for other in self.links_for_external_id(&link.external_id)? {
            if other.node_id != node_id && !self.is_managed(other.node_id)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Mark a single field as user-modified on a linked node, if it has a
    /// data-source link. No-op if the node has no link (e.g. a plain node).
    pub fn mark_field_modified(&self, node_id: Uuid, field: &str) -> Result<()> {
        let Some(link) = self.get_link(node_id)? else {
            return Ok(());
        };
        if link.user_modified_fields.iter().any(|f| f == field) {
            return Ok(());
        }
        let mut fields = link.user_modified_fields;
        fields.push(field.to_string());
        self.update_user_modified_fields(node_id, &fields)
    }
}

fn row_to_link(row: &rusqlite::Row<'_>) -> rusqlite::Result<ManagedNodeLink> {
    let id_blob: Vec<u8> = row.get(0)?;
    let gen_blob: Vec<u8> = row.get(1)?;
    let fields_json: String = row.get(4)?;
    let fields: Vec<String> = serde_json::from_str(&fields_json).unwrap_or_default();
    Ok(ManagedNodeLink {
        node_id: blob_to_uuid_sql(&id_blob)?,
        generator_node_id: blob_to_uuid_sql(&gen_blob)?,
        external_id: row.get(2)?,
        source_type: row.get(3)?,
        user_modified_fields: fields,
    })
}
