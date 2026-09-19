//! Assembles the first message sent when a user opens an agent chat from
//! somewhere in the app, and the one sent to an implementation session.
//!
//! Both are `media/context/` fragments followed by dynamic blocks; which
//! fragments and which blocks is the surface's recipe (see
//! [`crate::context_recipes`]), and the assembly itself is
//! [`crate::context_recipes::build_message`]. This module's job is turning the
//! caller's request into a [`DynamicContext`].

use crate::context_recipes::{
    FIX_SESSION, IMPLEMENT_SESSION, REVIEW_SESSION, VERIFY_SESSION, VISUAL_DESIGN_CHAT,
};
use crate::dynamic::DynamicContext;
use crate::gate::PlanStepWithLinks;
use crate::media::MediaPaths;
use anyhow::Result;
use std::path::Path;
use tod_store::outline::NodeObligation;
use tod_store::review::ReviewFinding;

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
    /// Rendered ancestor context — build with
    /// `tod_core::node_context::render_inherited_context`: what the node
    /// inherits, each Spec ancestor's summary and constraints.
    pub ancestor_context: String,
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
            ancestor_context: &request.ancestor_context,
            obligation: request.obligation.as_ref(),
            ..Default::default()
        },
        "",
    )
}

/// Surface key for an implementation session, used for session naming (see
/// `session_name::session_name_for`) — not a media context key.
pub const IMPLEMENT_SURFACE_KEY: &str = "active/implement";

/// Everything an implementation session's first-turn context describes.
#[derive(Debug, Clone)]
pub struct ImplementRequest<'a> {
    pub data_root: &'a Path,
    /// Where the session runs: the node's worktree, when one is set up.
    pub working_dir: &'a Path,
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
    build_plan_session_message(paths, &IMPLEMENT_SESSION, None, request)
}

/// Build the full verification-session first message: what an
/// implementation session is given, plus the `verifying` state agent's role
/// doc, which says how verification is done.
pub fn build_verify_message(
    paths: &MediaPaths,
    request: &ImplementRequest<'_>,
    role_doc: &str,
) -> Result<String> {
    build_plan_session_message(paths, &VERIFY_SESSION, Some(role_doc), request)
}

/// Build the full code-review-session first message: the same node, plan,
/// and obligations, plus the `review` state agent's role doc.
pub fn build_review_message(
    paths: &MediaPaths,
    request: &ImplementRequest<'_>,
    role_doc: &str,
) -> Result<String> {
    build_plan_session_message(paths, &REVIEW_SESSION, Some(role_doc), request)
}

/// Build the full fix-session first message: what an implementation session
/// is given, plus the node's open review findings to resolve.
pub fn build_fix_message(
    paths: &MediaPaths,
    request: &ImplementRequest<'_>,
    findings: &[ReviewFinding],
) -> Result<String> {
    build_plan_session_message_with(paths, &FIX_SESSION, None, request, findings)
}

fn build_plan_session_message(
    paths: &MediaPaths,
    recipe: &ContextRecipe,
    role_doc: Option<&str>,
    request: &ImplementRequest<'_>,
) -> Result<String> {
    build_plan_session_message_with(paths, recipe, role_doc, request, &[])
}

fn build_plan_session_message_with(
    paths: &MediaPaths,
    recipe: &ContextRecipe,
    role_doc: Option<&str>,
    request: &ImplementRequest<'_>,
    findings: &[ReviewFinding],
) -> Result<String> {
    crate::context_recipes::build_message(
        paths,
        recipe,
        role_doc,
        &DynamicContext {
            data_root: Some(request.data_root),
            working_dir: Some(request.working_dir),
            node: Some(&request.node),
            obligations: &request.obligations,
            ancestor_context: &request.ancestor_context,
            plan_steps: &request.plan_steps,
            findings,
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
            DynamicBlock::Node,
            DynamicBlock::AncestorContext,
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
                ancestor_context: &request.ancestor_context,
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
            ancestor_context: String::new(),
        };
        let text = dynamic_for(&req);
        assert!(text.contains("data") && text.contains("tod"));
        assert!(text.contains(&Uuid::nil().to_string()));
        assert!(text.contains("Obligations Panel"));
        assert!(text.contains("Panel for editing direct obligations."));
        assert!(text.contains("No individual obligation is selected"));
    }

    #[test]
    fn a_chat_carries_what_the_node_inherits() {
        let req = ContextRequest {
            recipe: NODE_CHAT,
            data_root: Path::new("/data/tod"),
            node: node(),
            obligation: None,
            ancestor_context: "\n## Inherited context (ancestors)\n\n### From \"Product\"\nShips it.\n"
                .into(),
        };
        let text = dynamic_for(&req);
        assert!(
            text.find("## Selected node").unwrap() < text.find("Ships it.").unwrap(),
            "{text}"
        );
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
            ancestor_context: String::new(),
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
            ancestor_context: String::new(),
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

    /// An implementation session is told where its code is, ahead of the data
    /// root, which may sit inside another checkout of the same repository.
    #[test]
    fn implement_names_its_working_directory_before_the_data_root() {
        let blocks = IMPLEMENT_SESSION.blocks;
        let pos = |b: &DynamicBlock| blocks.iter().position(|x| x == b).unwrap();
        assert!(pos(&DynamicBlock::WorkingDirectory) < pos(&DynamicBlock::DataRoot));

        let worktree = Path::new("/worktrees/node/tod");
        let text = render(
            blocks,
            &DynamicContext {
                data_root: Some(Path::new("/repo/.local/data")),
                working_dir: Some(worktree),
                ..Default::default()
            },
        );
        assert!(text.contains(&format!("**Working directory:** `{}`", worktree.display())));
    }
}
