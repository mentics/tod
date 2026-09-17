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

/// One section of the dynamic block. A surface lists the ones it wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicBlock {
    /// The data root, and the instruction to pass it to every `tod-cli` call.
    DataRoot,
    /// Inherited purpose, most general first. Omitted when there is none.
    PurposeChain,
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
    /// Pre-rendered ancestor context (see
    /// `crate::node_context::render_inherited_context`).
    AncestorContext,
    /// This node's plan steps with their dependency and `satisfies` links.
    Plan,
    /// Repo, branch, working directory, and task notes.
    Workspace,
    /// What a conversation is about: its kind, path, title, and (by kind) its
    /// full text, its obligations and plan steps, or the top-level nodes.
    Focus,
}

/// Everything any block might need. A surface fills in what its blocks use and
/// leaves the rest at its default.
#[derive(Debug, Default, Clone)]
pub struct DynamicContext<'a> {
    pub data_root: Option<&'a Path>,
    pub node: Option<&'a NodeSelection>,
    /// `(mode, phase_purpose)` for [`DynamicBlock::NodeProcessFields`].
    pub process_fields: Option<(&'a str, &'a str)>,
    pub purposes: &'a [String],
    pub obligation: Option<&'a ObligationSelection>,
    pub obligations: &'a [NodeObligation],
    pub ancestor_context: &'a str,
    pub plan_steps: &'a [PlanStepWithLinks],
    pub workspace: Option<&'a Workspace>,
    pub focus: Option<&'a FocusSelection>,
}

/// Render `blocks`, in order, under a single `# Current context` heading.
pub fn render(blocks: &[DynamicBlock], ctx: &DynamicContext<'_>) -> String {
    let mut out = String::from("# Current context\n\n");
    for block in blocks {
        render_block(*block, ctx, &mut out);
    }
    out
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
        }

        DynamicBlock::PurposeChain => {
            if ctx.purposes.is_empty() {
                return;
            }
            out.push_str("## Purpose\n\n");
            out.push_str(
                "From the top of the tree down to the selected node, most general first:\n\n",
            );
            for purpose in ctx.purposes {
                out.push_str(purpose);
                out.push_str("\n\n");
            }
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
        let purposes = vec!["Ship the product.".into(), "Ship this feature.".into()];
        let ctx = DynamicContext {
            data_root: Some(Path::new("/data/tod")),
            node: Some(&node),
            purposes: &purposes,
            ..Default::default()
        };
        let text = render(
            &[
                DynamicBlock::DataRoot,
                DynamicBlock::PurposeChain,
                DynamicBlock::Node,
            ],
            &ctx,
        );
        let root = text.find("Data root").unwrap();
        let purpose = text.find("## Purpose").unwrap();
        let general = text.find("Ship the product.").unwrap();
        let specific = text.find("Ship this feature.").unwrap();
        let node_idx = text.find("## Selected node").unwrap();
        assert!(root < purpose && purpose < general);
        assert!(general < specific, "purposes must go general to specific");
        assert!(specific < node_idx);
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
                DynamicBlock::PurposeChain,
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
}
