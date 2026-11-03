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
    updated_at  INTEGER NOT NULL,
    delete_sandbox INTEGER
);";

/// The `delete_sandbox` column (schema v92, idempotent): NULL means this node
/// says nothing and the nearest ancestor that does decides.
pub fn add_delete_sandbox(conn: &Connection) -> Result<()> {
    let has = conn
        .prepare("SELECT 1 FROM pragma_table_info('node_lifecycle_config') WHERE name = 'delete_sandbox'")?
        .exists([])?;
    if !has {
        conn.execute_batch("ALTER TABLE node_lifecycle_config ADD COLUMN delete_sandbox INTEGER")?;
    }
    Ok(())
}

/// The phases an agent works in, and so the keys a config may name: the
/// lifecycle states that have a phase agent, and the protocols that run
/// in the others (`implement`, `verify`, `review`, `fix`, `pr`).
pub const PHASES: [&str; 13] = [
    "proposed",
    "design",
    "planning",
    "implement",
    "verify",
    "review",
    "fix",
    "pr",
    "pr_draft",
    "pr_review",
    "merged",
    "released",
    "learn",
];

/// The last lifecycle state (`done`, which has no agent and so no entry in
/// [`PHASES`]): a node entering it has its sandbox deleted unless
/// [`resolve_delete_sandbox`] says otherwise.
pub const FINAL_PHASE: &str = crate::settings::VALID_LIFECYCLE_STATES[crate::settings::VALID_LIFECYCLE_STATES.len() - 1];

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

/// Whether the node's sandbox is deleted when it enters [`FINAL_PHASE`], and
/// the node the answer came from: the nearest node, from itself up, that
/// states it; on by default when none does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteSandbox {
    pub enabled: bool,
    pub source_node: Option<Uuid>,
}

/// What `node_id` itself says about deleting its sandbox, if anything.
pub fn own_delete_sandbox(conn: &Connection, node_id: Uuid) -> Result<Option<bool>> {
    let has = conn
        .prepare("SELECT 1 FROM pragma_table_info('node_lifecycle_config') WHERE name = 'delete_sandbox'")?
        .exists([])?;
    if !has {
        return Ok(None);
    }
    let value: Option<Option<i64>> = conn
        .query_row(
            "SELECT delete_sandbox FROM node_lifecycle_config WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.flatten().map(|v| v != 0))
}

/// [`DeleteSandbox`] for `node_id`.
pub fn resolve_delete_sandbox(conn: &Connection, node_id: Uuid) -> Result<DeleteSandbox> {
    for id in crate::outline::ancestor_chain(conn, node_id)?.into_iter().rev() {
        if let Some(enabled) = own_delete_sandbox(conn, id)? {
            return Ok(DeleteSandbox { enabled, source_node: Some(id) });
        }
    }
    Ok(DeleteSandbox { enabled: true, source_node: None })
}

/// Set (`Some`) or clear (`None`, so an ancestor's choice applies again) what
/// `node_id` says about deleting its sandbox.
pub fn set_delete_sandbox(conn: &Connection, node_id: Uuid, value: Option<bool>) -> Result<()> {
    conn.execute(
        "INSERT INTO node_lifecycle_config (node_id, skills, updated_at, delete_sandbox) VALUES (?1, '{}', ?2, ?3)
         ON CONFLICT(node_id) DO UPDATE SET delete_sandbox = excluded.delete_sandbox, updated_at = excluded.updated_at",
        params![uuid_to_blob(node_id), now_ms(), value.map(i64::from)],
    )?;
    Ok(())
}
