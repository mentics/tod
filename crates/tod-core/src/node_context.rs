//! The one place that resolves and renders a node's inherited (ancestor)
//! obligation context. Every surface that assembles context for a node —
//! interview snapshots, gate checks, "on entry" hooks, agent chats, and the
//! lifecycle panel's implementation sessions — calls
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
use std::path::Path;
use tod_store::interview::short_id;
use tod_store::outline::repos::NodeRepo;
use tod_store::outline::{
    Capability, KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation, PlanStep, ancestor_chain,
    resolve_obligations,
};
use uuid::Uuid;

pub(crate) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn node_title(nodes: &NodeRepo<'_>, id: Uuid) -> String {
    nodes
        .get(id)
        .ok()
        .flatten()
        .map(|n| n.title)
        .unwrap_or_else(|| short_id(id))
}

/// The lines a session snapshot opens with: where the data is, which
/// `tod-cli` to call, and which node the session is about. Callers add their
/// own session-specific lines (phase, mode, role) after these.
pub(crate) fn write_snapshot_header(
    out: &mut String,
    nodes: &NodeRepo<'_>,
    data_root: &Path,
    tod_cli: &Path,
    node_id: Uuid,
) -> Result<()> {
    writeln!(out, "Data root: {}", data_root.display())?;
    writeln!(out, "tod-cli: {}", tod_cli.display())?;
    match nodes.get(node_id)? {
        Some(node) => writeln!(out, "Node: {node_id} [[{}]] \"{}\"", node.slug, node.title)?,
        None => writeln!(out, "Node: {node_id}")?,
    }
    if let Some(lifecycle) = nodes.get_lifecycle(node_id)? {
        writeln!(out, "Lifecycle: {lifecycle}")?;
    }
    Ok(())
}

/// A node's own obligations under `## Obligations`, split into Requirements
/// and Constraints and, within each, grouped by section (unsectioned first).
/// `line` renders one item; it need not repeat the kind or section.
pub(crate) fn write_obligations_by_kind<T>(
    out: &mut String,
    items: &[T],
    obligation: impl Fn(&T) -> &NodeObligation,
    line: impl Fn(&T) -> String,
) -> Result<()> {
    out.push_str("\n## Obligations\n");
    if items.is_empty() {
        out.push_str("\n(none yet)\n");
    }
    for (kind, heading) in [
        (KIND_REQUIREMENT, "Requirements"),
        (KIND_CONSTRAINT, "Constraints"),
    ] {
        let of_kind: Vec<&T> = items
            .iter()
            .filter(|t| obligation(t).kind == kind)
            .collect();
        if of_kind.is_empty() {
            continue;
        }
        writeln!(out, "\n### {heading}")?;
        let mut sections: Vec<Option<&str>> = Vec::new();
        for t in &of_kind {
            let section = obligation(t).section.as_deref();
            if !sections.contains(&section) {
                sections.push(section);
            }
        }
        sections.sort_by_key(|s| s.is_some());
        for section in sections {
            if let Some(section) = section {
                writeln!(out, "{section}:")?;
            }
            for t in of_kind
                .iter()
                .filter(|t| obligation(t).section.as_deref() == section)
            {
                writeln!(out, "- {}", line(t))?;
            }
        }
    }
    Ok(())
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
    let reason = match &step.reason {
        Some(reason) => format!(" ({})", reason.describe()),
        None => String::new(),
    };
    let note = match &step.note {
        Some(note) => format!(" (note: {})", one_line(note)),
        None => String::new(),
    };
    format!(
        "[{}] {}{deps}{satisfies}: {}{reason}{note}",
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

/// Renders what `node_id` inherits: each Spec ancestor, root first, with its
/// title, its generated summary (`NodeRepo::get_summary`), and its
/// constraint-kind obligations in full. The summary is the only account of an
/// ancestor's scope a descendant gets: its details and requirements are never
/// listed, since a deep tree would otherwise put hundreds of unrelated rows
/// into every context. An ancestor still without a summary gets a pointer to
/// `tod-cli` instead. This never includes anything of `node_id`'s
/// own — callers show that separately.
pub fn render_inherited_context(
    conn: &Connection,
    nodes: &NodeRepo<'_>,
    node_id: Uuid,
    max_phase: Option<&str>,
) -> Result<String> {
    let mut groups: std::collections::HashMap<Uuid, Vec<NodeObligation>> =
        std::collections::HashMap::new();
    for item in resolve_obligations(conn, node_id, max_phase).unwrap_or_default() {
        groups
            .entry(item.source_node_id)
            .or_default()
            .push(item.obligation);
    }
    let mut ancestors = String::new();
    for source_id in ancestor_chain(conn, node_id)?
        .into_iter()
        .filter(|id| *id != node_id)
    {
        let items = groups.remove(&source_id).unwrap_or_default();
        let summary = nodes.get_summary(source_id).ok().flatten();
        let has_spec = nodes
            .list_capabilities(source_id)
            .is_ok_and(|caps| caps.contains(&Capability::Spec));
        if !has_spec || (summary.is_none() && items.is_empty()) {
            continue;
        }
        let title = node_title(nodes, source_id);
        writeln!(ancestors, "\n### From \"{title}\"")?;
        let constraints: Vec<&NodeObligation> =
            items.iter().filter(|o| o.kind == KIND_CONSTRAINT).collect();
        match summary {
            Some(summary) => writeln!(ancestors, "{}", one_line(&summary.body))?,
            None if constraints.len() < items.len() => writeln!(
                ancestors,
                "(No summary yet. Its requirements, if you need them: `obligations list --node {source_id}`.)"
            )?,
            None => {}
        }
        if !constraints.is_empty() {
            ancestors.push_str("\nConstraints:\n");
            for o in constraints {
                writeln!(ancestors, "- {}", obligation_line(o))?;
            }
        }
    }
    if ancestors.is_empty() {
        return Ok(String::new());
    }

    let mut out = String::new();
    out.push_str("\n## Inherited context (ancestors)\n\n");
    out.push_str(
        "Each ancestor below is summarized, not fully restated — its scope \
         is settled and out of bounds here. Only decide what belongs to \
         *this* node; a gap in an ancestor's own scope belongs on that \
         ancestor, not as a question or obligation on this node.\n",
    );
    out.push_str(&ancestors);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obligation(n: u128, kind: &str, section: Option<&str>) -> NodeObligation {
        NodeObligation {
            id: Uuid::from_u128(n),
            node_id: Uuid::nil(),
            kind: kind.into(),
            ordinal: n as i32,
            section: section.map(Into::into),
            body: format!("body {n}"),
            phase: "requirements".into(),
            visual_design_path: None,
        }
    }

    /// A Spec ancestor's summary reaches its descendants whether or not the
    /// ancestor has obligations; its details never do.
    #[test]
    fn an_ancestor_is_inherited_as_its_summary_alone() {
        use tod_store::outline::{CreatePosition, OutlineMutation};
        let fx = crate::interview::test_support::fixture();
        let child = Uuid::new_v4();
        fx.outline(OutlineMutation::CreateNode {
            node_id: Some(child),
            list_id: fx.fleet.list_outline_lists().unwrap()[0].id,
            parent_id: Some(fx.node),
            anchor_id: None,
            position: CreatePosition::Child,
            title: "Child".into(),
        });
        let set = |content_type: &str, body: &str| {
            fx.outline(OutlineMutation::SetExtraContent {
                node_id: fx.node,
                content_type: content_type.into(),
                body: body.into(),
            })
        };
        let inherited = || {
            fx.fleet
                .read(|conn| render_inherited_context(conn, &NodeRepo::new(conn), child, None))
                .unwrap()
        };
        set("details", "Long freeform account of the parent.");
        assert_eq!(inherited(), "", "details alone are not inherited");

        set("summary", "Covers the parent.");
        let text = inherited();
        assert!(text.contains("### From \"Interview node\""), "{text}");
        assert!(text.contains("Covers the parent."), "{text}");
        assert!(!text.contains("Long freeform"), "{text}");
    }

    fn render(items: &[NodeObligation]) -> String {
        let mut out = String::new();
        write_obligations_by_kind(&mut out, items, |o| o, |o| o.body.clone()).unwrap();
        out
    }

    #[test]
    fn obligations_with_none_say_so() {
        assert_eq!(render(&[]), "\n## Obligations\n\n(none yet)\n");
    }

    /// Requirements before constraints, and within a kind the unsectioned
    /// items come first, then each section under its own label.
    #[test]
    fn obligations_group_by_kind_then_section() {
        let items = [
            obligation(1, KIND_CONSTRAINT, None),
            obligation(2, KIND_REQUIREMENT, Some("Input")),
            obligation(3, KIND_REQUIREMENT, None),
            obligation(4, KIND_REQUIREMENT, Some("Input")),
        ];
        assert_eq!(
            render(&items),
            "\n## Obligations\n\
             \n### Requirements\n\
             - body 3\n\
             Input:\n\
             - body 2\n\
             - body 4\n\
             \n### Constraints\n\
             - body 1\n"
        );
    }
}
