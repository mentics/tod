//! Archive snapshots for subtree delete / restore.

use crate::outline::repos::{NodeRepo, OutlineRepo};
use crate::outline::types::{Capability, NodeKind, OutlineEntry};
use crate::outline::uuid_blob::{blob_to_uuid_sql, ms_to_datetime, now_ms, uuid_to_blob};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

pub const ARCHIVE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSubtreeArchive {
    pub version: u32,
    pub archived_at: i64,
    pub root_node_id: Uuid,
    pub list_id: Uuid,
    pub root_parent_id: Option<Uuid>,
    pub root_ordinal: i32,
    pub nodes: Vec<ArchivedNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedNode {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub kind: String,
    pub ref_target_id: Option<Uuid>,
    pub slug_manual: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub outline: ArchivedOutlineEntry,
    pub capabilities: Vec<String>,
    pub lifecycle: Option<String>,
    pub fields: Option<ArchivedFields>,
    pub obligations: Vec<ArchivedObligation>,
    pub capability_archives: Vec<ArchivedCapabilityArchive>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedOutlineEntry {
    pub list_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub ordinal: i32,
    pub collapsed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedFields {
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub notes: Option<String>,
    pub tags: String,
    pub linked_issues: String,
    pub linked_prs: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedObligation {
    pub id: Uuid,
    pub kind: String,
    pub ordinal: i32,
    pub section: Option<String>,
    pub body: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedCapabilityArchive {
    pub id: Uuid,
    pub capability: String,
    pub archived_at: i64,
    pub payload: String,
}

/// Build a JSON snapshot of `root_id` and all descendants in the same list.
pub fn build_subtree_archive(conn: &Connection, root_id: Uuid) -> Result<NodeSubtreeArchive> {
    let outline = OutlineRepo::new(conn);
    let entry = outline
        .get_entry(root_id)?
        .context("node missing from outline")?;
    let list_id = entry.list_id;
    let subtree = collect_subtree_ids(&outline, list_id, root_id)?;
    let mut nodes = Vec::with_capacity(subtree.len());
    for node_id in &subtree {
        nodes.push(snapshot_node(conn, *node_id)?);
    }
    Ok(NodeSubtreeArchive {
        version: ARCHIVE_VERSION,
        archived_at: now_ms(),
        root_node_id: root_id,
        list_id,
        root_parent_id: entry.parent_id,
        root_ordinal: entry.ordinal,
        nodes,
    })
}

/// Archive subtree, insert row, delete nodes. Returns (archive id, list id).
pub fn delete_subtree_archived(conn: &Connection, root_id: Uuid) -> Result<(Uuid, Uuid)> {
    validate_delete(conn, root_id)?;
    let archive = build_subtree_archive(conn, root_id)?;
    let list_id = archive.list_id;
    let archive_id = Uuid::new_v4();
    let payload = serde_json::to_string(&archive).context("serialize subtree archive")?;
    conn.execute(
        "INSERT INTO node_subtree_archives (id, root_node_id, list_id, archived_at, payload)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            uuid_to_blob(archive_id),
            uuid_to_blob(root_id),
            uuid_to_blob(list_id),
            archive.archived_at,
            payload,
        ],
    )?;
    let outline = OutlineRepo::new(conn);
    let subtree = collect_subtree_ids(&outline, list_id, root_id)?;
    let delete_order = subtree_delete_order(&outline, list_id, &subtree)?;
    for node_id in delete_order {
        conn.execute(
            "DELETE FROM nodes WHERE id = ?1",
            params![uuid_to_blob(node_id)],
        )?;
    }
    Ok((archive_id, list_id))
}

/// Restore a previously archived subtree. Returns list id. Removes archive row on success.
pub fn restore_subtree(conn: &Connection, archive_id: Uuid, _media_root: &Path) -> Result<Uuid> {
    let (payload, list_id): (String, Vec<u8>) = conn.query_row(
        "SELECT payload, list_id FROM node_subtree_archives WHERE id = ?1",
        params![uuid_to_blob(archive_id)],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let list_id = blob_to_uuid_sql(&list_id)?;
    let archive: NodeSubtreeArchive =
        serde_json::from_str(&payload).context("deserialize subtree archive")?;
    let depth_order = depth_sort(&archive.nodes);
    for node in &depth_order {
        restore_node(conn, node)?;
    }
    conn.execute(
        "DELETE FROM node_subtree_archives WHERE id = ?1",
        params![uuid_to_blob(archive_id)],
    )?;
    Ok(list_id)
}

fn validate_delete(conn: &Connection, root_id: Uuid) -> Result<()> {
    let outline = OutlineRepo::new(conn);
    let entry = outline
        .get_entry(root_id)?
        .context("node missing from outline")?;
    let list_id = entry.list_id;
    let subtree = collect_subtree_ids(&outline, list_id, root_id)?;
    for node_id in &subtree {
        let agent_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM agent_configs WHERE node_id = ?1",
            params![uuid_to_blob(*node_id)],
            |row| row.get(0),
        )?;
        if agent_count > 0 {
            anyhow::bail!("cannot delete: node has associated agents");
        }
    }
    let placeholders = subtree
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let blobs: Vec<Vec<u8>> = subtree.iter().map(|id| uuid_to_blob(*id)).collect();
    let sql = format!(
        "SELECT id FROM nodes WHERE kind = 'reference' AND ref_target_id IN ({placeholders})
         AND id NOT IN ({placeholders})"
    );
    let mut refs: Vec<Uuid> = Vec::new();
    {
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = blobs
            .iter()
            .chain(blobs.iter())
            .map(|b| b as &dyn rusqlite::ToSql)
            .collect();
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
        while let Some(row) = rows.next()? {
            let blob: Vec<u8> = row.get(0)?;
            refs.push(blob_to_uuid_sql(&blob)?);
        }
    }
    if !refs.is_empty() {
        anyhow::bail!("cannot delete: subtree is referenced by other nodes");
    }
    Ok(())
}

fn snapshot_node(conn: &Connection, node_id: Uuid) -> Result<ArchivedNode> {
    let node = NodeRepo::new(conn)
        .get(node_id)?
        .context("node missing")?;
    let outline = OutlineRepo::new(conn)
        .get_entry(node_id)?
        .context("outline entry missing")?;
    let capabilities = NodeRepo::new(conn)
        .list_capabilities(node_id)?
        .into_iter()
        .map(|c| c.as_str().to_string())
        .collect();
    let lifecycle: Option<String> = conn
        .query_row(
            "SELECT state FROM node_lifecycle WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
            |row| row.get(0),
        )
        .optional()?;
    let fields = conn
        .query_row(
            "SELECT repo, branch, notes, tags, linked_issues, linked_prs, updated_at
             FROM node_fields WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
            |row| {
                Ok(ArchivedFields {
                    repo: row.get(0)?,
                    branch: row.get(1)?,
                    notes: row.get(2)?,
                    tags: row.get(3)?,
                    linked_issues: row.get(4)?,
                    linked_prs: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            },
        )
        .optional()?;
    let mut obligations = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, kind, ordinal, section, body, created_at, updated_at
             FROM node_obligations WHERE node_id = ?1 ORDER BY kind, ordinal",
        )?;
        let rows = stmt.query_map(params![uuid_to_blob(node_id)], |row| {
            let id_blob: Vec<u8> = row.get(0)?;
            Ok(ArchivedObligation {
                id: blob_to_uuid_sql(&id_blob)?,
                kind: row.get(1)?,
                ordinal: row.get(2)?,
                section: row.get(3)?,
                body: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        for row in rows {
            obligations.push(row?);
        }
    }
    let mut capability_archives = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, capability, archived_at, payload FROM capability_archives WHERE node_id = ?1",
        )?;
        let rows = stmt.query_map(params![uuid_to_blob(node_id)], |row| {
            let id_blob: Vec<u8> = row.get(0)?;
            Ok(ArchivedCapabilityArchive {
                id: blob_to_uuid_sql(&id_blob)?,
                capability: row.get(1)?,
                archived_at: row.get(2)?,
                payload: row.get(3)?,
            })
        })?;
        for row in rows {
            capability_archives.push(row?);
        }
    }
    Ok(ArchivedNode {
        id: node.id,
        slug: node.slug,
        title: node.title,
        kind: node.kind.as_str().to_string(),
        ref_target_id: node.ref_target_id,
        slug_manual: node.slug_manual,
        created_at: node.created_at.timestamp_millis(),
        updated_at: node.updated_at.timestamp_millis(),
        outline: ArchivedOutlineEntry {
            list_id: outline.list_id,
            parent_id: outline.parent_id,
            ordinal: outline.ordinal,
            collapsed: outline.collapsed,
        },
        capabilities,
        lifecycle,
        fields,
        obligations,
        capability_archives,
    })
}

/// JSON snapshot of capability-owned data before disable (for undo archives).
pub fn build_capability_disable_payload(
    conn: &Connection,
    node_id: Uuid,
    cap: Capability,
) -> Result<String> {
    let payload = match cap {
        Capability::Spec => {
            let obligations = snapshot_obligations(conn, node_id)?;
            serde_json::json!({ "obligations": obligations })
        }
        Capability::Lifecycle => {
            let state: Option<String> = conn
                .query_row(
                    "SELECT state FROM node_lifecycle WHERE node_id = ?1",
                    params![uuid_to_blob(node_id)],
                    |row| row.get(0),
                )
                .optional()?;
            serde_json::json!({ "lifecycle": state })
        }
        Capability::Agent => {
            let fields = conn
                .query_row(
                    "SELECT repo, branch, notes, tags, linked_issues, linked_prs, updated_at
                     FROM node_fields WHERE node_id = ?1",
                    params![uuid_to_blob(node_id)],
                    |row| {
                        Ok(ArchivedFields {
                            repo: row.get(0)?,
                            branch: row.get(1)?,
                            notes: row.get(2)?,
                            tags: row.get(3)?,
                            linked_issues: row.get(4)?,
                            linked_prs: row.get(5)?,
                            updated_at: row.get(6)?,
                        })
                    },
                )
                .optional()?;
            serde_json::json!({ "fields": fields })
        }
    };
    serde_json::to_string(&payload).context("serialize capability archive")
}

fn snapshot_obligations(conn: &Connection, node_id: Uuid) -> Result<Vec<ArchivedObligation>> {
    let mut obligations = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, kind, ordinal, section, body, created_at, updated_at
         FROM node_obligations WHERE node_id = ?1 ORDER BY kind, ordinal",
    )?;
    let rows = stmt.query_map(params![uuid_to_blob(node_id)], |row| {
        let id_blob: Vec<u8> = row.get(0)?;
        Ok(ArchivedObligation {
            id: blob_to_uuid_sql(&id_blob)?,
            kind: row.get(1)?,
            ordinal: row.get(2)?,
            section: row.get(3)?,
            body: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })?;
    for row in rows {
        obligations.push(row?);
    }
    Ok(obligations)
}

fn restore_node(conn: &Connection, archived: &ArchivedNode) -> Result<()> {
    let blob = uuid_to_blob(archived.id);
    let ref_blob = archived.ref_target_id.map(uuid_to_blob);
    conn.execute(
        "INSERT OR IGNORE INTO nodes (id, slug, title, kind, ref_target_id, slug_manual, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            blob,
            archived.slug,
            archived.title,
            archived.kind,
            ref_blob,
            i32::from(archived.slug_manual),
            archived.created_at,
            archived.updated_at,
        ],
    )?;
    let outline = &archived.outline;
    conn.execute(
        "INSERT OR IGNORE INTO outline_entries (node_id, list_id, parent_id, ordinal, collapsed)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            blob,
            uuid_to_blob(outline.list_id),
            outline.parent_id.map(uuid_to_blob),
            outline.ordinal,
            i32::from(outline.collapsed),
        ],
    )?;
    let now = now_ms();
    for cap in &archived.capabilities {
        conn.execute(
            "INSERT OR IGNORE INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, ?2, ?3)",
            params![blob, cap, now],
        )?;
    }
    if let Some(state) = &archived.lifecycle {
        conn.execute(
            "INSERT OR IGNORE INTO node_lifecycle (node_id, state, updated_at) VALUES (?1, ?2, ?3)",
            params![blob, state, now],
        )?;
    }
    if let Some(fields) = &archived.fields {
        conn.execute(
            "INSERT OR IGNORE INTO node_fields (node_id, repo, branch, notes, tags, linked_issues, linked_prs, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                blob,
                fields.repo,
                fields.branch,
                fields.notes,
                fields.tags,
                fields.linked_issues,
                fields.linked_prs,
                fields.updated_at,
            ],
        )?;
    }
    for obl in &archived.obligations {
        conn.execute(
            "INSERT OR IGNORE INTO node_obligations (id, node_id, kind, ordinal, section, body, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                uuid_to_blob(obl.id),
                blob,
                obl.kind,
                obl.ordinal,
                obl.section,
                obl.body,
                obl.created_at,
                obl.updated_at,
            ],
        )?;
    }
    for cap_arch in &archived.capability_archives {
        conn.execute(
            "INSERT OR IGNORE INTO capability_archives (id, node_id, capability, archived_at, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                uuid_to_blob(cap_arch.id),
                blob,
                cap_arch.capability,
                cap_arch.archived_at,
                cap_arch.payload,
            ],
        )?;
    }
    Ok(())
}

fn depth_sort(nodes: &[ArchivedNode]) -> Vec<&ArchivedNode> {
    let parent_map: HashMap<Uuid, Option<Uuid>> = nodes
        .iter()
        .map(|n| (n.id, n.outline.parent_id))
        .collect();
    let depth = |id: Uuid| -> usize {
        let mut d = 0;
        let mut current = parent_map.get(&id).copied().flatten();
        while let Some(pid) = current {
            d += 1;
            current = parent_map.get(&pid).copied().flatten();
        }
        d
    };
    let mut sorted: Vec<&ArchivedNode> = nodes.iter().collect();
    sorted.sort_by_key(|n| depth(n.id));
    sorted
}

pub fn collect_subtree_ids(
    outline: &OutlineRepo<'_>,
    list_id: Uuid,
    root_id: Uuid,
) -> Result<Vec<Uuid>> {
    let entries = outline.list_for_list(list_id)?;
    let mut ids = vec![root_id];
    let mut queue = vec![root_id];
    while let Some(parent) = queue.pop() {
        for entry in &entries {
            if entry.parent_id == Some(parent) && !ids.contains(&entry.node_id) {
                ids.push(entry.node_id);
                queue.push(entry.node_id);
            }
        }
    }
    Ok(ids)
}

fn subtree_delete_order(
    outline: &OutlineRepo<'_>,
    list_id: Uuid,
    subtree: &[Uuid],
) -> Result<Vec<Uuid>> {
    let entries = outline.list_for_list(list_id)?;
    let depth_of = |id: Uuid| -> usize {
        let mut depth = 0usize;
        let mut current = Some(id);
        while let Some(node_id) = current {
            let Some(entry) = entries.iter().find(|e| e.node_id == node_id) else {
                break;
            };
            if let Some(parent) = entry.parent_id {
                depth += 1;
                current = Some(parent);
            } else {
                break;
            }
        }
        depth
    };
    let mut ordered = subtree.to_vec();
    ordered.sort_by_key(|id| std::cmp::Reverse(depth_of(*id)));
    Ok(ordered)
}

/// Human-readable label for a delete command entry.
pub fn delete_label(archive: &NodeSubtreeArchive) -> String {
    let root_title = archive
        .nodes
        .iter()
        .find(|n| n.id == archive.root_node_id)
        .map(|n| n.title.as_str())
        .unwrap_or("node");
    let child_count = archive.nodes.len().saturating_sub(1);
    if child_count == 0 {
        format!("Deleted \"{root_title}\"")
    } else {
        format!("Deleted \"{root_title}\" (+ {child_count} children)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::repos::ListRepo;
    use crate::outline::types::OutlineEntry;

    fn temp_conn() -> (std::path::PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("tod-archive-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.db");
        let conn = crate::fleet::schema::open_writer_connection(&path).unwrap();
        (dir, conn)
    }

    #[test]
    fn archive_delete_restore_roundtrip() {
        let (dir, conn) = temp_conn();
        let list = ListRepo::new(&conn).create("test", "Test").unwrap();
        let parent_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();
        NodeRepo::new(&conn)
            .create_with_id(parent_id, "parent", "Parent")
            .unwrap();
        NodeRepo::new(&conn)
            .create_with_id(child_id, "child", "Child")
            .unwrap();
        OutlineRepo::new(&conn)
            .insert(&OutlineEntry {
                node_id: parent_id,
                list_id: list.id,
                parent_id: None,
                ordinal: 0,
                collapsed: false,
            })
            .unwrap();
        OutlineRepo::new(&conn)
            .insert(&OutlineEntry {
                node_id: child_id,
                list_id: list.id,
                parent_id: Some(parent_id),
                ordinal: 0,
                collapsed: false,
            })
            .unwrap();

        let archive_id = delete_subtree_archived(&conn, parent_id).unwrap().0;
        assert!(NodeRepo::new(&conn).get(parent_id).unwrap().is_none());

        restore_subtree(&conn, archive_id, &dir).unwrap();
        assert!(NodeRepo::new(&conn).get(parent_id).unwrap().is_some());
        assert!(NodeRepo::new(&conn).get(child_id).unwrap().is_some());

        let _ = std::fs::remove_dir_all(dir);
    }
}
