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
use tod_store::outline::references::referenced_node_ids;
use tod_store::outline::repos::{NodeRepo, ObligationRepo, PlanStepRepo};
use tod_store::outline::{
    Capability, KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation, PlanStep, ancestor_chain,
    phase_visible, resolve_obligations,
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
/// `tod-cli` instead.
///
/// Then each component the node's obligations reference with `[[slug]]`
/// (the `node_references` edges), rendered the same way: a user of a
/// component builds on its constraints as it does on an ancestor's, and a
/// change to either reaches it as an incoming change
/// (`doc/conversation/incoming-changes.md` §2, §3). A component that is also an
/// ancestor is shown once, as an ancestor. This never includes anything of
/// `node_id`'s own — callers show that separately.
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
    let chain = ancestor_chain(conn, node_id)?;
    let mut ancestors = String::new();
    for source_id in chain.iter().copied().filter(|id| *id != node_id) {
        let items = groups.remove(&source_id).unwrap_or_default();
        write_inherited_source(&mut ancestors, nodes, source_id, &items)?;
    }

    let mut components: Vec<(String, Uuid)> = referenced_node_ids(conn, node_id)?
        .into_iter()
        .filter(|id| !chain.contains(id))
        .map(|id| (node_title(nodes, id), id))
        .collect();
    components.sort();
    let mut referenced = String::new();
    for (_, source_id) in components {
        let items: Vec<NodeObligation> = ObligationRepo::new(conn)
            .list_for_node(source_id)?
            .into_iter()
            .filter(|o| max_phase.is_none_or(|m| phase_visible(&o.phase, m)))
            .collect();
        write_inherited_source(&mut referenced, nodes, source_id, &items)?;
    }

    let mut out = String::new();
    if !ancestors.is_empty() {
        out.push_str(
            "
## Inherited context (ancestors)

",
        );
        out.push_str(
            "Each ancestor below is summarized, not fully restated — its scope              is settled and out of bounds here. Only decide what belongs to              *this* node; a gap in an ancestor's own scope belongs on that              ancestor, not as a question or obligation on this node.
",
        );
        out.push_str(&ancestors);
    }
    if !referenced.is_empty() {
        out.push_str(
            "
## Referenced components

",
        );
        out.push_str(
            "This node's obligations reference the components below with              `[[slug]]`. Each is summarized with its constraints, which hold              wherever it is used; a gap in a component belongs on that              component, not on this node.
",
        );
        out.push_str(&referenced);
    }
    Ok(out)
}

/// One inherited node's block: title, summary (or a pointer to its
/// requirements), and its constraints. Nothing when the node isn't Spec or
/// has neither summary nor obligations.
fn write_inherited_source(
    out: &mut String,
    nodes: &NodeRepo<'_>,
    source_id: Uuid,
    items: &[NodeObligation],
) -> Result<()> {
    let summary = nodes.get_summary(source_id).ok().flatten();
    let has_spec = nodes
        .list_capabilities(source_id)
        .is_ok_and(|caps| caps.contains(&Capability::Spec));
    if !has_spec || (summary.is_none() && items.is_empty()) {
        return Ok(());
    }
    let title = node_title(nodes, source_id);
    writeln!(
        out,
        "
### From \"{title}\""
    )?;
    let constraints: Vec<&NodeObligation> =
        items.iter().filter(|o| o.kind == KIND_CONSTRAINT).collect();
    match summary {
        Some(summary) => writeln!(out, "{}", one_line(&summary.body))?,
        None if constraints.len() < items.len() => writeln!(
            out,
            "(No summary yet. Its requirements, if you need them: `obligations list --node {source_id}`.)"
        )?,
        None => {}
    }
    if !constraints.is_empty() {
        out.push_str(
            "
Constraints:
",
        );
        for o in constraints {
            writeln!(out, "- {}", obligation_line(o))?;
        }
    }
    Ok(())
}

/// What went wrong on the way here, for the `learn` retrospective: every
/// failure verification recorded (on an obligation or a plan step), every
/// step handed back, every review finding, every gate criterion that did not
/// pass, and how many conversations of each kind the node needed. The current
/// plan and obligations only show where the work ended up — all `verified` —
/// so without this a retrospective reads a hard road as a clean run. Empty
/// when nothing of the kind was recorded.
pub fn render_work_history(conn: &Connection, node_id: Uuid) -> Result<String> {
    use tod_store::conversation::{ConversationRepo, Focus, TurnRole};
    use tod_store::outline::repos::plan_steps::{STATUS_IMPLEMENTED, STATUS_VERIFIED};
    use tod_store::outline::{GateRepo, OUTCOME_PASS};
    use tod_store::review::ReviewRepo;
    use tod_store::verification::VerdictRepo;

    let mut out = String::new();

    let verdicts: Vec<_> = VerdictRepo::new(conn)
        .history_for_node(node_id)?
        .into_iter()
        .filter(|verdict| !verdict.is_verified())
        .collect();
    if !verdicts.is_empty() {
        out.push_str("\n### Obligations that did not verify first time\n\n");
        for verdict in &verdicts {
            let _ = writeln!(
                out,
                "- [{}] {}: {}",
                short_id(verdict.obligation_id),
                verdict.status,
                one_line(&verdict.evidence)
            );
        }
    }

    let steps = PlanStepRepo::new(conn);
    let mut step_lines = String::new();
    for step in steps.list_for_node(node_id)? {
        let notes: Vec<_> = steps
            .list_notes(step.id)?
            .into_iter()
            .filter(|note| note.status != STATUS_IMPLEMENTED && note.status != STATUS_VERIFIED)
            .collect();
        if notes.is_empty() {
            continue;
        }
        let _ = writeln!(step_lines, "- [{}] {}", short_id(step.id), one_line(&step.body));
        for note in notes {
            let _ = writeln!(step_lines, "  - {}: {}", note.status, one_line(&note.body));
        }
    }
    if !step_lines.is_empty() {
        out.push_str("\n### Plan steps that failed verification or were handed back\n\n");
        out.push_str(&step_lines);
    }

    let findings = ReviewRepo::new(conn).list_for_node(node_id)?;
    if !findings.is_empty() {
        out.push_str("\n### Code review findings\n\n");
        for finding in &findings {
            let response = finding
                .response
                .as_deref()
                .map(|r| format!(" — {}", one_line(r)))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "- [{}] {} {}: {}{response}",
                short_id(finding.id),
                finding.severity,
                finding.status,
                one_line(&finding.summary)
            );
        }
    }

    let gates = GateRepo::new(conn);
    let mut gate_lines = String::new();
    for evaluation in gates.list_evaluations_for_node(node_id)? {
        if evaluation.outcome == OUTCOME_PASS {
            continue;
        }
        let label = gates
            .get(evaluation.criterion_id)?
            .map(|c| format!("{} → {}: {}", c.from_state, c.to_state, c.label))
            .unwrap_or_else(|| short_id(evaluation.criterion_id));
        let detail = evaluation
            .detail
            .as_deref()
            .map(|d| format!(" — {}", one_line(d)))
            .unwrap_or_default();
        let _ = writeln!(gate_lines, "- {} ({label}){detail}", evaluation.outcome);
    }
    if !gate_lines.is_empty() {
        out.push_str("\n### Gate criteria that did not pass (latest evaluation of each)\n\n");
        out.push_str(&gate_lines);
    }

    let conversations = ConversationRepo::new(conn);
    let mut runs: Vec<(String, usize, usize)> = Vec::new();
    for summary in conversations.list_for_focus(Focus::Node(node_id))? {
        let turns = conversations.turns(summary.conversation.id)?;
        let sent = turns
            .iter()
            .filter(|t| matches!(t.role, TurnRole::User | TurnRole::Continuation))
            .count();
        let kind = summary.conversation.protocol.as_str().to_string();
        match runs.iter_mut().find(|(k, _, _)| *k == kind) {
            Some(run) => {
                run.1 += 1;
                run.2 += sent;
            }
            None => runs.push((kind, 1, sent)),
        }
    }
    if !runs.is_empty() {
        out.push_str("\n### Conversations this node took\n\n");
        for (kind, count, sent) in &runs {
            let _ = writeln!(out, "- {kind}: {count} conversation(s), {sent} turn(s) sent");
        }
    }

    if out.is_empty() {
        return Ok(out);
    }
    Ok(format!(
        "\n## Work history\n\n\
         What this node went through on the way to its current state. The plan \
         and obligations above show only where it ended up; this is the record \
         of what failed, was sent back, or was found in review.\n{out}"
    ))
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
    /// The retrospective has to see what failed along the way, even once
    /// every step ends `verified` — and nothing when nothing did.
    #[test]
    fn work_history_keeps_failures_the_final_state_no_longer_shows() {
        use tod_store::outline::OutlineMutation;
        let fx = crate::interview::test_support::fixture();
        let history = fx
            .fleet
            .read(|conn| render_work_history(conn, fx.node))
            .unwrap();
        assert_eq!(history, "", "a node nothing happened to has no history");

        let step = Uuid::new_v4();
        fx.fleet
            .enqueue_outline(OutlineMutation::CreatePlanStep {
                step_id: Some(step),
                node_id: fx.node,
                after_id: None,
                before: false,
                body: "Sync tickets".into(),
            })
            .unwrap();
        for (status, note) in [
            ("failed", Some("Sync returns no tickets.")),
            ("implemented", None),
            ("verified", None),
        ] {
            fx.fleet
                .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                    step_id: step,
                    status: status.into(),
                    note: note.map(str::to_string),
                    reason: None,
                })
                .unwrap();
        }
        fx.fleet.writer().flush().unwrap();
        let history = fx
            .fleet
            .read(|conn| render_work_history(conn, fx.node))
            .unwrap();
        assert!(history.contains("## Work history"), "{history}");
        assert!(history.contains("failed: Sync returns no tickets."), "{history}");
    }

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

    /// A component the node references with `[[slug]]` is inherited like an
    /// ancestor: its title, summary, and constraints, never its requirements.
    #[test]
    fn a_referenced_component_is_inherited_like_an_ancestor() {
        use tod_store::outline::{CreatePosition, OutlineMutation};
        let fx = crate::interview::test_support::fixture();
        let comp = Uuid::new_v4();
        fx.outline(OutlineMutation::CreateNode {
            node_id: Some(comp),
            list_id: fx.fleet.list_outline_lists().unwrap()[0].id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Dynamic form".into(),
        });
        fx.outline(OutlineMutation::EnableCapabilities {
            node_id: comp,
            capabilities: vec![Capability::Spec],
        });
        fx.fleet.writer().flush().unwrap();
        for (kind, body) in [
            (KIND_CONSTRAINT, "Labels sit above fields"),
            (KIND_REQUIREMENT, "Renders a schema as fields"),
        ] {
            fx.outline(OutlineMutation::CreateObligation {
                obligation_id: None,
                node_id: comp,
                kind: kind.into(),
                after_id: None,
                before: false,
                section: None,
                body: body.into(),
                phase: "requirements".into(),
            });
        }
        fx.outline(OutlineMutation::SetExtraContent {
            node_id: comp,
            content_type: "summary".into(),
            body: "A form built from a schema.".into(),
        });
        fx.fleet.writer().flush().unwrap();
        let inherited = || {
            fx.fleet
                .read(|conn| render_inherited_context(conn, &NodeRepo::new(conn), fx.node, None))
                .unwrap()
        };
        assert_eq!(inherited(), "", "nothing referenced yet");

        let slug = fx
            .fleet
            .read(|conn| Ok(NodeRepo::new(conn).get(comp)?.unwrap().slug))
            .unwrap();
        fx.obligation(&format!("Settings render as a [[{slug}]]"));
        fx.fleet.writer().flush().unwrap();
        let text = inherited();
        assert!(text.contains("## Referenced components"), "{text}");
        assert!(!text.contains("## Inherited context (ancestors)"), "{text}");
        assert!(text.contains("### From \"Dynamic form\""), "{text}");
        assert!(text.contains("A form built from a schema."), "{text}");
        assert!(text.contains("Labels sit above fields"), "{text}");
        assert!(!text.contains("Renders a schema"), "{text}");
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
