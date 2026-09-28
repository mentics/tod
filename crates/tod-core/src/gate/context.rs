//! Assembles a lifecycle state agent's message: a phase agent's or an
//! evaluator's (`crate::conversation::phase`). Static docs from
//! `media/context/`, the state's role doc, the node's context, then the phase
//! status. The request type keeps its name from when these were gate checks.

use crate::context_recipes::{self, ContextRecipe};
use crate::dynamic::{DynamicContext, NodeSelection};
use crate::media::MediaPaths;
use anyhow::Result;
use std::path::Path;
use tod_store::outline::{GateCriterion, NodeGateEvaluation, NodeObligation, PlanStep};
use uuid::Uuid;

/// One plan step paired with its dependency and satisfied-obligation ids,
/// for rendering traceability (`--satisfies` / `--depends-on` links) in the
/// gate-check message — mirrors what the planning-interview snapshot shows.
#[derive(Debug, Clone)]
pub struct PlanStepWithLinks {
    pub step: PlanStep,
    pub depends_on: Vec<Uuid>,
    pub satisfies: Vec<Uuid>,
}

/// Everything a state agent is told about its node.
#[derive(Debug, Clone)]
pub struct GateCheckRequest<'a> {
    pub data_root: &'a Path,
    pub node_id: Uuid,
    pub node_title: String,
    pub node_lifecycle: String,
    /// Node body/details, when the node has any (the `details` extra-content
    /// field — design decisions live as design-phase obligations, not here).
    pub node_body: Option<String>,
    /// This node's own obligations (requirements/constraints) — not
    /// ancestors' — so a `verifying`/`review` gate check can confirm every
    /// requirement was traced without shelling out to `tod-cli obligations
    /// list` first, and so criteria about "does this node have obligations"
    /// aren't confused by ancestor obligations mixed into the same list.
    pub obligations: Vec<NodeObligation>,
    /// Rendered ancestor context — see
    /// `tod_core::node_context::render_inherited_context`: each
    /// ancestor's title, generated summary, and constraints, not its full
    /// requirements. Callers build this from a live connection since
    /// `GateCheckRequest` itself carries no DB handle.
    pub ancestor_context: String,
    /// This node's plan steps with their dependency and `--satisfies` links,
    /// for the same traceability reason.
    pub plan_steps: Vec<PlanStepWithLinks>,
    /// Rendered work history — see
    /// `tod_core::node_context::render_work_history`. Only the `learn`
    /// retrospective is given it; empty for every other state.
    pub work_history: String,
    pub from_state: String,
    pub to_state: String,
    /// Criteria for this transition paired with the node's most recent
    /// evaluation of each, if any. Empty when the transition has no seeded
    /// checklist — the gate is still run, governed by prose rules alone.
    pub criteria: Vec<(GateCriterion, Option<NodeGateEvaluation>)>,
}

/// Build a lifecycle phase agent's or evaluator's message
/// (`crate::context_recipes::PHASE` / `EVALUATE`): the recipe's static
/// layers, the state's role doc, the node's context, then `tail` (the phase
/// status, `crate::phase::render_status`).
pub fn build_phase_message(
    paths: &MediaPaths,
    recipe: &ContextRecipe,
    request: &GateCheckRequest<'_>,
    role_doc: &str,
    phase_purpose: &str,
    tail: &str,
) -> Result<String> {
    let node = node_selection(request);
    context_recipes::build_message(
        paths,
        recipe,
        Some(role_doc),
        &dynamic_context(request, phase_purpose, &node),
        tail,
    )
}

/// `GateCheckRequest` carries the node's fields flat; the dynamic blocks want
/// them as a `NodeSelection`.
fn node_selection(request: &GateCheckRequest<'_>) -> NodeSelection {
    NodeSelection {
        id: request.node_id,
        title: request.node_title.clone(),
        body: request.node_body.clone(),
        lifecycle: Some(request.node_lifecycle.clone()),
        slug: None,
    }
}

/// Map the request onto the blocks both state-agent surfaces render.
fn dynamic_context<'a>(
    request: &'a GateCheckRequest<'a>,
    phase_purpose: &'a str,
    node: &'a NodeSelection,
) -> DynamicContext<'a> {
    DynamicContext {
        data_root: Some(request.data_root),
        node: Some(node),
        process_fields: Some(("interactive", phase_purpose)),
        obligations: &request.obligations,
        ancestor_context: &request.ancestor_context,
        plan_steps: &request.plan_steps,
        work_history: &request.work_history,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_message_carries_the_role_doc_the_node_and_the_tail() {
        let paths = MediaPaths::discover().unwrap();
        let request = GateCheckRequest {
            data_root: Path::new("/data/tod"),
            node_id: Uuid::nil(),
            node_title: "Ship it".into(),
            node_lifecycle: "design".into(),
            node_body: None,
            obligations: Vec::new(),
            ancestor_context: String::new(),
            plan_steps: Vec::new(),
            work_history: String::new(),
            from_state: "design".into(),
            to_state: "planning".into(),
            criteria: Vec::new(),
        };
        let message = build_phase_message(
            &paths,
            &context_recipes::PHASE,
            &request,
            "Role doc marker.",
            "phase",
            "\nTail marker.\n",
        )
        .unwrap();
        assert!(message.contains("Role doc marker."), "{message}");
        assert!(message.contains("phase_purpose:** phase"), "{message}");
        assert!(message.contains("Ship it"), "{message}");
        assert!(message.contains("Tail marker."), "{message}");
    }
}
