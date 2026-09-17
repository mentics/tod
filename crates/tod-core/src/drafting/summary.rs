//! Node summaries: the few sentences, generated from a Spec node's details and
//! obligations, that are all its descendants inherit of its scope. The
//! drafting driver writes any that are missing or stale before a turn, so
//! context never lists an ancestor's requirements instead, nor an account of
//! it that no longer holds.

use crate::drafting::driver::tagged_block;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};
use tod_store::outline::repos::{NodeRepo, ObligationRepo};
use tod_store::outline::{
    Capability, EXTRA_CONTENT_DETAILS, KIND_CONSTRAINT, ancestor_chain,
};
use uuid::Uuid;

/// Delimits the summary in a summarizer's reply.
pub(crate) const NODE_SUMMARY_OPEN: &str = "<node-summary>";
pub(crate) const NODE_SUMMARY_CLOSE: &str = "</node-summary>";

use crate::node_context::one_line;

/// True when the node has a summary written since its details and
/// obligations last changed.
pub fn is_current(nodes: &NodeRepo<'_>, node_id: Uuid) -> bool {
    nodes
        .get_summary(node_id)
        .ok()
        .flatten()
        .is_some_and(|s| !s.stale)
}

/// True when the node has something to summarize: details, or requirements.
/// A node with only constraints needs no summary, since constraints are
/// inherited in full.
fn needs_summary(conn: &Connection, nodes: &NodeRepo<'_>, node_id: Uuid) -> Result<bool> {
    let has_details = nodes
        .get_extra_content(node_id, EXTRA_CONTENT_DETAILS)?
        .is_some_and(|d| !d.trim().is_empty());
    Ok(has_details
        || ObligationRepo::new(conn)
            .list_for_node(node_id)?
            .iter()
            .any(|o| o.kind != KIND_CONSTRAINT))
}

/// Spec ancestors of `node_id` whose summary is missing or stale, root first.
pub fn missing(conn: &Connection, node_id: Uuid) -> Result<Vec<Uuid>> {
    let nodes = NodeRepo::new(conn);
    let mut out = Vec::new();
    for id in ancestor_chain(conn, node_id)?
        .into_iter()
        .filter(|id| *id != node_id)
    {
        if nodes.list_capabilities(id)?.contains(&Capability::Spec)
            && !is_current(&nodes, id)
            && needs_summary(conn, &nodes, id)?
        {
            out.push(id);
        }
    }
    Ok(out)
}

/// The node's title and the message asking an agent for its summary.
pub fn request(conn: &Connection, node_id: Uuid) -> Result<(String, String)> {
    let nodes = NodeRepo::new(conn);
    let title = nodes.get(node_id)?.map(|n| n.title).unwrap_or_default();
    let mut out = String::from(
        "# Summarize a node\n\n\
         Agents working on this node's descendants see your summary in place of its \
         details and requirements; its constraints reach them in full. In 1 to 3 \
         sentences, say what the node is and what it covers, so a descendant knows what \
         is already settled above it. No constraints, implementation detail, or ids.\n\n\
         Everything you need is below: don't run tools or read files. Reply with only \
         the summary inside <node-summary> tags.\n\n",
    );
    writeln!(out, "Node: \"{title}\"")?;
    let path: Vec<String> = ancestor_chain(conn, node_id)?
        .into_iter()
        .filter(|id| *id != node_id)
        .filter_map(|id| nodes.get(id).ok().flatten().map(|n| n.title))
        .collect();
    if !path.is_empty() {
        writeln!(out, "Part of: {}", path.join(" › "))?;
    }
    if let Some(details) = nodes
        .get_extra_content(node_id, EXTRA_CONTENT_DETAILS)?
        .filter(|d| !d.trim().is_empty())
    {
        write!(out, "\n## Details\n\n{}\n", details.trim())?;
    }
    out.push_str("\n## Requirements\n\n");
    for o in ObligationRepo::new(conn)
        .list_for_node(node_id)?
        .iter()
        .filter(|o| o.kind != KIND_CONSTRAINT)
    {
        match &o.section {
            Some(section) => writeln!(out, "- ({section}) {}", one_line(&o.body))?,
            None => writeln!(out, "- {}", one_line(&o.body))?,
        }
    }
    Ok((title, out))
}

/// The summary out of a summarizer's whole reply.
pub fn parse(reply: &str) -> Option<String> {
    tagged_block(reply, NODE_SUMMARY_OPEN, NODE_SUMMARY_CLOSE).map(|s| one_line(&s))
}

/// Nodes some driver is summarizing now, so drivers under the same ancestor
/// (a subtree rewrite runs many) don't each ask for the same summary.
fn in_flight() -> &'static Mutex<HashSet<Uuid>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<Uuid>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(Default::default)
}

/// Take `node_id` for summarizing; false when another driver already has it.
pub(crate) fn claim(node_id: Uuid) -> bool {
    in_flight()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(node_id)
}

pub(crate) fn release(node_id: Uuid) {
    in_flight()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&node_id);
}

/// `--agent mock` summarizer: a canned summary naming the node.
pub(crate) fn mock_summarizer(text: &str) -> Result<String> {
    let title = text
        .lines()
        .find_map(|l| l.strip_prefix("Node: "))
        .map(|t| t.trim().trim_matches('"'))
        .unwrap_or("the node");
    Ok(format!(
        "Mock: summarizing.\n{NODE_SUMMARY_OPEN}Mock summary of {title}: what it covers.{NODE_SUMMARY_CLOSE}"
    ))
}
