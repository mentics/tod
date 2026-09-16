//! Assembles the first message sent when a user opens an agent chat from
//! somewhere in the app.
//!
//! The message is static context (versioned docs from `media/context/`, see
//! [`crate::media`]) followed by a dynamic block describing what the user
//! currently has selected. Both ids and text are included: the agent can work
//! with the text directly and only needs `tod-cli` for what it was not given.

use crate::gate::PlanStepWithLinks;
use crate::media::{MediaPaths, load_static_context};
use crate::node_context::{obligation_line, plan_step_line};
use anyhow::Result;
use std::path::Path;
use tod_store::outline::NodeObligation;
use uuid::Uuid;

/// The node an agent chat was opened against.
#[derive(Debug, Clone)]
pub struct NodeSelection {
    pub id: Uuid,
    pub title: String,
    /// Node body/details, when the node has any.
    pub body: Option<String>,
    pub lifecycle: Option<String>,
}

/// A specific obligation selected within the obligations panel.
#[derive(Debug, Clone)]
pub struct ObligationSelection {
    pub id: Uuid,
    pub kind: String,
    pub body: String,
    /// Path of the obligation's currently-linked visual-design mockup, when
    /// one exists (see `tod-cli visual-design`). `None` if this obligation
    /// has no mockup yet.
    pub visual_design_path: Option<String>,
}

/// Everything the dynamic half of the first message describes.
#[derive(Debug, Clone)]
pub struct ContextRequest<'a> {
    /// Which surface opened this chat (e.g. `"obligations"`,
    /// `"design/visual-design"`), for the surface-specific wording in the
    /// dynamic block below. Not the list of static fragments to load — see
    /// `layers`.
    pub surface: &'a str,
    /// Static context fragments to load, in order — see
    /// `crate::media::load_static_context`. Each surface picks exactly the
    /// fragments it needs (e.g. interactive surfaces add `"interactive"`;
    /// surfaces that never call `tod-cli` leave out `"tod_cli"`), so nothing
    /// surface-specific leaks into a surface that doesn't want it.
    pub layers: &'a [&'a str],
    pub data_root: &'a Path,
    pub node: NodeSelection,
    pub obligation: Option<ObligationSelection>,
    /// Purpose (Spec capability's `goal`) for the selected node and every
    /// ancestor that has one, ordered from the root of the tree down to the
    /// selected node — general inherited purpose first, this node's own
    /// purpose last.
    pub purposes: Vec<String>,
}

/// Build the full first message: static layers, then the live selection.
pub fn build_first_message(paths: &MediaPaths, request: &ContextRequest<'_>) -> Result<String> {
    let mut out = load_static_context(paths, request.layers)?;
    out.push_str("\n\n---\n\n");
    out.push_str(&render_dynamic(request));
    Ok(out)
}

fn render_dynamic(request: &ContextRequest<'_>) -> String {
    let mut out = String::from("# Current context\n\n");

    out.push_str(&format!(
        "**Data root:** `{}`\n\n\
         Pass this to every `tod-cli` invocation as `--data-root`.\n\n",
        request.data_root.display()
    ));

    if !request.purposes.is_empty() {
        out.push_str("## Purpose\n\n");
        out.push_str("From the top of the tree down to the selected node, most general first:\n\n");
        for purpose in &request.purposes {
            out.push_str(purpose);
            out.push_str("\n\n");
        }
    }

    out.push_str("## Selected node\n\n");
    out.push_str(&format!("- **Id:** `{}`\n", request.node.id));
    out.push_str(&format!("- **Title:** {}\n", request.node.title.trim()));
    if let Some(lifecycle) = request.node.lifecycle.as_deref() {
        if !lifecycle.trim().is_empty() {
            out.push_str(&format!("- **Lifecycle state:** {}\n", lifecycle.trim()));
        }
    }
    match request.node.body.as_deref().map(str::trim) {
        Some(body) if !body.is_empty() => {
            out.push_str("\n**Details:**\n\n");
            out.push_str(body);
            out.push('\n');
        }
        _ => {}
    }

    if let Some(obligation) = request.obligation.as_ref() {
        out.push_str("\n## Selected obligation\n\n");
        out.push_str(&format!("- **Id:** `{}`\n", obligation.id));
        out.push_str(&format!("- **Kind:** {}\n", obligation.kind));
        out.push_str("\n**Body:**\n\n");
        out.push_str(obligation.body.trim());
        out.push('\n');
        match obligation.visual_design_path.as_deref() {
            Some(path) => {
                out.push_str(&format!(
                    "\n**Visual design:** already has a mockup linked at `{path}` — saving \
                     again with `tod-cli visual-design save` replaces it.\n"
                ));
            }
            None => {
                out.push_str("\n**Visual design:** no mockup linked yet.\n");
            }
        }
        out.push_str(
            "\nThe user has this obligation selected, so an unqualified question \
             most likely refers to it.\n",
        );
    } else if request.surface == "obligations" {
        out.push_str(
            "\nNo individual obligation is selected — the user is looking at the \
             node's obligations as a whole.\n",
        );
    }

    out
}

/// Static context fragments for the obligations-panel chat. Interactive, and
/// the one surface with a scoped exception to "confirm first" — see
/// `surface/obligations.md`, which states that exception in terms of the
/// stance rather than flatly contradicting it.
pub const OBLIGATIONS_CONTEXT_LAYERS: &[&str] = &[
    "stance/interactive-chat",
    "domain/outline",
    "domain/obligations",
    "domain/plan",
    "cli/intro",
    "cli/node",
    "cli/obligations",
    "cli/plan",
    "surface/obligations",
];

/// Static context fragments for the visual-design chat. It mutates through
/// exactly one command, so it loads `cli/visual-design` and none of the other
/// nouns.
pub const VISUAL_DESIGN_CONTEXT_LAYERS: &[&str] = &[
    "stance/interactive-chat",
    "domain/outline",
    "domain/obligations",
    "cli/intro",
    "cli/visual-design",
    "surface/visual-design",
];

/// Surface key for an implementation session, used for session naming (see
/// `session_name::session_name_for`) — not a media context key.
pub const IMPLEMENT_SURFACE_KEY: &str = "active/implement";

/// Static context fragments for an implementation session, launched from the
/// lifecycle panel's Active-phase "Implement" button (see `crate::gate` for
/// the transition gate that requires Agent and Files before a node can reach
/// `active` at all).
///
/// Stance is `autonomous-session`, not `interactive-chat`: nobody is waiting
/// at a prompt, so the agent must act without confirming and may be as verbose
/// as the work needs. Domain covers what it handles (outline, obligations,
/// plan, lifecycle) and no more; the CLI nouns are the two it actually writes
/// through. See `doc/agent-context-map.md` for the full per-surface map.
pub const IMPLEMENT_CONTEXT_LAYERS: &[&str] = &[
    "stance/autonomous-session",
    "domain/outline",
    "domain/obligations",
    "domain/plan",
    "domain/lifecycle",
    "cli/intro",
    "cli/obligations",
    "cli/plan",
    "surface/implement",
];

/// The user-visible message auto-submitted on an implementation session's
/// first turn — the Implement button means "go implement this now", so the
/// session shouldn't sit waiting on the user to type that themselves.
pub const IMPLEMENT_START_MESSAGE: &str = "Go implement this.";

/// Everything an implementation session's first-turn context describes.
#[derive(Debug, Clone)]
pub struct ImplementRequest<'a> {
    pub data_root: &'a Path,
    pub node: NodeSelection,
    /// This node's plan steps with their dependency and `--satisfies` links.
    pub plan_steps: Vec<PlanStepWithLinks>,
    /// This node's own obligations (requirements/constraints) in full — they
    /// define what "done" means for this session. Not ancestors' — those
    /// come in `ancestor_context` instead.
    pub obligations: Vec<NodeObligation>,
    /// Rendered ancestor context — build with
    /// `tod_core::node_context::render_inherited_context`, the one place
    /// this policy (each ancestor's title, generated summary, and
    /// constraints, never its full requirements) is implemented. Callers
    /// build this from a live connection since `ImplementRequest` itself
    /// carries no DB handle.
    pub ancestor_context: String,
}

/// Build the full implementation-session first message: static layers (app.md
/// then active.md then active/implement.md, per the usual context layering),
/// then the inlined plan and obligation hierarchy.
pub fn build_implement_message(
    paths: &MediaPaths,
    request: &ImplementRequest<'_>,
) -> Result<String> {
    let mut out = load_static_context(paths, IMPLEMENT_CONTEXT_LAYERS)?;
    out.push_str("\n\n---\n\n");
    out.push_str(&render_implement_dynamic(request));
    Ok(out)
}

fn render_implement_dynamic(request: &ImplementRequest<'_>) -> String {
    let mut out = String::from("# Current context\n\n");

    out.push_str(&format!(
        "**Data root:** `{}`\n\n\
         Pass this to every `tod-cli` invocation as `--data-root`.\n\n",
        request.data_root.display()
    ));

    out.push_str("## Node\n\n");
    out.push_str(&format!("- **Id:** `{}`\n", request.node.id));
    out.push_str(&format!("- **Title:** {}\n", request.node.title.trim()));
    if let Some(lifecycle) = request.node.lifecycle.as_deref() {
        if !lifecycle.trim().is_empty() {
            out.push_str(&format!("- **Lifecycle state:** {}\n", lifecycle.trim()));
        }
    }
    match request.node.body.as_deref().map(str::trim) {
        Some(body) if !body.is_empty() => {
            out.push_str("\n**Details:**\n\n");
            out.push_str(body);
            out.push('\n');
        }
        _ => {}
    }

    out.push_str("\n## Plan\n\n");
    if request.plan_steps.is_empty() {
        out.push_str("(no plan steps)\n");
    } else {
        for entry in &request.plan_steps {
            out.push_str("- ");
            out.push_str(&plan_step_line(
                &entry.step,
                &entry.depends_on,
                &entry.satisfies,
            ));
            out.push('\n');
        }
    }

    out.push_str("\n## Obligations\n\n");
    out.push_str(
        "This node's own — they define what \"done\" means for this \
         session, not the summarized ancestor context below.\n\n",
    );
    if request.obligations.is_empty() {
        out.push_str("(none)\n");
    } else {
        for o in &request.obligations {
            out.push_str("- ");
            out.push_str(&obligation_line(o));
            out.push('\n');
        }
    }

    if !request.ancestor_context.is_empty() {
        out.push_str(&request.ancestor_context);
    }

    out
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
        }
    }

    #[test]
    fn includes_data_root_and_node_identity_and_text() {
        let req = ContextRequest {
            surface: "obligations",
            layers: &["obligations"],
            data_root: Path::new("/data/tod"),
            node: node(),
            obligation: None,
            purposes: Vec::new(),
        };
        let text = render_dynamic(&req);
        assert!(text.contains("data") && text.contains("tod"));
        assert!(text.contains(&Uuid::nil().to_string()));
        assert!(text.contains("Obligations Panel"));
        assert!(text.contains("Panel for editing direct obligations."));
        assert!(text.contains("No individual obligation is selected"));
    }

    #[test]
    fn purposes_render_general_to_specific_before_selected_node() {
        let req = ContextRequest {
            surface: "obligations",
            layers: &["obligations"],
            data_root: Path::new("/data/tod"),
            node: node(),
            obligation: None,
            purposes: vec!["Ship the product.".into(), "Ship this feature.".into()],
        };
        let text = render_dynamic(&req);
        let purpose_idx = text.find("## Purpose").unwrap();
        let ship_product_idx = text.find("Ship the product.").unwrap();
        let ship_feature_idx = text.find("Ship this feature.").unwrap();
        let node_idx = text.find("## Selected node").unwrap();
        assert!(purpose_idx < ship_product_idx);
        assert!(ship_product_idx < ship_feature_idx);
        assert!(ship_feature_idx < node_idx);
    }

    #[test]
    fn non_obligations_key_omits_obligations_panel_wording() {
        let req = ContextRequest {
            surface: "design/visual-design",
            layers: &["design/visual-design"],
            data_root: Path::new("/data/tod"),
            node: node(),
            obligation: None,
            purposes: Vec::new(),
        };
        let text = render_dynamic(&req);
        assert!(!text.contains("No individual obligation is selected"));
    }

    #[test]
    fn selected_obligation_contributes_id_and_body() {
        let req = ContextRequest {
            surface: "obligations",
            layers: &["obligations"],
            data_root: Path::new("/data/tod"),
            node: node(),
            obligation: Some(ObligationSelection {
                id: Uuid::from_u128(7),
                kind: "requirement".into(),
                body: "Must round-trip".into(),
                visual_design_path: None,
            }),
            purposes: Vec::new(),
        };
        let text = render_dynamic(&req);
        assert!(text.contains(&Uuid::from_u128(7).to_string()));
        assert!(text.contains("requirement"));
        assert!(text.contains("Must round-trip"));
    }
}
