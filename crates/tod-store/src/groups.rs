//! The Groups capability: a node's children are shown grouped, under headers
//! that are not nodes.
//!
//! Today there is one grouping, by lifecycle state (`by_lifecycle`): the tree
//! shows a header per state that has a child, in lifecycle order, with the
//! children under it. A header exists only while a child is in that state.
//! Grouping needs every child to have a lifecycle state, so while it is on a
//! child without the Lifecycle capability cannot stay: it moves *down* to the
//! first place that accepts it, as the next sibling of the grouped node, then of
//! that node's parent if that groups too, and so on ([`place_valid`]).

use crate::outline::mutations::bump_ordinals_after;
use crate::outline::repos::OutlineRepo;
use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub const CREATE_TABLE: &str = "
CREATE TABLE IF NOT EXISTS node_groups (
    node_id       BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    by_lifecycle  INTEGER NOT NULL DEFAULT 0,
    updated_at    INTEGER NOT NULL
);";

/// Whether `node_id` groups its children by lifecycle state.
pub fn by_lifecycle(conn: &Connection, node_id: Uuid) -> Result<bool> {
    let on: Option<i64> = conn
        .query_row(
            "SELECT g.by_lifecycle FROM node_groups g
             JOIN node_capabilities c ON c.node_id = g.node_id AND c.capability = 'groups'
             WHERE g.node_id = ?1",
            params![uuid_to_blob(node_id)],
            |row| row.get(0),
        )
        .optional()?;
    Ok(on.unwrap_or(0) != 0)
}

/// Turn grouping by lifecycle on or off for `node_id`; turning it on moves
/// the children that cannot be grouped out of the node.
pub fn set_by_lifecycle(conn: &Connection, node_id: Uuid, on: bool) -> Result<()> {
    conn.execute(
        "INSERT INTO node_groups (node_id, by_lifecycle, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(node_id) DO UPDATE SET by_lifecycle = ?2, updated_at = ?3",
        params![uuid_to_blob(node_id), i64::from(on), now_ms()],
    )?;
    if on {
        let children: Vec<Uuid> = {
            let mut stmt = conn.prepare(
                "SELECT node_id FROM outline_entries WHERE parent_id = ?1 ORDER BY ordinal",
            )?;
            stmt.query_map(params![uuid_to_blob(node_id)], |row| {
                let blob: Vec<u8> = row.get(0)?;
                blob_to_uuid_sql(&blob)
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for child in children {
            place_valid(conn, child)?;
        }
    }
    Ok(())
}

fn has_lifecycle(conn: &Connection, node_id: Uuid) -> Result<bool> {
    Ok(conn
        .prepare("SELECT 1 FROM node_capabilities WHERE node_id = ?1 AND capability = 'lifecycle'")?
        .exists(params![uuid_to_blob(node_id)])?)
}

/// Whether `node_id` may be a child of `parent_id`: not while the parent
/// groups by lifecycle and the node has no lifecycle state.
pub fn accepts(conn: &Connection, parent_id: Uuid, node_id: Uuid) -> Result<bool> {
    Ok(!by_lifecycle(conn, parent_id)? || has_lifecycle(conn, node_id)?)
}

/// Move `node_id` down to the first place that accepts it: while its parent
/// does not, it becomes the next sibling of that parent. Returns whether it
/// moved.
pub fn place_valid(conn: &Connection, node_id: Uuid) -> Result<bool> {
    let outline = OutlineRepo::new(conn);
    let mut moved = false;
    loop {
        let Some(entry) = outline.get_entry(node_id)? else { return Ok(moved) };
        let Some(parent) = entry.parent_id else { return Ok(moved) };
        if accepts(conn, parent, node_id)? {
            return Ok(moved);
        }
        let Some(parent_entry) = outline.get_entry(parent)? else { return Ok(moved) };
        let ordinal = parent_entry.ordinal + 1;
        bump_ordinals_after(conn, parent_entry.list_id, parent_entry.parent_id, ordinal)?;
        outline.set_parent(node_id, parent_entry.parent_id, ordinal)?;
        moved = true;
    }
}
