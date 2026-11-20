//! The dynamic half of an agent's first message: the live state the surface
//! wants inlined, composed from named blocks.
//!
//! Before this existed, each builder rendered its own copy of "data root,
//! purpose chain, node, obligations, ancestors, plan" with slightly different
//! headings, and varied by branching on which surface it was serving. A
//! surface now names the blocks it wants, in order, and no renderer knows who
//! it is rendering for.
//!
//! See `doc/agent-context-map.md`.

use crate::gate::PlanStepWithLinks;
use crate::node_context::{obligation_line, plan_step_line};
use std::path::Path;
use tod_store::conversation::Focus;
use tod_store::outline::NodeObligation;
use tod_store::review::ReviewFinding;
use tod_store::verification::ObligationVerdict;
use uuid::Uuid;

/// The node an agent surface was opened against.
#[derive(Debug, Clone)]
pub struct NodeSelection {
    pub id: Uuid,
    pub title: String,
    /// Node body/details, when the node has any.
    pub body: Option<String>,
    pub lifecycle: Option<String>,
    /// The node's stable slug, when the caller has it. Rendered so an agent
    /// can address the node by slug rather than pasting a UUID.
    pub slug: Option<String>,
}

/// Where a session's work lands: the repo, branch, and directory a fleet agent
/// was launched into.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub cwd: String,
    /// Free-text notes recorded on the task.
    pub notes: Vec<String>,
}

/// A specific obligation selected within the surface.
#[derive(Debug, Clone)]
pub struct ObligationSelection {
    pub id: Uuid,
    pub kind: String,
    pub body: String,
    /// Path of the obligation's currently-linked visual-design mockup, when
    /// one exists (see `tod-cli visual-design`). `None` if this obligation has
    /// no mockup yet.
    pub visual_design_path: Option<String>,
}

/// What a conversation is about, loaded for [`DynamicBlock::Focus`]. The
/// block renders whatever is here; it does not know who assembled it.
#[derive(Debug, Clone)]
pub struct FocusSelection {
    pub focus: Focus,
    /// Titles of the nodes above the focused item, root first. For an
    /// obligation or plan step this ends with the node it lives on.
    pub path: Vec<String>,
    /// The node the focus is on: the focused node itself, or the node an
    /// obligation or plan step lives on. `None` for the project.
    pub node: Option<Uuid>,
    /// A node's title; an obligation's or plan step's short label.
    pub title: String,
    /// The focused node's slug, or the slug of the node an item lives on.
    pub slug: Option<String>,
    /// An obligation's or plan step's full text.
    pub text: Option<String>,
    /// Listings under the focus, each with its heading: a node's obligations
    /// and plan steps, or the project's top-level nodes. Lines carry ids.
    pub sections: Vec<(String, Vec<String>)>,
}

/// The mockup state of a visual-design conversation, for
/// [`DynamicBlock::VisualDesign`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualDesignState {
    /// The saved mockup linked from the obligation, if any.
    pub saved_path: Option<String>,
    /// The conversation's working draft, which the agent edits in place.
    pub draft_path: String,
    /// Whether the draft exists and differs from the saved mockup.
    pub differs: bool,
}

/// One change a node inherits and has not been checked against, as an agent
/// is shown it. Built by `crate::incoming::describe`; the block renders it
/// without knowing where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingChangeItem {
    /// E.g. "Constraint [1a2b3c4d] added".
    pub headline: String,
    /// The node the change was made on.
    pub source_title: String,
    /// `ancestor` or `reference`.
    pub via: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// One section of the dynamic block. A surface lists the ones it wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicBlock {
    /// The data root, and the instruction to pass it to every `tod-cli` call.
    DataRoot,
    /// Node id, title, lifecycle state, and details.
    Node,
    /// `mode` and `phase_purpose` lines, for the state-agent surfaces whose
    /// role docs key off them.
    NodeProcessFields,
    /// The selected obligation, if any. `fallback` is emitted instead when
    /// nothing is selected — pass `""` on a surface where that cannot happen
    /// or needs no remark.
    SelectedObligation { fallback: &'static str },
    /// This node's own obligations. `note` explains how this surface should
    /// read them relative to the inherited ones.
    NodeObligations { note: &'static str },
    /// Pre-rendered ancestor context — what the node inherits (see
    /// `crate::node_context::render_inherited_context`). Omitted when empty.
    AncestorContext,
    /// This node's plan steps with their dependency and `satisfies` links.
    Plan,
    /// Verification's current verdict on each obligation it has ruled on,
    /// with the evidence. Omitted until there is one.
    ObligationVerdicts,
    /// Pre-rendered record of what failed, was sent back, or was found in
    /// review on the way here (see
    /// `crate::node_context::render_work_history`). Omitted when empty.
    WorkHistory,
    /// The review findings a fix session is to resolve — the open ones,
    /// with their ids, severity, location, and detail.
    ReviewFindings,
    /// Repo, branch, working directory, and task notes.
    Workspace,
    /// The directory the agent was launched in, for a surface whose code
    /// work must stay there.
    WorkingDirectory,
    /// The changes a node inherits and has not been checked against, each
    /// with its before and after text, the node it was made on, and how it
    /// reached this one.
    IncomingChanges,
    /// What the guidance selector chooses from (pre-rendered by
    /// `crate::guidance`): the change or the design, the candidates, the
    /// project's routing table, the lenses, and what is selected so far.
    GuidanceSelection,
    /// What a conversation is about: its kind, path, title, and (by kind) its
    /// full text, its obligations and plan steps, or the top-level nodes.
    Focus,
    /// The variables and credentials defined for the node the work is on
    /// (pre-rendered; see `crate::environment_context`). Omitted when none.
    Environment,
    /// The pull requests linked to the node the work is on, as links
    /// (pre-rendered; see `crate::pr_context`). Omitted when none.
    PullRequests,
    /// The skills the user chose for the phase the agent is working in
    /// (pre-rendered; see `crate::skills_context`). Omitted when none.
    ConfiguredSkills,
    /// The databases the user has set up for this cloud node, by name
    /// (`crate::databases_context`). Omitted when none.
    Databases,
    /// The saved mockup's path, the working draft's path, and whether they
    /// differ. Omitted when the surface supplied no state.
    VisualDesign,
}

/// Everything any block might need. A surface fills in what its blocks use and
/// leaves the rest at its default.
#[derive(Debug, Default, Clone)]
pub struct DynamicContext<'a> {
    pub data_root: Option<&'a Path>,
    pub node: Option<&'a NodeSelection>,
    /// `(mode, phase_purpose)` for [`DynamicBlock::NodeProcessFields`].
    pub process_fields: Option<(&'a str, &'a str)>,
    pub obligation: Option<&'a ObligationSelection>,
    pub obligations: &'a [NodeObligation],
    pub ancestor_context: &'a str,
    pub plan_steps: &'a [PlanStepWithLinks],
    pub verdicts: &'a [ObligationVerdict],
    pub work_history: &'a str,
    pub findings: &'a [ReviewFinding],
    pub workspace: Option<&'a Workspace>,
    pub working_dir: Option<&'a Path>,
    pub focus: Option<&'a FocusSelection>,
    pub incoming: &'a [IncomingChangeItem],
    pub guidance: &'a str,
    pub environment: &'a str,
    pub skills: &'a str,
    pub pull_requests: &'a str,
    pub visual_design: Option<&'a VisualDesignState>,
}

/// One finding as a fix session is shown it: a line with its id, severity,
/// location, and summary, then its root cause and detail indented under it.
pub fn review_finding_lines(finding: &ReviewFinding) -> String {
    finding_lines(finding, "", true)
}

/// [`review_finding_lines`], indented by `indent`, with or without its root
/// cause (a group's header already names it).
fn finding_lines(finding: &ReviewFinding, indent: &str, with_root_cause: bool) -> String {
    let location = finding
        .location()
        .map(|l| format!(" `{l}`"))
        .unwrap_or_default();
    let mut out = format!(
        "{indent}- [{}] {}{location}: {}\n",
        tod_store::interview::short_id(finding.id),
        finding.severity,
        finding.summary.trim()
    );
    let root_cause = finding.root_cause.as_deref().map(str::trim).filter(|c| !c.is_empty());
    if with_root_cause && let Some(cause) = root_cause {
        out.push_str(&format!("{indent}  Root cause: {cause}\n"));
    }
    if let Some(detail) = finding.detail.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        for line in detail.lines() {
            out.push_str(indent);
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Findings as a fix session is shown them: those that share a root cause
/// under one entry naming it, so the fixer sees the class and fixes it once,
/// and the rest one by one ([`review_finding_lines`]).
pub fn review_findings_lines(findings: &[ReviewFinding]) -> String {
    let mut out = String::new();
    for group in tod_store::review::group_by_root_cause(findings) {
        match (group.root_cause, group.findings.as_slice()) {
            (Some(cause), [_, _, ..]) => {
                out.push_str(&format!(
                    "- Root cause shared by {} findings: {cause}\n",
                    group.findings.len()
                ));
                for finding in &group.findings {
                    out.push_str(&finding_lines(finding, "  ", false));
                }
            }
            _ => {
                for finding in &group.findings {
                    out.push_str(&review_finding_lines(finding));
                }
            }
        }
    }
    out
}

/// One verdict as a session is shown it: the obligation's id and status (with
/// how it was shown and what ran, when recorded), then the evidence indented
/// under it.
pub fn obligation_verdict_lines(verdict: &ObligationVerdict) -> String {
    let mut out = format!(
        "- [{}] {}",
        tod_store::interview::short_id(verdict.obligation_id),
        verdict.status
    );
    if let Some(method) = &verdict.method {
        out.push_str(&format!(" (method: {method})"));
    }
    out.push('\n');
    if let Some(ran) = verdict.ran.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        out.push_str(&format!("  ran: {ran}\n"));
    }
    for line in verdict.evidence.trim().lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// One incoming change: its headline, where it came from, then its before
/// and after text indented under it.
pub fn incoming_change_lines(item: &IncomingChangeItem) -> String {
    let mut out = format!(
        "- {} on \"{}\" (through {} {})\n",
        item.headline.trim(),
        item.source_title.trim(),
        if item.via == "reference" { "a" } else { "an" },
        item.via
    );
    for (label, text) in [("Before", &item.before), ("After", &item.after)] {
        let text = text.as_deref().map(str::trim).unwrap_or("(none)");
        out.push_str(&format!("  - {label}: {text}\n"));
    }
    out
}

/// Render `blocks`, in order, under a single `# Current context` heading.
pub fn render(blocks: &[DynamicBlock], ctx: &DynamicContext<'_>) -> String {
    let mut out = String::from("# Current context\n\n");
    for block in blocks {
        render_block(*block, ctx, &mut out);
    }
    out
}

/// The paragraph naming the app's own `tod-cli` by full path, for an agent on
/// this machine. `None` when there is no such binary (tests, a broken install).
fn host_cli_note() -> Option<String> {
    let cli = crate::interview::tod_cli_path();
    cli.is_file().then(|| {
        format!(
            "**`tod-cli`:** `{}` — call it by this full path if plain \
             `tod-cli` is not found. Never build or `cargo run` your own \
             copy against this data root.\n\n",
            cli.display().to_string().replace('\\', "/")
        )
    })
}

/// What an agent in a cloud sandbox or dev container is told instead of
/// [`host_cli_note`]: this machine's paths do not exist there.
const REMOTE_CLI_NOTE: &str = "**`tod-cli`:** run plain `tod-cli` (on your `PATH`). \
    It relays to the app, which uses its own data root, so the `--data-root` \
    path above is on the app's machine and does not exist on this filesystem: \
    never look for it or for a `tod-cli` binary here. If `tod-cli` is not \
    found, report that; do not build your own copy.\n\n";

/// `context` (an opening or snapshot built on this machine) adjusted for an
/// agent that runs in a sandbox or dev container: the host `tod-cli` path is
/// replaced by [`REMOTE_CLI_NOTE`], since the agent would otherwise call a
/// binary that is not there.
pub fn for_remote_agent(context: String) -> String {
    match host_cli_note() {
        Some(note) if context.contains(&note) => context.replacen(&note, REMOTE_CLI_NOTE, 1),
        _ => context,
    }
}

fn render_block(block: DynamicBlock, ctx: &DynamicContext<'_>, out: &mut String) {
    match block {
        DynamicBlock::DataRoot => {
            let Some(root) = ctx.data_root else { return };
            out.push_str(&format!(
                "**Data root:** `{}`\n\n\
                 Pass this to every `tod-cli` invocation as `--data-root`.\n\n",
                root.display()
            ));
            // The app puts tod-cli on the agent's PATH, but an agent's own
            // shell setup can lose it; the full path always works. Forward
            // slashes, so it runs unchanged from bash or PowerShell.
            if let Some(note) = host_cli_note() {
                out.push_str(&note);
            }
        }

        DynamicBlock::WorkingDirectory => {
            let Some(dir) = ctx.working_dir else { return };
            out.push_str(&format!(
                "**Working directory:** `{}`\n\n\
                 All code work happens here.\n\n",
                dir.display()
            ));
        }

        DynamicBlock::Node => {
            let Some(node) = ctx.node else { return };
            out.push_str("## Selected node\n\n");
            out.push_str(&format!("- **Id:** `{}`\n", node.id));
            out.push_str(&format!("- **Title:** {}\n", node.title.trim()));
            if let Some(slug) = node.slug.as_deref().filter(|s| !s.trim().is_empty()) {
                out.push_str(&format!("- **Slug:** `{}`\n", slug.trim()));
            }
            if let Some(lifecycle) = node.lifecycle.as_deref() {
                if !lifecycle.trim().is_empty() {
                    out.push_str(&format!("- **Lifecycle state:** {}\n", lifecycle.trim()));
                }
            }
            if let Some(body) = node.body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
                out.push_str("\n**Details:**\n\n");
                out.push_str(body);
                out.push('\n');
            }
        }

        DynamicBlock::NodeProcessFields => {
            let Some((mode, phase_purpose)) = ctx.process_fields else {
                return;
            };
            out.push_str(&format!(
                "- **mode:** {mode}\n- **phase_purpose:** {phase_purpose}\n"
            ));
        }

        DynamicBlock::SelectedObligation { fallback } => match ctx.obligation {
            Some(obligation) => {
                out.push_str("\n## Selected obligation\n\n");
                out.push_str(&format!("- **Id:** `{}`\n", obligation.id));
                out.push_str(&format!("- **Kind:** {}\n", obligation.kind));
                out.push_str("\n**Body:**\n\n");
                out.push_str(obligation.body.trim());
                out.push('\n');
                match obligation.visual_design_path.as_deref() {
                    Some(path) => out.push_str(&format!(
                        "\n**Visual design:** already has a mockup linked at `{path}` — \
                         saving again with `tod-cli visual-design save` replaces it.\n"
                    )),
                    None => out.push_str("\n**Visual design:** no mockup linked yet.\n"),
                }
                out.push_str(
                    "\nThe user has this obligation selected, so an unqualified question \
                     most likely refers to it.\n",
                );
            }
            None if !fallback.is_empty() => {
                out.push('\n');
                out.push_str(fallback);
                out.push('\n');
            }
            None => {}
        },

        DynamicBlock::NodeObligations { note } => {
            out.push_str("\n## Obligations\n\n");
            if !note.is_empty() {
                out.push_str(note);
                out.push_str("\n\n");
            }
            if ctx.obligations.is_empty() {
                out.push_str("(none)\n");
            } else {
                for o in ctx.obligations {
                    out.push_str("- ");
                    out.push_str(&obligation_line(o));
                    out.push('\n');
                }
            }
        }

        DynamicBlock::VisualDesign => {
            if let Some(state) = ctx.visual_design {
                out.push_str("\n## Mockup\n\n");
                out.push_str(&format!(
                    "- **Working draft (edit this file):** `{}`\n",
                    state.draft_path
                ));
                match state.saved_path.as_deref() {
                    Some(saved) => {
                        out.push_str(&format!("- **Saved mockup:** `{saved}`\n"));
                        out.push_str(if state.differs {
                            "- The draft differs from the saved mockup.\n"
                        } else {
                            "- The draft matches the saved mockup.\n"
                        });
                    }
                    None => out.push_str("- **Saved mockup:** none linked yet.\n"),
                }
            }
        }

        DynamicBlock::AncestorContext => {
            if !ctx.ancestor_context.is_empty() {
                out.push_str(ctx.ancestor_context);
            }
        }

        DynamicBlock::Plan => {
            out.push_str("\n## Plan steps\n\n");
            if ctx.plan_steps.is_empty() {
                out.push_str("(none)\n");
            } else {
                for entry in ctx.plan_steps {
                    out.push_str("- ");
                    out.push_str(&plan_step_line(
                        &entry.step,
                        &entry.depends_on,
                        &entry.satisfies,
                    ));
                    out.push('\n');
                }
            }
        }

        DynamicBlock::ObligationVerdicts => {
            if !ctx.verdicts.is_empty() {
                out.push_str(
                    "\n## Verification verdicts\n\n\
                     What verification last found for each obligation it has ruled \
                     on. An obligation not listed here is unchecked.\n\n",
                );
                for verdict in ctx.verdicts {
                    out.push_str(&obligation_verdict_lines(verdict));
                }
            }
        }

        DynamicBlock::WorkHistory => {
            if !ctx.work_history.is_empty() {
                out.push_str(ctx.work_history);
            }
        }

        DynamicBlock::ReviewFindings => {
            out.push_str("\n## Open review findings\n\n");
            if ctx.findings.is_empty() {
                out.push_str("(none)\n");
            } else {
                out.push_str(&review_findings_lines(ctx.findings));
            }
        }

        DynamicBlock::IncomingChanges => {
            out.push_str("\n## Incoming changes\n\n");
            if ctx.incoming.is_empty() {
                out.push_str("(none)\n");
            }
            for item in ctx.incoming {
                out.push_str(&incoming_change_lines(item));
            }
        }

        DynamicBlock::GuidanceSelection => {
            out.push_str("\n## Guidance to select from\n\n");
            out.push_str(ctx.guidance.trim());
            out.push('\n');
        }

        DynamicBlock::Focus => {
            let Some(focus) = ctx.focus else { return };
            let kind = match focus.focus {
                Focus::Project => "the whole project",
                Focus::Node(_) => "node",
                Focus::Obligation { .. } => "obligation",
                Focus::PlanStep { .. } => "plan step",
            };
            out.push_str("\n## Focus\n\n");
            out.push_str(
                "What this conversation is about. It is a starting point, not a \
                 boundary.\n\n",
            );
            out.push_str(&format!("- **Kind:** {kind}\n"));
            if let Some(id) = focus.focus.focus_id() {
                out.push_str(&format!("- **Id:** `{id}`\n"));
            }
            if let Some(node) = focus
                .focus
                .node_id()
                .filter(|n| Some(*n) != focus.focus.focus_id())
            {
                out.push_str(&format!("- **Node id:** `{node}`\n"));
            }
            if !focus.path.is_empty() {
                out.push_str(&format!("- **Path:** {}\n", focus.path.join(" › ")));
            }
            out.push_str(&format!("- **Title:** {}\n", focus.title.trim()));
            if let Some(slug) = focus.slug.as_deref().filter(|s| !s.trim().is_empty()) {
                out.push_str(&format!("- **Slug:** `{}`\n", slug.trim()));
            }
            if let Some(text) = focus.text.as_deref().map(str::trim) {
                out.push_str("\n**Text:**\n\n");
                out.push_str(text);
                out.push('\n');
            }
            for (heading, lines) in &focus.sections {
                out.push_str(&format!("\n### {heading}\n\n"));
                if lines.is_empty() {
                    out.push_str("(none)\n");
                }
                for line in lines {
                    out.push_str("- ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }

        DynamicBlock::Environment => {
            if !ctx.environment.is_empty() {
                out.push_str(ctx.environment);
            }
        }

        DynamicBlock::PullRequests => {
            if !ctx.pull_requests.is_empty() {
                out.push_str(ctx.pull_requests);
            }
        }

        DynamicBlock::ConfiguredSkills => {
            if !ctx.skills.is_empty() {
                out.push_str(ctx.skills);
            }
        }

        DynamicBlock::Databases => {
            out.push_str(&crate::databases_context::render(&crate::databases_context::declared(ctx.data_root)));
            out.push_str(&crate::access_context::render(&crate::access_context::declared(ctx.data_root)));
        }

        DynamicBlock::Workspace => {
            let Some(workspace) = ctx.workspace else { return };
            out.push_str("\n## Workspace\n\n");
            out.push_str(&format!(
                "- **Repository:** {}\n",
                workspace.repo.as_deref().unwrap_or("(not set)")
            ));
            out.push_str(&format!(
                "- **Branch:** {}\n",
                workspace.branch.as_deref().unwrap_or("(default)")
            ));
            out.push_str(&format!("- **Working directory:** {}\n", workspace.cwd));
            out.push_str("\n**Notes:**\n\n");
            if workspace.notes.is_empty() {
                out.push_str("(none)\n");
            } else {
                for note in &workspace.notes {
                    out.push_str(&format!("- {note}\n"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> NodeSelection {
        NodeSelection {
            id: Uuid::nil(),
            title: "Obligations Panel".into(),
            body: Some("Panel for editing direct obligations.".into()),
            lifecycle: Some("active".into()),
            slug: Some("obligations-panel".into()),
        }
    }

    #[test]
    fn blocks_render_in_the_order_listed() {
        let node = node();
        let ctx = DynamicContext {
            data_root: Some(Path::new("/data/tod")),
            node: Some(&node),
            ancestor_context: "\n## Inherited context (ancestors)\n",
            ..Default::default()
        };
        let text = render(
            &[
                DynamicBlock::DataRoot,
                DynamicBlock::AncestorContext,
                DynamicBlock::Node,
            ],
            &ctx,
        );
        let root = text.find("Data root").unwrap();
        let ancestors = text.find("## Inherited context").unwrap();
        let node_idx = text.find("## Selected node").unwrap();
        assert!(root < ancestors && ancestors < node_idx);
        assert!(text.contains(&Uuid::nil().to_string()));
        assert!(text.contains("Panel for editing direct obligations."));
    }

    #[test]
    fn focus_renders_kind_path_title_text_and_listings() {
        let step = Uuid::from_u128(9);
        let node = Uuid::from_u128(3);
        let focus = FocusSelection {
            focus: Focus::PlanStep { node, id: step },
            path: vec!["Root".into(), "Auth".into()],
            node: Some(node),
            title: "Plan step 00000000".into(),
            slug: Some("auth".into()),
            text: Some("Hash passwords with argon2.".into()),
            sections: Vec::new(),
        };
        let ctx = DynamicContext {
            focus: Some(&focus),
            ..Default::default()
        };
        let text = render(&[DynamicBlock::Focus], &ctx);
        assert!(text.contains("**Kind:** plan step"), "{text}");
        assert!(text.contains(&step.to_string()));
        assert!(text.contains(&node.to_string()));
        assert!(text.contains("Root › Auth"));
        assert!(text.contains("Hash passwords with argon2."));

        let project = FocusSelection {
            focus: Focus::Project,
            path: Vec::new(),
            node: None,
            title: "The whole project".into(),
            slug: None,
            text: None,
            sections: vec![("Top-level nodes".into(), vec!["`id` Auth".into()])],
        };
        let text = render(
            &[DynamicBlock::Focus],
            &DynamicContext {
                focus: Some(&project),
                ..Default::default()
            },
        );
        assert!(text.contains("**Kind:** the whole project"), "{text}");
        assert!(!text.contains("**Id:**"), "{text}");
        assert!(
            text.contains("### Top-level nodes\n\n- `id` Auth"),
            "{text}"
        );
    }

    #[test]
    fn a_block_whose_data_is_absent_renders_nothing() {
        let text = render(
            &[
                DynamicBlock::DataRoot,
                DynamicBlock::AncestorContext,
                DynamicBlock::Focus,
            ],
            &DynamicContext::default(),
        );
        assert_eq!(text, "# Current context\n\n");
    }

    #[test]
    fn selected_obligation_contributes_id_kind_and_body() {
        let obligation = ObligationSelection {
            id: Uuid::from_u128(7),
            kind: "requirement".into(),
            body: "Must round-trip".into(),
            visual_design_path: None,
        };
        let ctx = DynamicContext {
            obligation: Some(&obligation),
            ..Default::default()
        };
        let text = render(&[DynamicBlock::SelectedObligation { fallback: "" }], &ctx);
        assert!(text.contains(&Uuid::from_u128(7).to_string()));
        assert!(text.contains("requirement"));
        assert!(text.contains("Must round-trip"));
    }

    /// The fallback replaced a `surface == "obligations"` branch inside the
    /// renderer: it is the surface's wording, not the renderer's knowledge of
    /// which surface it is serving.
    #[test]
    fn fallback_is_emitted_only_when_the_surface_supplies_one() {
        let with = render(
            &[DynamicBlock::SelectedObligation {
                fallback: "No individual obligation is selected.",
            }],
            &DynamicContext::default(),
        );
        assert!(with.contains("No individual obligation is selected."));

        let without = render(
            &[DynamicBlock::SelectedObligation { fallback: "" }],
            &DynamicContext::default(),
        );
        assert_eq!(without, "# Current context\n\n");
    }

    fn finding(seq: u128, summary: &str, root_cause: Option<&str>) -> ReviewFinding {
        ReviewFinding {
            id: Uuid::from_u128(seq << 120),
            node_id: Uuid::nil(),
            conversation_id: None,
            seq: seq as i64,
            severity: "high".into(),
            file: Some("a.py".into()),
            line: Some(seq as i64),
            summary: summary.into(),
            detail: Some("How it goes wrong.".into()),
            status: "open".into(),
            response: None,
            created_at: 0,
            updated_at: 0,
            pr_thread: None,
            pr_posted_at: None,
            root_cause: root_cause.map(str::to_string),
        }
    }

    /// Findings that share a root cause are shown as one class; a lone root
    /// cause stays on its finding.
    #[test]
    fn findings_that_share_a_root_cause_are_shown_together() {
        let findings = [
            finding(1, "First", Some("The id is treated as unique")),
            finding(2, "Second", Some("Nulls are not handled")),
            finding(3, "Third", Some("the id is treated as unique")),
            finding(4, "Fourth", None),
        ];
        let text = review_findings_lines(&findings);
        assert_eq!(
            text,
            "- Root cause shared by 2 findings: The id is treated as unique\n\
             \x20 - [01000000] high `a.py:1`: First\n\
             \x20   How it goes wrong.\n\
             \x20 - [03000000] high `a.py:3`: Third\n\
             \x20   How it goes wrong.\n\
             - [02000000] high `a.py:2`: Second\n\
             \x20 Root cause: Nulls are not handled\n\
             \x20 How it goes wrong.\n\
             - [04000000] high `a.py:4`: Fourth\n\
             \x20 How it goes wrong.\n"
        );
    }
}
