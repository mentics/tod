//! Load flattened visible tree rows for UI.

use crate::outline::repos::generator::GeneratorRepo;
use crate::outline::repos::{NodeRepo, OutlineRepo};
use crate::outline::types::{Capability, FlatNodeRow, OutlineEntry};
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
        let gen_repo = GeneratorRepo::new(self.conn);

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
            let managed = gen_repo.is_managed(entry.node_id)?;
            let link = if managed {
                gen_repo.get_link(entry.node_id)?
            } else {
                None
            };
            let external_id = link.as_ref().map(|l| l.external_id.clone());
            let source_type = link.map(|l| l.source_type);
            let (managed_count, generator_status, generator_error) =
                if capabilities.contains(&Capability::Generator) {
                    let count = gen_repo.links_for_generator(entry.node_id)?.len();
                    let config = gen_repo.get_config(entry.node_id)?;
                    (
                        Some(count),
                        config.as_ref().and_then(|c| c.last_refresh_status.clone()),
                        config.and_then(|c| c.last_refresh_error),
                    )
                } else {
                    (None, None, None)
                };
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
                managed,
                external_id,
                source_type,
                managed_count,
                generator_status,
                generator_error,
            });
            if !entry.collapsed {
                self.walk(by_parent, node_repo, Some(entry.node_id), depth + 1, out)?;
            }
        }
        Ok(())
    }
}

fn group_by_parent(entries: &[OutlineEntry]) -> HashMap<Option<Uuid>, Vec<&OutlineEntry>> {
    let mut map: HashMap<Option<Uuid>, Vec<&OutlineEntry>> = HashMap::new();
    for entry in entries {
        map.entry(entry.parent_id).or_default().push(entry);
    }
    map
}
