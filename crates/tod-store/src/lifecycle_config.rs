//! The Lifecycle config capability: the skills the agent for each lifecycle
//! phase should use.
//!
//! It is its own capability, not part of Lifecycle, because Lifecycle sits on
//! nearly every leaf node while this is meant for one or two high-level nodes
//! and inherited by everything below. A node with the capability holds a map
//! from [`PHASES`] key to an ordered list of skill names (`node_lifecycle_config`,
//! one JSON row). A node's skills for a phase come from the nearest node, from
//! itself up, that lists that phase ([`resolve`]); an explicit empty list
//! means no skills, and stops an ancestor's list applying below.

use crate::outline::uuid_blob::{now_ms, uuid_to_blob};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const CREATE_TABLE: &str = "
CREATE TABLE IF NOT EXISTS node_lifecycle_config (
    node_id     BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    skills      TEXT NOT NULL DEFAULT '{}',
    updated_at  INTEGER NOT NULL
);";

/// The phases an agent works in, and so the keys a config may name: the
/// lifecycle states that have a phase agent, and the protocols that run
/// in the others (`implement`, `verify`, `review`, `fix`, `pr`).
pub const PHASES: [&str; 11] = [
    "proposed",
    "design",
    "planning",
    "implement",
    "verify",
    "review",
    "fix",
    "pr",
    "merged",
    "released",
    "learn",
];

/// Skills by phase key, as stored on one node.
pub type Skills = BTreeMap<String, Vec<String>>;

/// The skills for one phase after inheritance, and the node they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub skills: Vec<String>,
    pub source_node: Uuid,
    pub inherited: bool,
}

/// Whether `phase` is a key a config may name.
pub fn is_phase(phase: &str) -> bool {
    PHASES.contains(&phase)
}

/// The skills defined on `node_id` itself.
pub fn skills(conn: &Connection, node_id: Uuid) -> Result<Skills> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT skills FROM node_lifecycle_config WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
}

/// Replace the skills on `node_id`. Rejects an unknown phase, and trims
/// names, dropping blanks and repeats within a phase.
pub fn set_skills(conn: &Connection, node_id: Uuid, config: &Skills) -> Result<()> {
    let mut clean = Skills::new();
    for (phase, names) in config {
        if !is_phase(phase) {
            bail!("{phase} is not a lifecycle phase ({})", PHASES.join(", "));
        }
        let mut list: Vec<String> = Vec::new();
        for name in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
            if !list.iter().any(|have| have == name) {
                list.push(name.to_string());
            }
        }
        clean.insert(phase.clone(), list);
    }
    conn.execute(
        "INSERT INTO node_lifecycle_config (node_id, skills, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(node_id) DO UPDATE SET skills = excluded.skills, updated_at = excluded.updated_at",
        params![uuid_to_blob(node_id), serde_json::to_string(&clean)?, now_ms()],
    )?;
    Ok(())
}

/// The skills for `phase` on `node_id`: the nearest node, from itself up,
/// that lists the phase wins. `None` when no node does.
pub fn resolve(conn: &Connection, node_id: Uuid, phase: &str) -> Result<Option<Resolved>> {
    // `ancestor_chain` runs from the root down to the node: scan nearest first.
    for id in crate::outline::ancestor_chain(conn, node_id)?.into_iter().rev() {
        if let Some(list) = skills(conn, id)?.remove(phase) {
            return Ok(Some(Resolved { skills: list, source_node: id, inherited: id != node_id }));
        }
    }
    Ok(None)
}
