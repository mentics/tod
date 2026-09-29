//! Generator repository — generator config, managed nodes, and ticket links.
//!
//! Nothing stores which node came from which generator: a node *is* ticket T
//! when its `node_fields.ticket` is T, and every link is found by that id
//! (`doc/lifecycle/plan-step-phases.md`, section 4). A managed node's
//! generator is its nearest generator ancestor; a non-managed node with
//! ticket T receives what any generator returning T fetches.

use crate::outline::types::Capability;
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
    /// Quick-accept destination: the node newly-accepted tickets are copied
    /// under. `None` until the user sets it — accept is inert until then.
    pub accept_destination_node_id: Option<Uuid>,
    /// Capabilities auto-enabled on a ticket's copy when it is accepted.
    pub accept_capabilities: Vec<Capability>,
}

/// A managed node's link to its data source, derived from its ticket and its
/// nearest generator ancestor.
#[derive(Debug, Clone)]
pub struct ManagedNodeLink {
    pub node_id: Uuid,
    pub generator_node_id: Uuid,
    pub external_id: String,
    pub source_type: String,
    pub user_modified_fields: Vec<String>,
}

/// Data-source type whose items are Linear issues.
pub const SOURCE_TYPE_LINEAR: &str = "linear";

impl ManagedNodeLink {
    /// Whether a copy of this node carries its external id as a ticket
    /// (the Ticket capability) rather than in its title.
    pub fn is_ticket(&self) -> bool {
        self.source_type == SOURCE_TYPE_LINEAR
    }

    /// Title for a copied-out node. A copy is an ordinary node, so nothing
    /// shows the external id beside it: its title carries it, unless the
    /// copy's Ticket capability already does.
    pub fn copy_title(&self, title: &str) -> String {
        copy_title(&self.source_type, &self.external_id, title)
    }
}

/// See [`ManagedNodeLink::copy_title`].
pub fn copy_title(source_type: &str, external_id: &str, title: &str) -> String {
    if source_type == SOURCE_TYPE_LINEAR {
        title.to_string()
    } else {
        format!("{external_id}: {title}")
    }
}

/// A non-managed node that is some ticket: a refresh returning that ticket
/// pushes its fields here, except those the user has edited.
#[derive(Debug, Clone)]
pub struct TicketHolder {
    pub node_id: Uuid,
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
                        last_refresh_status, last_refresh_error, last_refresh_at,
                        accept_destination_node_id, accept_capabilities
                 FROM node_generator_config WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                row_to_config,
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

    /// Set the quick-accept destination node and capabilities for a
    /// generator. Both are `None`/empty until explicitly set by the user.
    pub fn set_accept_config(
        &self,
        node_id: Uuid,
        destination_node_id: Option<Uuid>,
        capabilities: &[Capability],
    ) -> Result<()> {
        let caps_json = serde_json::to_string(capabilities)?;
        self.conn.execute(
            "UPDATE node_generator_config
             SET accept_destination_node_id = ?2, accept_capabilities = ?3
             WHERE node_id = ?1",
            params![
                uuid_to_blob(node_id),
                destination_node_id.map(uuid_to_blob),
                caps_json,
            ],
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

    // ── Ticket links ────────────────────────────────────────────────────

    /// The link of a managed node: its ticket, and the generator it sits
    /// under. `None` for a node that is not managed, has no ticket, or has
    /// no generator above it.
    pub fn get_link(&self, node_id: Uuid) -> Result<Option<ManagedNodeLink>> {
        if !self.is_managed(node_id).unwrap_or(false) {
            return Ok(None);
        }
        let Some((external_id, user_modified_fields)) = self.ticket_row(node_id)? else {
            return Ok(None);
        };
        let Some(generator_node_id) = self.find_generator_ancestor(node_id)? else {
            return Ok(None);
        };
        let source_type = self
            .get_config(generator_node_id)?
            .map(|config| config.data_source_type)
            .unwrap_or_default();
        Ok(Some(ManagedNodeLink {
            node_id,
            generator_node_id,
            external_id,
            source_type,
            user_modified_fields,
        }))
    }

    /// Links of every managed node under a generator that has a ticket.
    pub fn links_for_generator(&self, generator_node_id: Uuid) -> Result<Vec<ManagedNodeLink>> {
        let source_type = self
            .get_config(generator_node_id)?
            .map(|config| config.data_source_type)
            .unwrap_or_default();
        let mut links = Vec::new();
        for node_id in self.collect_managed_descendants(generator_node_id)? {
            if let Some((external_id, user_modified_fields)) = self.ticket_row(node_id)? {
                links.push(ManagedNodeLink {
                    node_id,
                    generator_node_id,
                    external_id,
                    source_type: source_type.clone(),
                    user_modified_fields,
                });
            }
        }
        Ok(links)
    }

    /// Every non-managed node that is `ticket`, whichever generator (if any)
    /// it was accepted from: the nodes a refresh returning it updates.
    pub fn holders_of(&self, ticket: &str) -> Result<Vec<TicketHolder>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.node_id, f.ticket_modified_fields
             FROM node_fields f JOIN nodes n ON n.id = f.node_id
             WHERE f.ticket = ?1 AND n.managed = 0",
        )?;
        let holders = stmt
            .query_map(params![ticket], |row| {
                let id_blob: Vec<u8> = row.get(0)?;
                let fields: String = row.get(1)?;
                Ok(TicketHolder {
                    node_id: blob_to_uuid_sql(&id_blob)?,
                    user_modified_fields: serde_json::from_str(&fields).unwrap_or_default(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(holders)
    }

    /// Whether some non-managed node is `ticket`: the generator's
    /// "already accepted" marker.
    pub fn is_accepted(&self, ticket: &str) -> Result<bool> {
        Ok(self
            .conn
            .prepare(
                "SELECT 1 FROM node_fields f JOIN nodes n ON n.id = f.node_id
                 WHERE f.ticket = ?1 AND n.managed = 0",
            )?
            .exists(params![ticket])?)
    }

    /// The fields a refresh must leave alone on a node that is a ticket.
    pub fn user_modified_fields(&self, node_id: Uuid) -> Result<Vec<String>> {
        Ok(self
            .ticket_row(node_id)?
            .map(|(_, fields)| fields)
            .unwrap_or_default())
    }

    /// A node's ticket and the fields a refresh must leave alone.
    fn ticket_row(&self, node_id: Uuid) -> Result<Option<(String, Vec<String>)>> {
        let row: Option<(Option<String>, String)> = self
            .conn
            .query_row(
                "SELECT ticket, ticket_modified_fields FROM node_fields WHERE node_id = ?1",
                params![uuid_to_blob(node_id)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(ticket, fields)| {
            Some((ticket?, serde_json::from_str(&fields).unwrap_or_default()))
        }))
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
            "DELETE FROM outline_entries WHERE node_id = ?1",
            params![uuid_to_blob(id)],
        )?;
        self.conn
            .execute("DELETE FROM nodes WHERE id = ?1", params![uuid_to_blob(id)])?;
        Ok(())
    }

    /// Every managed node under `parent_id`, at any depth.
    pub fn managed_descendants(&self, parent_id: Uuid) -> Result<Vec<Uuid>> {
        self.collect_managed_descendants(parent_id)
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
            "UPDATE node_fields SET ticket_modified_fields = ?2 WHERE node_id = ?1",
            params![uuid_to_blob(node_id), json],
        )?;
        Ok(())
    }

    /// Mark a single field as user-modified on a node that is a ticket, so a
    /// refresh leaves it alone. No-op if the node has no ticket.
    pub fn mark_field_modified(&self, node_id: Uuid, field: &str) -> Result<()> {
        let Some((_, mut fields)) = self.ticket_row(node_id)? else {
            return Ok(());
        };
        if fields.iter().any(|f| f == field) {
            return Ok(());
        }
        fields.push(field.to_string());
        self.update_user_modified_fields(node_id, &fields)
    }
}

fn row_to_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<GeneratorConfig> {
    let id_blob: Vec<u8> = row.get(0)?;
    let dest_blob: Option<Vec<u8>> = row.get(6)?;
    let caps_json: String = row.get(7)?;
    let accept_capabilities: Vec<Capability> = serde_json::from_str(&caps_json).unwrap_or_default();
    Ok(GeneratorConfig {
        node_id: blob_to_uuid_sql(&id_blob)?,
        data_source_type: row.get(1)?,
        config_json: row.get(2)?,
        last_refresh_status: row.get(3)?,
        last_refresh_error: row.get(4)?,
        last_refresh_at: row.get(5)?,
        accept_destination_node_id: dest_blob.map(|b| blob_to_uuid_sql(&b)).transpose()?,
        accept_capabilities,
    })
}
