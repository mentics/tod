//! Archive snapshots for subtree delete / restore.

use crate::outline::repos::{NodeRepo, OutlineRepo};
use crate::outline::types::Capability;
use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
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
    pub created_at: i64,
    pub updated_at: i64,
    pub outline: ArchivedOutlineEntry,
    pub capabilities: Vec<String>,
    pub lifecycle: Option<String>,
    pub fields: Option<ArchivedFields>,
    #[serde(default)]
    pub tags: Option<ArchivedTags>,
    pub obligations: Vec<ArchivedObligation>,
    pub capability_archives: Vec<ArchivedCapabilityArchive>,
    #[serde(default)]
    pub files: Option<ArchivedNodeFiles>,
    #[serde(default)]
    pub agent: Option<ArchivedNodeAgent>,
    /// Archives written before plan steps were archived have none.
    #[serde(default)]
    pub plan_steps: Vec<ArchivedPlanStep>,
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
    pub linked_issues: String,
    pub linked_prs: String,
    pub updated_at: i64,
}

/// Files capability row (`node_files`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedNodeFiles {
    pub use_worktree: bool,
    pub worktree_path: Option<String>,
    pub worktree_lease_id: Option<String>,
    pub worktree_lease_holder: Option<String>,
    pub updated_at: i64,
}

/// Agent capability row (`node_agent`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedNodeAgent {
    pub platform: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedTags {
    pub tags: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedObligation {
    pub id: Uuid,
    pub kind: String,
    pub ordinal: i32,
    pub section: Option<String>,
    pub body: String,
    #[serde(default = "default_archived_phase")]
    pub phase: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Archives written before phase-tagging existed deserialize their obligations
/// as `unknown` rather than failing.
fn default_archived_phase() -> String {
    crate::interview::PHASE_UNKNOWN.to_string()
}

/// A plan step with its outgoing dependencies and obligation links. Edges
/// whose other end is gone at restore time are skipped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedPlanStep {
    pub id: Uuid,
    pub ordinal: i32,
    pub body: String,
    pub status: String,
    #[serde(default)]
    pub note: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub depends_on: Vec<Uuid>,
    #[serde(default)]
    pub satisfies: Vec<Uuid>,
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
    // Edges last: a step may depend on a step (or satisfy an obligation) of
    // another node in the same subtree.
    for node in &depth_order {
        restore_plan_step_edges(conn, node)?;
    }
    conn.execute(
        "DELETE FROM node_subtree_archives WHERE id = ?1",
        params![uuid_to_blob(archive_id)],
    )?;
    Ok(list_id)
}

fn truncate_title(title: &str, max_chars: usize) -> String {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let truncated: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}…")
}

fn node_label(conn: &Connection, node_id: Uuid) -> String {
    let title = NodeRepo::new(conn)
        .get(node_id)
        .ok()
        .flatten()
        .map(|node| truncate_title(&node.title, 40))
        .unwrap_or_default();
    if title.is_empty() {
        "this node".into()
    } else {
        format!("node \"{title}\"")
    }
}

fn validate_delete(conn: &Connection, root_id: Uuid) -> Result<()> {
    let outline = OutlineRepo::new(conn);
    let entry = outline
        .get_entry(root_id)?
        .context("node missing from outline")?;
    let list_id = entry.list_id;
    let subtree = collect_subtree_ids(&outline, list_id, root_id)?;
    for node_id in &subtree {
        let running: i64 = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM agent_runs
                     WHERE node_id = ?1 AND ended_at IS NULL AND runtime_status != 'not_running')
                  + (SELECT COUNT(*) FROM shell_sessions WHERE node_id = ?1)",
            params![uuid_to_blob(*node_id)],
            |row| row.get(0),
        )?;
        if running > 0 {
            let label = node_label(conn, root_id);
            anyhow::bail!("{label} has running agents or open shells — stop them before deleting");
        }
    }
    Ok(())
}

fn snapshot_fields(conn: &Connection, node_id: Uuid) -> Result<Option<ArchivedFields>> {
    conn.query_row(
        "SELECT repo, branch, notes, linked_issues, linked_prs, updated_at
         FROM node_fields WHERE node_id = ?1",
        params![uuid_to_blob(node_id)],
        |row| {
            Ok(ArchivedFields {
                repo: row.get(0)?,
                branch: row.get(1)?,
                notes: row.get(2)?,
                linked_issues: row.get(3)?,
                linked_prs: row.get(4)?,
                updated_at: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn snapshot_node_files(conn: &Connection, node_id: Uuid) -> Result<Option<ArchivedNodeFiles>> {
    conn.query_row(
        "SELECT use_worktree, worktree_path, worktree_lease_id, worktree_lease_holder, updated_at
         FROM node_files WHERE node_id = ?1",
        params![uuid_to_blob(node_id)],
        |row| {
            Ok(ArchivedNodeFiles {
                use_worktree: row.get::<_, i64>(0)? != 0,
                worktree_path: row.get(1)?,
                worktree_lease_id: row.get(2)?,
                worktree_lease_holder: row.get(3)?,
                updated_at: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn snapshot_node_agent(conn: &Connection, node_id: Uuid) -> Result<Option<ArchivedNodeAgent>> {
    conn.query_row(
        "SELECT platform, model, effort, updated_at FROM node_agent WHERE node_id = ?1",
        params![uuid_to_blob(node_id)],
        |row| {
            Ok(ArchivedNodeAgent {
                platform: row.get(0)?,
                model: row.get(1)?,
                effort: row.get(2)?,
                updated_at: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn snapshot_node(conn: &Connection, node_id: Uuid) -> Result<ArchivedNode> {
    let node = NodeRepo::new(conn).get(node_id)?.context("node missing")?;
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
    let fields = snapshot_fields(conn, node_id)?;
    let tags = conn
        .query_row(
            "SELECT tags, updated_at FROM node_tags WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
            |row| {
                Ok(ArchivedTags {
                    tags: row.get(0)?,
                    updated_at: row.get(1)?,
                })
            },
        )
        .optional()?;
    let mut obligations = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, kind, ordinal, section, body, phase, created_at, updated_at
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
                phase: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        for row in rows {
            obligations.push(row?);
        }
    }
    let plan_steps = snapshot_plan_steps(conn, node_id)?;
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
        tags,
        obligations,
        capability_archives,
        files: snapshot_node_files(conn, node_id)?,
        agent: snapshot_node_agent(conn, node_id)?,
        plan_steps,
    })
}

fn snapshot_plan_steps(conn: &Connection, node_id: Uuid) -> Result<Vec<ArchivedPlanStep>> {
    let repo = crate::outline::repos::PlanStepRepo::new(conn);
    let mut steps = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, ordinal, body, status, created_at, updated_at, note
             FROM node_plan_steps WHERE node_id = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map(params![uuid_to_blob(node_id)], |row| {
            let id_blob: Vec<u8> = row.get(0)?;
            Ok(ArchivedPlanStep {
                id: blob_to_uuid_sql(&id_blob)?,
                ordinal: row.get(1)?,
                body: row.get(2)?,
                status: row.get(3)?,
                note: row.get(6)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
                depends_on: Vec::new(),
                satisfies: Vec::new(),
            })
        })?;
        for row in rows {
            steps.push(row?);
        }
    }
    for step in &mut steps {
        step.depends_on = repo.list_dependencies(step.id)?;
        step.satisfies = repo.list_obligations(step.id)?;
    }
    Ok(steps)
}

fn restore_plan_step_edges(conn: &Connection, archived: &ArchivedNode) -> Result<()> {
    for step in &archived.plan_steps {
        let from = uuid_to_blob(step.id);
        for dep in &step.depends_on {
            conn.execute(
                "INSERT OR IGNORE INTO node_plan_step_deps (step_id, depends_on_step_id)
                 SELECT ?1, id FROM node_plan_steps WHERE id = ?2",
                params![from, uuid_to_blob(*dep)],
            )?;
        }
        for obligation in &step.satisfies {
            conn.execute(
                "INSERT OR IGNORE INTO node_plan_step_obligations (step_id, obligation_id)
                 SELECT ?1, id FROM node_obligations WHERE id = ?2",
                params![from, uuid_to_blob(*obligation)],
            )?;
        }
    }
    Ok(())
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
            serde_json::json!({ "agent": snapshot_node_agent(conn, node_id)? })
        }
        Capability::Files => {
            let fields = snapshot_fields(conn, node_id)?;
            serde_json::json!({
                "repo": fields.as_ref().and_then(|f| f.repo.clone()),
                "branch": fields.as_ref().and_then(|f| f.branch.clone()),
                "files": snapshot_node_files(conn, node_id)?,
            })
        }
        Capability::Ticket => {
            let fields = snapshot_fields(conn, node_id)?;
            serde_json::json!({
                "linked_issues": fields.as_ref().map(|f| f.linked_issues.clone()),
                "linked_prs": fields.as_ref().map(|f| f.linked_prs.clone()),
            })
        }
        Capability::Tags => {
            let tags = conn
                .query_row(
                    "SELECT tags, updated_at FROM node_tags WHERE node_id = ?1",
                    params![uuid_to_blob(node_id)],
                    |row| {
                        Ok(ArchivedTags {
                            tags: row.get(0)?,
                            updated_at: row.get(1)?,
                        })
                    },
                )
                .optional()?;
            serde_json::json!({ "tags": tags })
        }
        Capability::Generator => {
            // Generator config archival will be implemented with the generator schema tables.
            serde_json::json!({ "generator": {} })
        }
    };
    serde_json::to_string(&payload).context("serialize capability archive")
}

fn snapshot_obligations(conn: &Connection, node_id: Uuid) -> Result<Vec<ArchivedObligation>> {
    let mut obligations = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, kind, ordinal, section, body, phase, created_at, updated_at
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
            phase: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    for row in rows {
        obligations.push(row?);
    }
    Ok(obligations)
}

fn restore_node(conn: &Connection, archived: &ArchivedNode) -> Result<()> {
    let blob = uuid_to_blob(archived.id);
    conn.execute(
        "INSERT OR IGNORE INTO nodes (id, slug, title, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            blob,
            archived.slug,
            archived.title,
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
            "INSERT OR IGNORE INTO node_fields (node_id, repo, branch, notes, linked_issues, linked_prs, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                blob,
                fields.repo,
                fields.branch,
                fields.notes,
                fields.linked_issues,
                fields.linked_prs,
                fields.updated_at,
            ],
        )?;
    }
    if let Some(tags) = &archived.tags {
        conn.execute(
            "INSERT OR IGNORE INTO node_tags (node_id, tags, updated_at) VALUES (?1, ?2, ?3)",
            params![blob, tags.tags, tags.updated_at],
        )?;
    }
    if let Some(files) = &archived.files {
        conn.execute(
            "INSERT OR IGNORE INTO node_files
               (node_id, use_worktree, worktree_path, worktree_lease_id, worktree_lease_holder, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                blob,
                i32::from(files.use_worktree),
                files.worktree_path,
                files.worktree_lease_id,
                files.worktree_lease_holder,
                files.updated_at,
            ],
        )?;
    }
    if let Some(agent) = &archived.agent {
        conn.execute(
            "INSERT OR IGNORE INTO node_agent (node_id, platform, model, effort, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                blob,
                agent.platform,
                agent.model,
                agent.effort,
                agent.updated_at
            ],
        )?;
    }
    for obl in &archived.obligations {
        conn.execute(
            "INSERT OR IGNORE INTO node_obligations (id, node_id, kind, ordinal, section, body, phase, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                uuid_to_blob(obl.id),
                blob,
                obl.kind,
                obl.ordinal,
                obl.section,
                obl.body,
                obl.phase,
                obl.created_at,
                obl.updated_at,
            ],
        )?;
    }
    for step in &archived.plan_steps {
        conn.execute(
            "INSERT OR IGNORE INTO node_plan_steps
                (id, node_id, ordinal, body, status, note, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                uuid_to_blob(step.id),
                blob,
                step.ordinal,
                step.body,
                step.status,
                step.note,
                step.created_at,
                step.updated_at,
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
    let parent_map: HashMap<Uuid, Option<Uuid>> =
        nodes.iter().map(|n| (n.id, n.outline.parent_id)).collect();
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

        NodeRepo::new(&conn)
            .enable_capabilities(child_id, &[Capability::Agent, Capability::Files])
            .unwrap();
        let child = child_id.to_string();
        crate::fleet::repos::node_files::NodeFilesRepo::new(&conn)
            .update_worktree(&child, Some("/wt/child"), None, None)
            .unwrap();
        crate::fleet::repos::node_agent::NodeAgentRepo::new(&conn)
            .upsert(&child, Some("cursor"), None, Some("high"))
            .unwrap();

        let archive_id = delete_subtree_archived(&conn, parent_id).unwrap().0;
        assert!(NodeRepo::new(&conn).get(parent_id).unwrap().is_none());
        assert!(
            crate::fleet::repos::node_files::NodeFilesRepo::new(&conn)
                .get(&child)
                .unwrap()
                .is_none()
        );

        restore_subtree(&conn, archive_id, &dir).unwrap();
        assert!(NodeRepo::new(&conn).get(parent_id).unwrap().is_some());
        assert!(NodeRepo::new(&conn).get(child_id).unwrap().is_some());
        let files = crate::fleet::repos::node_files::NodeFilesRepo::new(&conn)
            .get(&child)
            .unwrap()
            .expect("files restored");
        assert_eq!(files.worktree_path(), Some("/wt/child"));
        let agent = crate::fleet::repos::node_agent::NodeAgentRepo::new(&conn)
            .get(&child)
            .unwrap()
            .expect("agent restored");
        assert_eq!(agent.platform.as_deref(), Some("cursor"));
        assert_eq!(agent.effort.as_deref(), Some("high"));

        let _ = std::fs::remove_dir_all(dir);
    }
}
