//! Load flattened visible tree rows for UI.

use crate::outline::repos::{NodeRepo, OutlineRepo};
use crate::outline::types::{FlatNodeRow, OutlineEntry};
use crate::outline::uuid_blob::uuid_to_blob;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use uuid::Uuid;

pub struct TreeLoader<'a> {
    conn: &'a Connection,
}

impl<'a> TreeLoader<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn flatten_visible(&self, list_id: Uuid) -> Result<Vec<FlatNodeRow>> {
        let outline = OutlineRepo::new(self.conn);
        let node_repo = NodeRepo::new(self.conn);
        let entries = outline.list_for_list(list_id)?;
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let by_parent = group_by_parent(&entries);
        let mut out = Vec::new();
        self.walk(&by_parent, &node_repo, None, 0, &mut out)?;
        Ok(out)
    }

    fn walk(
        &self,
        by_parent: &HashMap<Option<Uuid>, Vec<&OutlineEntry>>,
        node_repo: &NodeRepo<'_>,
        parent_id: Option<Uuid>,
        depth: usize,
        out: &mut Vec<FlatNodeRow>,
    ) -> Result<()> {
        let Some(children) = by_parent.get(&parent_id) else {
            return Ok(());
        };
        let mut children = children.clone();
        children.sort_by_key(|e| e.ordinal);

        for entry in children {
            let Some(node) = node_repo.get(entry.node_id)? else {
                continue;
            };
            let capabilities = node_repo.list_capabilities(entry.node_id)?;
            let lifecycle = node_repo.get_lifecycle(entry.node_id)?;
            let tags = node_repo.get_tags(entry.node_id)?;
            let ticket_id = node_repo.get_ticket_id(entry.node_id)?;
            let has_children = by_parent
                .get(&Some(entry.node_id))
                .map(|c| !c.is_empty())
                .unwrap_or(false);
            out.push(FlatNodeRow {
                node,
                depth,
                parent_id: entry.parent_id,
                capabilities,
                lifecycle,
                tags,
                ticket_id,
                tree_ordinal: out.len(),
                collapsed: entry.collapsed,
                has_children,
            });
            if !entry.collapsed {
                self.walk(by_parent, node_repo, Some(entry.node_id), depth + 1, out)?;
            }
        }
        Ok(())
    }

    pub fn detect_reference_loop(&self, list_id: Uuid) -> Result<Option<Vec<Uuid>>> {
        let outline = OutlineRepo::new(self.conn);
        let node_repo = NodeRepo::new(self.conn);
        let entries = outline.list_for_list(list_id)?;
        for entry in &entries {
            let Some(node) = node_repo.get(entry.node_id)? else {
                continue;
            };
            if node.kind != crate::outline::types::NodeKind::Reference {
                continue;
            }
            if let Some(target) = node.ref_target_id {
                let mut visited = std::collections::HashSet::new();
                if self.ref_walk_has_cycle(target, &node_repo, &mut visited) {
                    return Ok(Some(visited.into_iter().collect()));
                }
            }
        }
        Ok(None)
    }

    fn ref_walk_has_cycle(
        &self,
        node_id: Uuid,
        node_repo: &NodeRepo<'_>,
        visited: &mut std::collections::HashSet<Uuid>,
    ) -> bool {
        if !visited.insert(node_id) {
            return true;
        }
        let Ok(Some(node)) = node_repo.get(node_id) else {
            return false;
        };
        if node.kind == crate::outline::types::NodeKind::Reference {
            if let Some(target) = node.ref_target_id {
                return self.ref_walk_has_cycle(target, node_repo, visited);
            }
        }
        false
    }

    pub fn record_loop_issue(&self, list_id: Uuid, cycle: &[Uuid]) -> Result<()> {
        let detail =
            serde_json::to_string(&cycle.iter().map(|id| id.to_string()).collect::<Vec<_>>())?;
        self.conn.execute(
            "INSERT INTO list_health_issues (id, list_id, issue_type, detail, detected_at, cleared_at)
             VALUES (?1, ?2, 'reference_loop', ?3, ?4, NULL)",
            rusqlite::params![
                uuid_to_blob(Uuid::new_v4()),
                uuid_to_blob(list_id),
                detail,
                crate::outline::uuid_blob::now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn list_has_open_loop(&self, list_id: Uuid) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM list_health_issues WHERE list_id = ?1 AND issue_type = 'reference_loop' AND cleared_at IS NULL",
            rusqlite::params![uuid_to_blob(list_id)],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }
}

fn group_by_parent(entries: &[OutlineEntry]) -> HashMap<Option<Uuid>, Vec<&OutlineEntry>> {
    let mut map: HashMap<Option<Uuid>, Vec<&OutlineEntry>> = HashMap::new();
    for entry in entries {
        map.entry(entry.parent_id).or_default().push(entry);
    }
    map
}
