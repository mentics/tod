//! Load flattened visible tree rows for UI.

use crate::outline::types::{Capability, FlatNodeRow, Node, OutlineEntry};
use crate::outline::uuid_blob::{blob_to_uuid_sql, ms_to_datetime, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub struct TreeLoader<'a> {
    conn: &'a Connection,
}

impl<'a> TreeLoader<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn flatten_visible(&self, list_id: Uuid) -> Result<Vec<FlatNodeRow>> {
        let entries = crate::outline::repos::OutlineRepo::new(self.conn).list_for_list(list_id)?;
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let by_parent = group_by_parent(&entries);
        let data = TreeData::load(self.conn, list_id)?;
        let mut out = Vec::new();
        walk(&by_parent, &data, None, 0, &mut out);
        Ok(out)
    }
}

/// Everything the flattened rows need, loaded once per list rather than once
/// per node.
///
/// The per-node repo accessors cost a prepared statement each, so a node used
/// to run seven to ten of them; a few hundred rows then took hundreds of
/// milliseconds on the UI thread. Each field below is one query scoped to the
/// list, and the walk is pure map lookups.
struct TreeData {
    nodes: HashMap<Uuid, Node>,
    managed: HashSet<Uuid>,
    capabilities: HashMap<Uuid, Vec<Capability>>,
    lifecycle: HashMap<Uuid, String>,
    tags: HashMap<Uuid, Vec<String>>,
    ticket_ids: HashMap<Uuid, String>,
    /// Link on the node itself: (external id, source type, generator node id).
    links: HashMap<Uuid, (String, String, Uuid)>,
    /// How many nodes each generator node owns. Not scoped to the list: a
    /// generator's count is of everything it produced.
    managed_counts: HashMap<Uuid, usize>,
    /// Generator node → (last refresh status, last refresh error).
    generator_status: HashMap<Uuid, (Option<String>, Option<String>)>,
    /// Generator node → quick-accept destination, when configured.
    accept_destinations: HashMap<Uuid, Uuid>,
}

impl TreeData {
    fn load(conn: &Connection, list_id: Uuid) -> Result<Self> {
        let list_blob = uuid_to_blob(list_id);

        // Every query joins the list's outline entries so a big archive of
        // other lists' nodes costs nothing here.
        let mut stmt = conn.prepare(
            "SELECT n.id, n.slug, n.title, n.created_at, n.updated_at, n.managed
             FROM nodes n JOIN outline_entries e ON e.node_id = n.id
             WHERE e.list_id = ?1",
        )?;
        let mut nodes = HashMap::new();
        let mut managed = HashSet::new();
        let mut rows = stmt.query(params![list_blob])?;
        while let Some(row) = rows.next()? {
            let id_blob: Vec<u8> = row.get(0)?;
            let id = blob_to_uuid_sql(&id_blob)?;
            nodes.insert(
                id,
                Node {
                    id,
                    slug: row.get(1)?,
                    title: row.get(2)?,
                    created_at: ms_to_datetime(row.get(3)?),
                    updated_at: ms_to_datetime(row.get(4)?),
                },
            );
            if row.get::<_, i32>(5)? != 0 {
                managed.insert(id);
            }
        }
        drop(rows);
        drop(stmt);

        let mut capabilities: HashMap<Uuid, Vec<Capability>> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT c.node_id, c.capability
             FROM node_capabilities c JOIN outline_entries e ON e.node_id = c.node_id
             WHERE e.list_id = ?1
             ORDER BY c.capability",
        )?;
        let mut rows = stmt.query(params![list_blob])?;
        while let Some(row) = rows.next()? {
            let id_blob: Vec<u8> = row.get(0)?;
            let raw: String = row.get(1)?;
            capabilities
                .entry(blob_to_uuid_sql(&id_blob)?)
                .or_default()
                .push(Capability::parse(&raw).unwrap_or(Capability::Spec));
        }
        drop(rows);
        drop(stmt);

        let lifecycle = string_map(
            conn,
            &list_blob,
            "SELECT l.node_id, l.state
             FROM node_lifecycle l JOIN outline_entries e ON e.node_id = l.node_id
             WHERE e.list_id = ?1",
        )?;

        let tags = string_map(
            conn,
            &list_blob,
            "SELECT t.node_id, t.tags
             FROM node_tags t JOIN outline_entries e ON e.node_id = t.node_id
             WHERE e.list_id = ?1",
        )?
        .into_iter()
        .map(|(id, raw)| {
            (
                id,
                serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default(),
            )
        })
        .collect();

        // A node's ticket id is the first of its linked issues.
        let ticket_ids = string_map(
            conn,
            &list_blob,
            "SELECT f.node_id, f.linked_issues
             FROM node_fields f JOIN outline_entries e ON e.node_id = f.node_id
             WHERE e.list_id = ?1",
        )?
        .into_iter()
        .filter_map(|(id, raw)| {
            serde_json::from_str::<Vec<String>>(&raw)
                .ok()?
                .into_iter()
                .next()
                .map(|ticket| (id, ticket))
        })
        .collect();

        let mut links = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT l.node_id, l.external_id, l.source_type, l.generator_node_id
             FROM managed_node_links l JOIN outline_entries e ON e.node_id = l.node_id
             WHERE e.list_id = ?1",
        )?;
        let mut rows = stmt.query(params![list_blob])?;
        while let Some(row) = rows.next()? {
            let id_blob: Vec<u8> = row.get(0)?;
            let gen_blob: Vec<u8> = row.get(3)?;
            links.insert(
                blob_to_uuid_sql(&id_blob)?,
                (row.get(1)?, row.get(2)?, blob_to_uuid_sql(&gen_blob)?),
            );
        }
        drop(rows);
        drop(stmt);

        let mut accept_destinations = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT node_id, accept_destination_node_id FROM node_generator_config
             WHERE accept_destination_node_id IS NOT NULL",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id_blob: Vec<u8> = row.get(0)?;
            let dest_blob: Vec<u8> = row.get(1)?;
            accept_destinations.insert(blob_to_uuid_sql(&id_blob)?, blob_to_uuid_sql(&dest_blob)?);
        }
        drop(rows);
        drop(stmt);

        let mut managed_counts = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT generator_node_id, COUNT(*) FROM managed_node_links
             GROUP BY generator_node_id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id_blob: Vec<u8> = row.get(0)?;
            let count: i64 = row.get(1)?;
            managed_counts.insert(blob_to_uuid_sql(&id_blob)?, count as usize);
        }
        drop(rows);
        drop(stmt);

        let mut generator_status = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT g.node_id, g.last_refresh_status, g.last_refresh_error
             FROM node_generator_config g JOIN outline_entries e ON e.node_id = g.node_id
             WHERE e.list_id = ?1",
        )?;
        let mut rows = stmt.query(params![list_blob])?;
        while let Some(row) = rows.next()? {
            let id_blob: Vec<u8> = row.get(0)?;
            generator_status.insert(blob_to_uuid_sql(&id_blob)?, (row.get(1)?, row.get(2)?));
        }

        Ok(Self {
            nodes,
            managed,
            capabilities,
            lifecycle,
            tags,
            ticket_ids,
            links,
            managed_counts,
            generator_status,
            accept_destinations,
        })
    }
}

/// Collect a `(node_id blob, text)` query into a map.
fn string_map(
    conn: &Connection,
    list_blob: &[u8],
    sql: &str,
) -> rusqlite::Result<HashMap<Uuid, String>> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params![list_blob])?;
    let mut map = HashMap::new();
    while let Some(row) = rows.next()? {
        let id_blob: Vec<u8> = row.get(0)?;
        map.insert(blob_to_uuid_sql(&id_blob)?, row.get(1)?);
    }
    Ok(map)
}

fn walk(
    by_parent: &HashMap<Option<Uuid>, Vec<&OutlineEntry>>,
    data: &TreeData,
    parent_id: Option<Uuid>,
    depth: usize,
    out: &mut Vec<FlatNodeRow>,
) {
    let Some(children) = by_parent.get(&parent_id) else {
        return;
    };
    let mut children = children.clone();
    children.sort_by_key(|e| e.ordinal);

    for entry in children {
        let Some(node) = data.nodes.get(&entry.node_id).cloned() else {
            continue;
        };
        let capabilities = data
            .capabilities
            .get(&entry.node_id)
            .cloned()
            .unwrap_or_default();
        let has_children = by_parent
            .get(&Some(entry.node_id))
            .map(|c| !c.is_empty())
            .unwrap_or(false);
        let managed = data.managed.contains(&entry.node_id);
        let link = if managed {
            data.links.get(&entry.node_id)
        } else {
            None
        };
        let (managed_count, generator_status, generator_error) =
            if capabilities.contains(&Capability::Generator) {
                let (status, error) = data
                    .generator_status
                    .get(&entry.node_id)
                    .cloned()
                    .unwrap_or((None, None));
                (
                    Some(
                        data.managed_counts
                            .get(&entry.node_id)
                            .copied()
                            .unwrap_or(0),
                    ),
                    status,
                    error,
                )
            } else {
                (None, None, None)
            };
        let accept_ready = link
            .map(|(_, _, generator_node_id)| data.accept_destinations.contains_key(generator_node_id))
            .unwrap_or(false);
        out.push(FlatNodeRow {
            node,
            depth,
            parent_id: entry.parent_id,
            capabilities,
            lifecycle: data.lifecycle.get(&entry.node_id).cloned(),
            tags: data.tags.get(&entry.node_id).cloned().unwrap_or_default(),
            ticket_id: data.ticket_ids.get(&entry.node_id).cloned(),
            tree_ordinal: out.len(),
            collapsed: entry.collapsed,
            has_children,
            managed,
            external_id: link.map(|(external_id, _, _)| external_id.clone()),
            source_type: link.map(|(_, source_type, _)| source_type.clone()),
            managed_count,
            generator_status,
            generator_error,
            accept_ready,
        });
        if !entry.collapsed {
            walk(by_parent, data, Some(entry.node_id), depth + 1, out);
        }
    }
}

fn group_by_parent(entries: &[OutlineEntry]) -> HashMap<Option<Uuid>, Vec<&OutlineEntry>> {
    let mut map: HashMap<Option<Uuid>, Vec<&OutlineEntry>> = HashMap::new();
    for entry in entries {
        map.entry(entry.parent_id).or_default().push(entry);
    }
    map
}
