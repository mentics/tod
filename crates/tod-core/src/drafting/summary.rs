//! Node summaries: the few sentences that stand in for an ancestor's
//! requirements in its descendants' context. The drafting driver writes any
//! that are missing before a turn, so context never lists an ancestor's
//! requirements instead.

use crate::drafting::driver::tagged_block;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};
use tod_store::outline::repos::{NodeRepo, ObligationRepo};
use tod_store::outline::{
    EXTRA_CONTENT_GOAL, EXTRA_CONTENT_SUMMARY, KIND_CONSTRAINT, ancestor_chain, resolve_obligations,
};
use uuid::Uuid;

/// Delimits the summary in a summarizer's reply.
pub(crate) const NODE_SUMMARY_OPEN: &str = "<node-summary>";
pub(crate) const NODE_SUMMARY_CLOSE: &str = "</node-summary>";

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn has_summary(nodes: &NodeRepo<'_>, node_id: Uuid) -> bool {
    nodes
        .get_extra_content(node_id, EXTRA_CONTENT_SUMMARY)
        .ok()
        .flatten()
        .is_some_and(|s| !s.trim().is_empty())
}

/// Ancestors whose requirements reach `node_id`'s context and that have no
/// summary to stand in for them, root first. An ancestor with only
/// constraints needs none: constraints are shown in full.
pub fn missing(conn: &Connection, node_id: Uuid, max_phase: Option<&str>) -> Result<Vec<Uuid>> {
    let nodes = NodeRepo::new(conn);
    let mut out = Vec::new();
    for item in resolve_obligations(conn, node_id, max_phase)? {
        let source = item.source_node_id;
        if source == node_id
            || source.is_nil()
            || item.obligation.kind == KIND_CONSTRAINT
            || out.contains(&source)
        {
            continue;
        }
        if !has_summary(&nodes, source) {
            out.push(source);
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
         requirements; its constraints reach them in full. In 1 to 3 sentences, say what \
         the node is and what it covers, so a descendant knows what is already settled \
         above it. No constraints, implementation detail, or ids.\n\n\
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
    if let Some(goal) = nodes
        .get_extra_content(node_id, EXTRA_CONTENT_GOAL)?
        .filter(|g| !g.trim().is_empty())
    {
        writeln!(out, "Goal: {}", one_line(&goal))?;
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
