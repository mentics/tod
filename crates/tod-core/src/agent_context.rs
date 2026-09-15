//! Assembles the first message sent when a user opens an agent chat from
//! somewhere in the app.
//!
//! The message is static context (versioned docs from `media/context/`, see
//! [`crate::media`]) followed by a dynamic block describing what the user
//! currently has selected. Both ids and text are included: the agent can work
//! with the text directly and only needs `tod-cli` for what it was not given.

use crate::gate::PlanStepWithLinks;
use crate::interview::context::{obligation_line, plan_step_line};
use crate::media::{MediaPaths, load_static_context};
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
    /// Context key under `media/context/` (e.g. `"obligations"`).
    pub key: &'a str,
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
    let mut out = load_static_context(paths, request.key)?;
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
        out.push_str(
            "From the top of the tree down to the selected node, most general first:\n\n",
        );
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
    } else if request.key == "obligations" {
        out.push_str(
            "\nNo individual obligation is selected — the user is looking at the \
             node's obligations as a whole.\n",
        );
    }

    out
}

/// Context key under `media/context/` for an implementation session, launched
/// from the lifecycle panel's Active-phase "Implement" button (see
/// `crate::gate` for the transition gate that requires Agent and Files
/// before a node can reach `active` at all).
pub const IMPLEMENT_CONTEXT_KEY: &str = "active/implement";

/// One node's own obligations, paired with its title, for rendering the full
/// ancestor-to-node obligation hierarchy an implementation session gets
/// up front.
#[derive(Debug, Clone)]
pub struct ImplementNodeObligations {
    pub node_title: String,
    pub obligations: Vec<NodeObligation>,
}

/// Everything an implementation session's first-turn context describes.
#[derive(Debug, Clone)]
pub struct ImplementRequest<'a> {
    pub data_root: &'a Path,
    pub node: NodeSelection,
    /// This node's plan steps with their dependency and `--satisfies` links.
    pub plan_steps: Vec<PlanStepWithLinks>,
    /// Obligations for this node and every ancestor, most general (root)
    /// first, ending with the node itself.
    pub obligation_hierarchy: Vec<ImplementNodeObligations>,
}

/// Build the full implementation-session first message: static layers (app.md
/// then active.md then active/implement.md, per the usual context layering),
/// then the inlined plan and obligation hierarchy.
pub fn build_implement_message(paths: &MediaPaths, request: &ImplementRequest<'_>) -> Result<String> {
    let mut out = load_static_context(paths, IMPLEMENT_CONTEXT_KEY)?;
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
            out.push_str(&plan_step_line(&entry.step, &entry.depends_on, &entry.satisfies));
            out.push('\n');
        }
    }

    out.push_str(
        "\n## Obligation hierarchy\n\n\
         From the root of the tree down to this node, most general first:\n\n",
    );
    if request.obligation_hierarchy.is_empty() {
        out.push_str("(none)\n");
    } else {
        for level in &request.obligation_hierarchy {
            out.push_str(&format!("### {}\n\n", level.node_title.trim()));
            if level.obligations.is_empty() {
                out.push_str("(none)\n\n");
                continue;
            }
            for o in &level.obligations {
                out.push_str("- ");
                out.push_str(&obligation_line(o));
                out.push('\n');
            }
            out.push('\n');
        }
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
            key: "obligations",
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
            key: "obligations",
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
            key: "design/visual-design",
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
            key: "obligations",
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
