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
        self.flatten(list_id, false)
    }

    /// Every node in tree order, including those under collapsed parents.
    /// Rows carry their `collapsed` flag so a caller can hide what they hold.
    pub fn flatten_all(&self, list_id: Uuid) -> Result<Vec<FlatNodeRow>> {
        self.flatten(list_id, true)
    }

    fn flatten(&self, list_id: Uuid, include_collapsed: bool) -> Result<Vec<FlatNodeRow>> {
        let entries = crate::outline::repos::OutlineRepo::new(self.conn).list_for_list(list_id)?;
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let by_parent = group_by_parent(&entries);
        let mut data = TreeData::load(self.conn, list_id)?;
        data.count_managed(&by_parent, None, None);
        let mut out = Vec::new();
        walk(&by_parent, &data, None, None, 0, include_collapsed, &mut out);
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
    /// Each node's ticket. A managed node's is its external id.
    ticket_ids: HashMap<Uuid, String>,
    /// How many managed nodes each generator node has under it (managed
    /// nodes sit in their generator's list, so this is complete).
    managed_counts: HashMap<Uuid, usize>,
    /// Generator node → its data source type.
    source_types: HashMap<Uuid, String>,
    /// Generator node → (last refresh status, last refresh error).
    generator_status: HashMap<Uuid, (Option<String>, Option<String>)>,
    /// Generator node → quick-accept destination, when configured.
    accept_destinations: HashMap<Uuid, Uuid>,
    /// Tickets some non-managed node is. A managed node with one of these
    /// `has_copies`, whichever generator it came from: it was accepted. Read
    /// live from `node_fields.ticket`, so it clears as soon as the last such
    /// node is deleted. Not scoped to the list: a copy may live anywhere.
    copied: HashSet<String>,
    /// Tickets some managed node is: a non-managed node with one of these is
    /// a linked copy, which refreshes update. Not scoped to the list either.
    generated: HashSet<String>,
    /// Nodes whose ticket is in a terminal state, from their synced metadata.
    ticket_terminal: HashSet<Uuid>,
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

        let ticket_ids = string_map(
            conn,
            &list_blob,
            "SELECT f.node_id, f.ticket
             FROM node_fields f JOIN outline_entries e ON e.node_id = f.node_id
             WHERE e.list_id = ?1 AND f.ticket IS NOT NULL",
        )?;

        let mut accept_destinations = HashMap::new();
        let mut source_types = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT node_id, data_source_type, accept_destination_node_id
             FROM node_generator_config",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id_blob: Vec<u8> = row.get(0)?;
            let id = blob_to_uuid_sql(&id_blob)?;
            source_types.insert(id, row.get(1)?);
            if let Some(dest_blob) = row.get::<_, Option<Vec<u8>>>(2)? {
                accept_destinations.insert(id, blob_to_uuid_sql(&dest_blob)?);
            }
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
        drop(rows);
        drop(stmt);

        let mut copied = HashSet::new();
        let mut generated = HashSet::new();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT f.ticket, n.managed
             FROM node_fields f JOIN nodes n ON n.id = f.node_id
             WHERE f.ticket IS NOT NULL",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let ticket: String = row.get(0)?;
            if row.get::<_, i32>(1)? != 0 {
                generated.insert(ticket);
            } else {
                copied.insert(ticket);
            }
        }
        drop(rows);
        drop(stmt);

        let ticket_terminal = string_map(
            conn,
            &list_blob,
            "SELECT x.node_id, x.body
             FROM node_extra_content x JOIN outline_entries e ON e.node_id = x.node_id
             WHERE e.list_id = ?1 AND x.content_type = 'metadata'",
        )?
        .into_iter()
        .filter(|(_, body)| {
            serde_json::from_str::<serde_json::Value>(body)
                .is_ok_and(|meta| crate::outline::types::ticket_is_terminal(&meta))
        })
        .map(|(id, _)| id)
        .collect();

        Ok(Self {
            nodes,
            managed,
            capabilities,
            lifecycle,
            tags,
            ticket_ids,
            managed_counts: HashMap::new(),
            source_types,
            generator_status,
            accept_destinations,
            copied,
            generated,
            ticket_terminal,
        })
    }

    /// Count each generator's managed nodes: every managed node below it,
    /// collapsed or not, down to the next generator.
    fn count_managed(
        &mut self,
        by_parent: &HashMap<Option<Uuid>, Vec<&OutlineEntry>>,
        parent_id: Option<Uuid>,
        generator: Option<Uuid>,
    ) {
        let Some(children) = by_parent.get(&parent_id) else {
            return;
        };
        for entry in children {
            let id = entry.node_id;
            if let Some(generator) = generator.filter(|_| self.managed.contains(&id)) {
                *self.managed_counts.entry(generator).or_default() += 1;
            }
            let generator = if self.is_generator(id) { Some(id) } else { generator };
            self.count_managed(by_parent, Some(id), generator);
        }
    }

    fn is_generator(&self, id: Uuid) -> bool {
        self.capabilities
            .get(&id)
            .is_some_and(|caps| caps.contains(&Capability::Generator))
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

/// `generator` is the nearest generator above `parent_id`'s children: a
/// managed node's source.
fn walk(
    by_parent: &HashMap<Option<Uuid>, Vec<&OutlineEntry>>,
    data: &TreeData,
    parent_id: Option<Uuid>,
    generator: Option<Uuid>,
    depth: usize,
    include_collapsed: bool,
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
        let ticket = data.ticket_ids.get(&entry.node_id);
        // A managed node's link: (external id, source type, generator node id).
        let link = ticket
            .zip(generator)
            .filter(|_| managed)
            .map(|(external_id, generator_node_id)| {
                let source_type = data
                    .source_types
                    .get(&generator_node_id)
                    .cloned()
                    .unwrap_or_default();
                (external_id, source_type, generator_node_id)
            });
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
            .as_ref()
            .is_some_and(|(_, _, generator_node_id)| data.accept_destinations.contains_key(generator_node_id));
        let linked_copy = !managed && ticket.is_some_and(|t| data.generated.contains(t));
        let has_copies = link
            .as_ref()
            .is_some_and(|(external_id, _, _)| data.copied.contains(*external_id));
        out.push(FlatNodeRow {
            node,
            depth,
            parent_id: entry.parent_id,
            capabilities,
            lifecycle: data.lifecycle.get(&entry.node_id).cloned(),
            tags: data.tags.get(&entry.node_id).cloned().unwrap_or_default(),
            // A managed node shows its id as its external id instead.
            ticket_id: if managed { None } else { ticket.cloned() },
            tree_ordinal: out.len(),
            collapsed: entry.collapsed,
            has_children,
            managed,
            external_id: link.as_ref().map(|(external_id, _, _)| (*external_id).clone()),
            source_type: link.as_ref().map(|(_, source_type, _)| source_type.clone()),
            managed_count,
            generator_status,
            generator_error,
            accept_ready,
            linked_copy,
            has_copies,
            ticket_terminal: ticket.is_some() && data.ticket_terminal.contains(&entry.node_id),
        });
        if include_collapsed || !entry.collapsed {
            let generator = if data.is_generator(entry.node_id) {
                Some(entry.node_id)
            } else {
                generator
            };
            walk(
                by_parent,
                data,
                Some(entry.node_id),
                generator,
                depth + 1,
                include_collapsed,
                out,
            );
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
