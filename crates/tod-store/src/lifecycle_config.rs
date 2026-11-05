//! The Lifecycle config capability: the skills the agent for each lifecycle
//! phase should use, and the model and effort it runs with.
//!
//! It is its own capability, not part of Lifecycle, because Lifecycle sits on
//! nearly every leaf node while this is meant for one or two high-level nodes
//! and inherited by everything below. A node with the capability holds a map
//! from [`PHASES`] key to an ordered list of skill names (`node_lifecycle_config`,
//! one JSON row). A node's skills for a phase come from the nearest node, from
//! itself up, that lists that phase ([`resolve`]); an explicit empty list
//! means no skills, and stops an ancestor's list applying below.
//!
//! The model and effort for a phase (`agents`, one JSON row beside the skills)
//! resolve field by field the same way ([`resolve_agent`]): the nearest node
//! that sets the phase's model gives the model, the nearest that sets its
//! effort gives the effort. A launch applies them over the node's Agent
//! capability and the settings, so a phase's choice wins for that phase only.

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
    delete_sandbox INTEGER,
    agents      TEXT NOT NULL DEFAULT '{}',
    pr_draft_review INTEGER
);";

/// The `pr_draft_review` column (idempotent, no schema bump, like `agents`):
/// NULL means this node says nothing and the nearest ancestor that does decides.
pub fn add_pr_draft_review(conn: &Connection) -> Result<()> {
    let has = conn
        .prepare("SELECT 1 FROM pragma_table_info('node_lifecycle_config') WHERE name = 'pr_draft_review'")?
        .exists([])?;
    if !has {
        conn.execute_batch("ALTER TABLE node_lifecycle_config ADD COLUMN pr_draft_review INTEGER")?;
    }
    Ok(())
}

/// The `agents` column (idempotent, no schema bump: the sync copies only the
/// columns both sides have): the model and effort by phase, as JSON.
pub fn add_agents(conn: &Connection) -> Result<()> {
    let has = conn
        .prepare("SELECT 1 FROM pragma_table_info('node_lifecycle_config') WHERE name = 'agents'")?
        .exists([])?;
    if !has {
        conn.execute_batch("ALTER TABLE node_lifecycle_config ADD COLUMN agents TEXT NOT NULL DEFAULT '{}'")?;
    }
    Ok(())
}

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
///
/// A phase's list is ordered and flat, with [`GROUP_BREAK`] elements between
/// groups: the first group is worked in the phase agent's own session, and each
/// later one in a session of its own (`a, b; c` is two groups, `; a` is an empty
/// first group and then `a`). A list with no break is one group, as before.
pub type Skills = BTreeMap<String, Vec<String>>;

/// The element of a stored list that separates two groups of skills. It is
/// never a skill name (names are plain file names), so no name can collide.
pub const GROUP_BREAK: &str = ";";

/// Parse the text a user enters for one phase: names separated by commas,
/// groups by semicolons. A leading semicolon is an empty first group (so the
/// phase agent's own session runs no skills); a trailing semicolon or two in a
/// row would be an empty group that means nothing, and is an error.
pub fn parse_list(text: &str) -> Result<Vec<String>> {
    let mut list = Vec::new();
    for (index, group) in text.split(';').enumerate() {
        if index > 0 {
            list.push(GROUP_BREAK.to_string());
        }
        let names: Vec<&str> = group.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
        if names.is_empty() && index > 0 {
            bail!(
                "an empty skill group after a `;` means nothing: remove the extra or trailing `;` \
                 (only a leading `;` is meaningful, and keeps the first skills out of the phase agent's session)"
            );
        }
        list.extend(names.into_iter().map(String::from));
    }
    // Text with no names at all (`''`) stays the explicit empty list.
    Ok(list)
}

/// The text form of a stored list, as [`parse_list`] reads it.
pub fn format_list(list: &[String]) -> String {
    let mut out = String::new();
    let mut first_in_group = true;
    for name in list {
        if name == GROUP_BREAK {
            out.push(';');
            first_in_group = true;
            continue;
        }
        if !first_in_group {
            out.push(',');
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(name);
        first_in_group = false;
    }
    out
}

/// A stored list as its groups, in order. An empty list has none; otherwise
/// the first is the phase agent's (possibly empty).
pub fn groups(list: &[String]) -> Vec<Vec<String>> {
    if list.is_empty() {
        return Vec::new();
    }
    list.split(|n| n == GROUP_BREAK).map(<[String]>::to_vec).collect()
}

/// The model and effort the agent for one phase runs with, as stored on one
/// node. Either may be unset, leaving it to an ancestor, the node's Agent
/// capability, or the settings. Free text, like the Agent capability's: the
/// launch coerces a value its platform does not know to the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PhaseAgent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl PhaseAgent {
    pub fn is_empty(&self) -> bool {
        self.model.is_none() && self.effort.is_none()
    }
}

/// Model and effort by phase key, as stored on one node.
pub type Agents = BTreeMap<String, PhaseAgent>;

/// The model and effort for one phase after inheritance, each with the node
/// it came from. `None` where no node sets it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedAgent {
    pub model: Option<(String, Uuid)>,
    pub effort: Option<(String, Uuid)>,
}

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
        // Blanks and repeats within one group are dropped; a break must follow
        // a name or open the list, never repeat or end it.
        let mut group_start = 0;
        for name in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
            if name == GROUP_BREAK {
                if list.last().is_some_and(|n| n == GROUP_BREAK) {
                    bail!("{phase}: an empty skill group means nothing (two `;` in a row)");
                }
                list.push(name.to_string());
                group_start = list.len();
            } else if !list[group_start..].iter().any(|have| have == name) {
                list.push(name.to_string());
            }
        }
        if list.last().is_some_and(|n| n == GROUP_BREAK) {
            bail!("{phase}: a trailing `;` leaves an empty skill group, which means nothing");
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

/// The model and effort defined on `node_id` itself, by phase.
pub fn agents(conn: &Connection, node_id: Uuid) -> Result<Agents> {
    let has = conn
        .prepare("SELECT 1 FROM pragma_table_info('node_lifecycle_config') WHERE name = 'agents'")?
        .exists([])?;
    if !has {
        return Ok(Agents::new());
    }
    let raw: Option<String> = conn
        .query_row(
            "SELECT agents FROM node_lifecycle_config WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
}

/// Replace the model and effort by phase on `node_id`. Rejects an unknown
/// phase; trims values, and drops a blank one and a phase that sets nothing.
pub fn set_agents(conn: &Connection, node_id: Uuid, config: &Agents) -> Result<()> {
    let mut clean = Agents::new();
    for (phase, agent) in config {
        if !is_phase(phase) {
            bail!("{phase} is not a lifecycle phase ({})", PHASES.join(", "));
        }
        let trim = |v: &Option<String>| v.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(String::from);
        let agent = PhaseAgent { model: trim(&agent.model), effort: trim(&agent.effort) };
        if !agent.is_empty() {
            clean.insert(phase.clone(), agent);
        }
    }
    conn.execute(
        "INSERT INTO node_lifecycle_config (node_id, skills, updated_at, agents) VALUES (?1, '{}', ?2, ?3)
         ON CONFLICT(node_id) DO UPDATE SET agents = excluded.agents, updated_at = excluded.updated_at",
        params![uuid_to_blob(node_id), now_ms(), serde_json::to_string(&clean)?],
    )?;
    Ok(())
}

/// The model and effort for `phase` on `node_id`, each from the nearest
/// node, from itself up, that sets it.
pub fn resolve_agent(conn: &Connection, node_id: Uuid, phase: &str) -> Result<ResolvedAgent> {
    let mut out = ResolvedAgent::default();
    for id in crate::outline::ancestor_chain(conn, node_id)?.into_iter().rev() {
        let Some(agent) = agents(conn, id)?.remove(phase) else { continue };
        if out.model.is_none() && let Some(model) = agent.model {
            out.model = Some((model, id));
        }
        if out.effort.is_none() && let Some(effort) = agent.effort {
            out.effort = Some((effort, id));
        }
        if out.model.is_some() && out.effort.is_some() {
            break;
        }
    }
    Ok(out)
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

/// Whether `pr_draft` includes the review steps (the `review` phase's code
/// review, posted on the pull request, and the `fix` phase's skills for its
/// agent), and the node the answer came from: the nearest node, from itself
/// up, that states it; off by default when none does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrDraftReview {
    pub enabled: bool,
    pub source_node: Option<Uuid>,
}

/// What `node_id` itself says about the review steps in `pr_draft`, if anything.
pub fn own_pr_draft_review(conn: &Connection, node_id: Uuid) -> Result<Option<bool>> {
    let has = conn
        .prepare("SELECT 1 FROM pragma_table_info('node_lifecycle_config') WHERE name = 'pr_draft_review'")?
        .exists([])?;
    if !has {
        return Ok(None);
    }
    let value: Option<Option<i64>> = conn
        .query_row(
            "SELECT pr_draft_review FROM node_lifecycle_config WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.flatten().map(|v| v != 0))
}

/// [`PrDraftReview`] for `node_id`.
pub fn resolve_pr_draft_review(conn: &Connection, node_id: Uuid) -> Result<PrDraftReview> {
    for id in crate::outline::ancestor_chain(conn, node_id)?.into_iter().rev() {
        if let Some(enabled) = own_pr_draft_review(conn, id)? {
            return Ok(PrDraftReview { enabled, source_node: Some(id) });
        }
    }
    Ok(PrDraftReview { enabled: false, source_node: None })
}

/// Set (`Some`) or clear (`None`, so an ancestor's choice applies again) what
/// `node_id` says about the review steps in `pr_draft`.
pub fn set_pr_draft_review(conn: &Connection, node_id: Uuid, value: Option<bool>) -> Result<()> {
    conn.execute(
        "INSERT INTO node_lifecycle_config (node_id, skills, updated_at, pr_draft_review) VALUES (?1, '{}', ?2, ?3)
         ON CONFLICT(node_id) DO UPDATE SET pr_draft_review = excluded.pr_draft_review, updated_at = excluded.updated_at",
        params![uuid_to_blob(node_id), now_ms(), value.map(i64::from)],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_list_without_a_semicolon_is_one_group() {
        assert_eq!(parse_list("a, b").unwrap(), v(&["a", "b"]));
        assert_eq!(groups(&v(&["a", "b"])), vec![v(&["a", "b"])]);
        assert!(parse_list("").unwrap().is_empty());
        assert!(groups(&[]).is_empty());
    }

    #[test]
    fn semicolons_separate_groups() {
        let list = parse_list("a, b; c; d, e").unwrap();
        assert_eq!(list, v(&["a", "b", ";", "c", ";", "d", "e"]));
        assert_eq!(groups(&list), vec![v(&["a", "b"]), v(&["c"]), v(&["d", "e"])]);
        assert_eq!(format_list(&list), "a, b; c; d, e");
    }

    #[test]
    fn a_leading_semicolon_leaves_the_first_group_empty() {
        let list = parse_list("; a, b").unwrap();
        assert_eq!(groups(&list), vec![vec![], v(&["a", "b"])]);
        assert_eq!(format_list(&list), "; a, b");
        assert_eq!(parse_list(&format_list(&list)).unwrap(), list);
    }

    #[test]
    fn an_empty_group_after_the_first_is_refused() {
        for text in ["a;", "a;;b", ";", "; ;a", "a; ,"] {
            assert!(parse_list(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn set_agents_trims_and_drops_phases_that_set_nothing() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE nodes (id BLOB PRIMARY KEY NOT NULL);").unwrap();
        conn.execute_batch(CREATE_TABLE).unwrap();
        let node = Uuid::new_v4();
        conn.execute("INSERT INTO nodes (id) VALUES (?1)", params![uuid_to_blob(node)]).unwrap();
        let mut config = Agents::new();
        config.insert("review".into(), PhaseAgent { model: Some(" opus ".into()), effort: Some("".into()) });
        config.insert("fix".into(), PhaseAgent::default());
        set_agents(&conn, node, &config).unwrap();
        let stored = agents(&conn, node).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored["review"], PhaseAgent { model: Some("opus".into()), effort: None });
        // The skills column is untouched by it.
        assert!(skills(&conn, node).unwrap().is_empty());
        let mut bad = Agents::new();
        bad.insert("done".into(), PhaseAgent { model: Some("opus".into()), effort: None });
        assert!(set_agents(&conn, node, &bad).is_err());
    }

    #[test]
    fn set_skills_refuses_empty_groups_and_keeps_breaks() {
        let conn = Connection::open_in_memory().unwrap();
        let mut bad = Skills::new();
        bad.insert("pr_draft".into(), v(&["a", ";", ";", "b"]));
        // No table needed: validation runs before the write.
        assert!(set_skills(&conn, Uuid::new_v4(), &bad).is_err());
        bad.insert("pr_draft".into(), v(&["a", ";"]));
        assert!(set_skills(&conn, Uuid::new_v4(), &bad).is_err());
    }
}
