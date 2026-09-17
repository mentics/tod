//! Assembles the first message sent when a user opens an agent chat from
//! somewhere in the app, and the one sent to an implementation session.
//!
//! Both are `media/context/` fragments followed by dynamic blocks; which
//! fragments and which blocks is the surface's recipe (see
//! [`crate::context_recipes`]), and the assembly itself is
//! [`crate::context_recipes::build_message`]. This module's job is turning the
//! caller's request into a [`DynamicContext`].

use crate::context_recipes::{IMPLEMENT_SESSION, VISUAL_DESIGN_CHAT};
use crate::dynamic::DynamicContext;
use crate::gate::PlanStepWithLinks;
use crate::media::MediaPaths;
use anyhow::Result;
use std::path::Path;
use tod_store::outline::NodeObligation;

pub use crate::context_recipes::ContextRecipe;
pub use crate::dynamic::{NodeSelection, ObligationSelection};

/// Everything the dynamic half of a chat's first message describes.
#[derive(Debug, Clone)]
pub struct ContextRequest<'a> {
    /// The surface's recipe — its static fragments and dynamic blocks.
    pub recipe: &'a ContextRecipe,
    pub data_root: &'a Path,
    pub node: NodeSelection,
    pub obligation: Option<ObligationSelection>,
    /// Purpose (Spec capability's `goal`) for the selected node and every
    /// ancestor that has one, ordered from the root of the tree down to the
    /// selected node — general inherited purpose first, this node's own
    /// purpose last.
    pub purposes: Vec<String>,
}

/// The recipe for a chat opened from the visual-design panel.
pub const VISUAL_DESIGN_RECIPE: &ContextRecipe = &VISUAL_DESIGN_CHAT;

/// Build the full first message: static fragments, then the live selection.
pub fn build_first_message(paths: &MediaPaths, request: &ContextRequest<'_>) -> Result<String> {
    crate::context_recipes::build_message(
        paths,
        request.recipe,
        None,
        &DynamicContext {
            data_root: Some(request.data_root),
            node: Some(&request.node),
            purposes: &request.purposes,
            obligation: request.obligation.as_ref(),
            ..Default::default()
        },
        "",
    )
}

/// Surface key for an implementation session, used for session naming (see
/// `session_name::session_name_for`) — not a media context key.
pub const IMPLEMENT_SURFACE_KEY: &str = "active/implement";

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
    /// define what "done" means for this session. Not ancestors' — those come
    /// in `ancestor_context` instead.
    pub obligations: Vec<NodeObligation>,
    /// Rendered ancestor context — build with
    /// `tod_core::node_context::render_inherited_context`, the one place this
    /// policy (each ancestor's title, generated summary, and constraints,
    /// never its full requirements) is implemented. Callers build this from a
    /// live connection since `ImplementRequest` itself carries no DB handle.
    pub ancestor_context: String,
}

/// Build the full implementation-session first message.
pub fn build_implement_message(
    paths: &MediaPaths,
    request: &ImplementRequest<'_>,
) -> Result<String> {
    crate::context_recipes::build_message(
        paths,
        &IMPLEMENT_SESSION,
        None,
        &DynamicContext {
            data_root: Some(request.data_root),
            node: Some(&request.node),
            obligations: &request.obligations,
            ancestor_context: &request.ancestor_context,
            plan_steps: &request.plan_steps,
            ..Default::default()
        },
        "",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic::{DynamicBlock, render};
    use uuid::Uuid;

    /// A chat recipe with a selection fallback, standing in for a surface
    /// that shows a node's obligations as a whole.
    const NODE_CHAT: &ContextRecipe = &ContextRecipe {
        name: "test node chat",
        layers: &[],
        blocks: &[
            DynamicBlock::DataRoot,
            DynamicBlock::PurposeChain,
            DynamicBlock::Node,
            DynamicBlock::SelectedObligation {
                fallback: "No individual obligation is selected",
            },
        ],
    };

    fn node() -> NodeSelection {
        NodeSelection {
            id: Uuid::nil(),
            title: "Obligations Panel".into(),
            body: Some("Panel for editing direct obligations.".into()),
            lifecycle: Some("active".into()),
            slug: Some("obligations-panel".into()),
        }
    }

    fn dynamic_for(request: &ContextRequest<'_>) -> String {
        render(
            request.recipe.blocks,
            &DynamicContext {
                data_root: Some(request.data_root),
                node: Some(&request.node),
                purposes: &request.purposes,
                obligation: request.obligation.as_ref(),
                ..Default::default()
            },
        )
    }

    #[test]
    fn includes_data_root_and_node_identity_and_text() {
        let req = ContextRequest {
            recipe: NODE_CHAT,
            data_root: Path::new("/data/tod"),
            node: node(),
            obligation: None,
            purposes: Vec::new(),
        };
        let text = dynamic_for(&req);
        assert!(text.contains("data") && text.contains("tod"));
        assert!(text.contains(&Uuid::nil().to_string()));
        assert!(text.contains("Obligations Panel"));
        assert!(text.contains("Panel for editing direct obligations."));
        assert!(text.contains("No individual obligation is selected"));
    }

    #[test]
    fn purposes_render_general_to_specific_before_selected_node() {
        let req = ContextRequest {
            recipe: NODE_CHAT,
            data_root: Path::new("/data/tod"),
            node: node(),
            obligation: None,
            purposes: vec!["Ship the product.".into(), "Ship this feature.".into()],
        };
        let text = dynamic_for(&req);
        let purpose_idx = text.find("## Purpose").unwrap();
        let ship_product_idx = text.find("Ship the product.").unwrap();
        let ship_feature_idx = text.find("Ship this feature.").unwrap();
        let node_idx = text.find("## Selected node").unwrap();
        assert!(purpose_idx < ship_product_idx);
        assert!(ship_product_idx < ship_feature_idx);
        assert!(ship_feature_idx < node_idx);
    }

    /// A selection fallback is one recipe's wording, so a different surface
    /// must not pick it up. This used to be a `surface == "obligations"`
    /// branch inside the renderer.
    #[test]
    fn another_surface_omits_the_obligations_panel_wording() {
        let req = ContextRequest {
            recipe: VISUAL_DESIGN_RECIPE,
            data_root: Path::new("/data/tod"),
            node: node(),
            obligation: None,
            purposes: Vec::new(),
        };
        assert!(!dynamic_for(&req).contains("No individual obligation is selected"));
    }

    #[test]
    fn selected_obligation_contributes_id_and_body() {
        let req = ContextRequest {
            recipe: NODE_CHAT,
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
        let text = dynamic_for(&req);
        assert!(text.contains(&Uuid::from_u128(7).to_string()));
        assert!(text.contains("requirement"));
        assert!(text.contains("Must round-trip"));
    }

    /// An implementation session's own obligations must precede the
    /// summarized ancestor context they are contrasted against.
    #[test]
    fn implement_renders_plan_then_own_obligations_then_ancestors() {
        let blocks = IMPLEMENT_SESSION.blocks;
        let pos = |b: &DynamicBlock| blocks.iter().position(|x| x == b).unwrap();
        assert!(pos(&DynamicBlock::Plan) < pos(&DynamicBlock::AncestorContext));
        assert!(
            blocks
                .iter()
                .position(|b| matches!(b, DynamicBlock::NodeObligations { .. }))
                .unwrap()
                < pos(&DynamicBlock::AncestorContext)
        );
    }
}
