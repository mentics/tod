//! The one place that resolves and renders a node's inherited (ancestor)
//! obligation context. Every surface that assembles context for a node —
//! interview snapshots, gate checks, drafting turns, "on entry" hooks, and
//! the lifecycle panel's implementation sessions — calls
//! [`render_inherited_context`] rather than re-deriving this policy locally.
//!
//! The policy (per the user's stated design): an ancestor's scope is settled
//! and out of bounds for the node in front of the agent, so an ancestor
//! contributes its title, its generated summary, and its constraint-kind
//! obligations only — never its full requirements, which would put hundreds
//! of unrelated rows into every context for a deep tree. The node the
//! session/turn is actually about gets its own obligations in full (its
//! requirements are exactly what "done" means for it); callers fetch those
//! separately (e.g. `FleetStore::list_obligations_for_node`) since whether
//! and how to show them is caller-specific (an interview snapshot sections
//! them by requirement/constraint, an implementation session lists them
//! plainly).

use anyhow::Result;
use rusqlite::Connection;
use std::fmt::Write as _;
use tod_store::interview::short_id;
use tod_store::outline::repos::NodeRepo;
use tod_store::outline::{
    EXTRA_CONTENT_SUMMARY, KIND_CONSTRAINT, NodeObligation, PlanStep, resolve_obligations,
};
use uuid::Uuid;

pub(crate) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn node_title(nodes: &NodeRepo<'_>, id: Uuid) -> String {
    if id.is_nil() {
        return "global".into();
    }
    nodes
        .get(id)
        .ok()
        .flatten()
        .map(|n| n.title)
        .unwrap_or_else(|| short_id(id))
}

pub fn plan_step_line(step: &PlanStep, deps: &[Uuid], obligations: &[Uuid]) -> String {
    let deps = if deps.is_empty() {
        String::new()
    } else {
        format!(
            " deps=[{}]",
            deps.iter()
                .map(|id| short_id(*id))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let satisfies = if obligations.is_empty() {
        String::new()
    } else {
        format!(
            " satisfies=[{}]",
            obligations
                .iter()
                .map(|id| short_id(*id))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    format!(
        "[{}] {}{deps}{satisfies}: {}",
        short_id(step.id),
        step.status,
        one_line(&step.body)
    )
}

pub fn obligation_line(o: &NodeObligation) -> String {
    let section = o
        .section
        .as_deref()
        .map(|s| format!(" ({s})"))
        .unwrap_or_default();
    let visual_design = if o.visual_design_path.is_some() {
        " [visual design attached]"
    } else {
        ""
    };
    format!(
        "[{}] {}{section}: {}{visual_design}",
        short_id(o.id),
        o.kind,
        one_line(&o.body)
    )
}

/// Renders `node_id`'s ancestor (and global) obligations. Each ancestor
/// contributes its title, its generated summary (`EXTRA_CONTENT_SUMMARY`),
/// and its constraint-kind obligations in full. Its requirements are never
/// listed: the summary stands in for them, and a deep tree would otherwise
/// put hundreds into every context. The drafting driver writes missing
/// summaries before a turn (`crate::drafting::summary`); anywhere else, an
/// ancestor still without one gets a pointer to `tod-cli` instead. Global
/// (no owning node) obligations always show in full; there is nothing to
/// summarize about them. This never includes `node_id`'s own obligations —
/// callers show those separately, in full.
pub fn render_inherited_context(
    conn: &Connection,
    nodes: &NodeRepo<'_>,
    node_id: Uuid,
    max_phase: Option<&str>,
) -> Result<String> {
    let inherited: Vec<_> = resolve_obligations(conn, node_id, max_phase)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.source_node_id != node_id)
        .collect();
    if inherited.is_empty() {
        return Ok(String::new());
    }

    let mut order: Vec<Uuid> = Vec::new();
    let mut groups: std::collections::HashMap<Uuid, Vec<NodeObligation>> =
        std::collections::HashMap::new();
    for item in inherited {
        if !order.contains(&item.source_node_id) {
            order.push(item.source_node_id);
        }
        groups
            .entry(item.source_node_id)
            .or_default()
            .push(item.obligation);
    }

    let mut out = String::new();
    out.push_str("\n## Inherited context (ancestors)\n\n");
    out.push_str(
        "Each ancestor below is summarized, not fully restated — its scope \
         is settled and out of bounds here. Only decide what belongs to \
         *this* node; a gap in an ancestor's own scope belongs on that \
         ancestor, not as a question or obligation on this node.\n",
    );

    for source_id in order {
        let items = groups.remove(&source_id).unwrap_or_default();
        if source_id.is_nil() {
            out.push_str("\n### Global\n");
            for o in &items {
                writeln!(out, "- {}", obligation_line(o))?;
            }
            continue;
        }
        let title = node_title(nodes, source_id);
        writeln!(out, "\n### From \"{title}\"")?;
        let summary = nodes
            .get_extra_content(source_id, EXTRA_CONTENT_SUMMARY)
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty());
        let constraints: Vec<&NodeObligation> =
            items.iter().filter(|o| o.kind == KIND_CONSTRAINT).collect();
        match summary {
            Some(summary) => writeln!(out, "{}", one_line(&summary))?,
            None if constraints.len() < items.len() => writeln!(
                out,
                "(No summary yet. Its requirements, if you need them: `obligations list --node {source_id}`.)"
            )?,
            None => {}
        }
        if !constraints.is_empty() {
            out.push_str("\nConstraints:\n");
            for o in constraints {
                writeln!(out, "- {}", obligation_line(o))?;
            }
        }
    }
    Ok(out)
}
