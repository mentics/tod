//! Assembles the one-shot message sent to a gate-check agent turn.
//!
//! Mirrors `crate::agent_context`'s shape (static docs from `media/context/`
//! then a dynamic block) but adds the `gate_check:` structured section that
//! `assets/process/agents/state/base.md` documents: forward state, criteria
//! (id/slug/label), and prior evaluations, when any exist for this node.

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

/// The gate-check surface's recipe (static fragments + dynamic blocks).
pub const GATE_CHECK_RECIPE: &ContextRecipe = &context_recipes::GATE_CHECK;

/// The on-entry surface's recipe.
pub const ON_ENTRY_RECIPE: &ContextRecipe = &context_recipes::ON_ENTRY;

/// Everything the gate-check turn needs to describe the node under check.
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
    pub from_state: String,
    pub to_state: String,
    /// Criteria for this transition paired with the node's most recent
    /// evaluation of each, if any. Empty when the transition has no seeded
    /// checklist — the gate is still run, governed by prose rules alone.
    pub criteria: Vec<(GateCriterion, Option<NodeGateEvaluation>)>,
}

/// Build the full gate-check message: static layers, the state agent's own
/// role doc (forward-gate prose rules, response format, the "no Q&A in this
/// session" rule), then the live node and structured gate-check block.
///
/// `role_doc` is `process_bundle::state_role_doc(manifest, from_state)` —
/// callers assemble it (it needs the bundled process root) and pass it in so
/// this module stays free of `TodInstallPaths`/`ProcessManifest` concerns.
/// Without it the agent has only the generic `gate_check.md` instructions and
/// none of the transition-specific rules it needs, which is what pushes it
/// into extra exploratory tool calls instead of a quick single-turn verdict.
pub fn build_gate_check_message(
    paths: &MediaPaths,
    request: &GateCheckRequest<'_>,
    role_doc: &str,
) -> Result<String> {
    let node = node_selection(request);
    context_recipes::build_message(
        paths,
        GATE_CHECK_RECIPE,
        Some(role_doc),
        &dynamic_context(request, "gate_check", &node),
        &gate_check_tail(request),
    )
}

/// Build the on-entry message: static layers, the state agent's own role doc,
/// then the live node context with no gate-check block — this turn is about
/// doing the state's own "On entry" work (e.g. drafting plan steps), not
/// evaluating a forward gate.
pub fn build_on_entry_message(
    paths: &MediaPaths,
    request: &GateCheckRequest<'_>,
    role_doc: &str,
) -> Result<String> {
    let node = node_selection(request);
    context_recipes::build_message(
        paths,
        ON_ENTRY_RECIPE,
        Some(role_doc),
        &dynamic_context(request, "on_entry", &node),
        "\nYou have just entered this lifecycle state. Perform this state's \
         **\"On entry\"** responsibilities from your role doc now, directly \
         via `tod-cli` (e.g. drafting or refining plan steps) — do not wait \
         for a gate check or an interview turn to do this piecemeal. This \
         turn may run again later as obligations or plan steps change; check \
         what already exists first and add only what's missing, don't \
         duplicate or discard existing work.\n\n\
         This is **not** a gate-check turn: do not evaluate the forward \
         gate, and do not return `result` or `gate_results` — this reply is \
         not parsed as structured data. Reply with a short plain-text \
         summary of what you did, or that nothing was needed.\n",
    )
}

/// The gate-check-only tail, appended after the shared dynamic blocks: the
/// structured `gate_check:` section `assets/process/agents/state/base.md`
/// documents, plus the response-format rules for it.
fn gate_check_tail(request: &GateCheckRequest<'_>) -> String {
    let mut out = String::from("\n## Gate check\n\n");
    out.push_str(&render_gate_check_yaml(request));
    out.push_str(
        "\nEvaluate your forward gate for this transition now. This is a \
         **structured protocol response, not a chat reply**: the app parses \
         your entire message as one YAML document and renders `gate_results` \
         as a table with a button per row — it is not read by a human as \
         prose. Your response must be **the YAML document and nothing else**: \
         no narration before it (\"Let me...\", \"I'll now...\"), no \
         explanation after it, no markdown code fence around it, no restating \
         your reasoning outside the `findings`/`detail` fields where it \
         belongs. Do not write to the database yourself — the app persists \
         `gate_results` and applies the lifecycle change.\n\n\
         Two fields look similar but are **not the same vocabulary** — do \
         not swap them: the top-level `result` is exactly one of \
         `pass | blocked | needs_human | no_change` (never `fail`); each \
         row's `gate_results[].outcome` is exactly one of \
         `pass | fail | waived`. A blocked transition is \
         `result: blocked` with the failing row(s) marked `outcome: fail`.\n\n\
         Set each failing row's `action` deliberately — the app renders a \
         button directly off it, so getting it wrong means the user sees the \
         wrong (or no) button. Use `action: interview` whenever answering \
         that phase's interview — including any open/unanswered interview \
         questions — would satisfy the criterion; this is the common case, \
         don't default to `none` here. Use `action: none` only when the app \
         genuinely has no in-app tool for it (e.g. no feature yet for \
         recording API/data-structure specs or tracking spikes), and when \
         you do, say so plainly in that row's `detail` (e.g. \"no in-app tool \
         for this yet; resolve outside the app or waive\") — never leave \
         `detail` empty on a failing row.\n",
    );

    out
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
        ..Default::default()
    }
}

fn render_gate_check_yaml(request: &GateCheckRequest<'_>) -> String {
    let mut out = String::from("```yaml\ngate_check:\n");
    out.push_str(&format!("  forward_state: {}\n", request.to_state));
    if request.criteria.is_empty() {
        out.push_str("  criteria: []\n");
    } else {
        out.push_str("  criteria:\n");
        for (criterion, _) in &request.criteria {
            out.push_str(&format!("    - id: {}\n", criterion.id));
            out.push_str(&format!("      slug: {}\n", criterion.slug));
            out.push_str(&format!("      label: {:?}\n", criterion.label));
        }
    }
    let prior: Vec<_> = request
        .criteria
        .iter()
        .filter_map(|(criterion, eval)| eval.as_ref().map(|e| (criterion, e)))
        .collect();
    if prior.is_empty() {
        out.push_str("  prior_evaluations: []\n");
    } else {
        out.push_str("  prior_evaluations:\n");
        for (criterion, eval) in prior {
            out.push_str(&format!("    - criterion_id: {}\n", criterion.id));
            out.push_str(&format!("      outcome: {}\n", eval.outcome));
            if let Some(detail) = eval.detail.as_deref() {
                out.push_str(&format!("      detail: {detail:?}\n"));
            }
        }
    }
    out.push_str("```\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::outline::{OUTCOME_PASS, SOURCE_AGENT};

    /// The dynamic half of a gate-check message: the shared blocks plus the
    /// gate-check tail, without needing a media bundle on disk.
    fn render_dynamic(request: &GateCheckRequest<'_>) -> String {
        let node = node_selection(request);
        let mut out = crate::dynamic::render(
            GATE_CHECK_RECIPE.blocks,
            &dynamic_context(request, "gate_check", &node),
        );
        out.push_str(&gate_check_tail(request));
        out
    }

    /// The dynamic half of an on-entry message — same blocks, no tail.
    fn render_node_context(request: &GateCheckRequest<'_>, phase_purpose: &str) -> String {
        let node = node_selection(request);
        let ctx = dynamic_context(request, phase_purpose, &node);
        crate::dynamic::render(ON_ENTRY_RECIPE.blocks, &ctx)
    }

    fn criterion(id: Uuid) -> GateCriterion {
        GateCriterion {
            id,
            from_state: "design".into(),
            to_state: "planning".into(),
            slug: "design-planning.done-criteria-clear".into(),
            label: "Do I know what done looks like?".into(),
            sort_order: 1,
            active: true,
        }
    }

    #[test]
    fn renders_forward_state_and_empty_criteria() {
        let request = GateCheckRequest {
            data_root: Path::new("/data/tod"),
            node_id: Uuid::nil(),
            node_title: "Ship it".into(),
            node_lifecycle: "verifying".into(),
            node_body: None,
            obligations: Vec::new(),
            ancestor_context: String::new(),
            plan_steps: Vec::new(),
            from_state: "verifying".into(),
            to_state: "review".into(),
            criteria: Vec::new(),
        };
        let text = render_dynamic(&request);
        assert!(text.contains("forward_state: review"));
        assert!(text.contains("criteria: []"));
        assert!(text.contains("phase_purpose:** gate_check"));
    }

    #[test]
    fn renders_criteria_and_prior_evaluations() {
        let id = Uuid::from_u128(42);
        let request = GateCheckRequest {
            data_root: Path::new("/data/tod"),
            node_id: Uuid::nil(),
            node_title: "Ship it".into(),
            node_lifecycle: "design".into(),
            node_body: None,
            obligations: Vec::new(),
            ancestor_context: String::new(),
            plan_steps: Vec::new(),
            from_state: "design".into(),
            to_state: "planning".into(),
            criteria: vec![(
                criterion(id),
                Some(NodeGateEvaluation {
                    node_id: Uuid::nil(),
                    criterion_id: id,
                    outcome: OUTCOME_PASS.to_string(),
                    detail: Some("looks good".into()),
                    source: SOURCE_AGENT.to_string(),
                    evaluated_at: 0,
                    action: tod_store::outline::repos::gate::ACTION_NONE.to_string(),
                }),
            )],
        };
        let text = render_dynamic(&request);
        assert!(text.contains(&id.to_string()));
        assert!(text.contains("design-planning.done-criteria-clear"));
        assert!(text.contains("outcome: pass"));
        assert!(text.contains("looks good"));
    }

    #[test]
    fn on_entry_message_has_no_gate_block_and_right_phase_purpose() {
        let request = GateCheckRequest {
            data_root: Path::new("/data/tod"),
            node_id: Uuid::nil(),
            node_title: "Ship it".into(),
            node_lifecycle: "planning".into(),
            node_body: None,
            obligations: Vec::new(),
            ancestor_context: String::new(),
            plan_steps: Vec::new(),
            from_state: "planning".into(),
            to_state: "planning".into(),
            criteria: Vec::new(),
        };
        let text = render_node_context(&request, "on_entry");
        assert!(text.contains("phase_purpose:** on_entry"));
        assert!(!text.contains("## Gate check"));
        assert!(!text.contains("gate_check:"));
    }

    #[test]
    fn gate_check_message_includes_the_state_role_doc() {
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
            from_state: "design".into(),
            to_state: "planning".into(),
            criteria: Vec::new(),
        };
        let message = build_gate_check_message(
            &paths,
            &request,
            "## State agent conventions\n\nDo not conduct sequential Q&A in this session.",
        )
        .unwrap();
        assert!(
            message.contains("Do not conduct sequential Q&A in this session."),
            "gate-check message must carry the state role doc so the agent has its \
             forward-gate rules and the no-Q&A rule without extra exploration"
        );
    }

    #[test]
    fn on_entry_message_includes_the_state_role_doc() {
        let paths = MediaPaths::discover().unwrap();
        let request = GateCheckRequest {
            data_root: Path::new("/data/tod"),
            node_id: Uuid::nil(),
            node_title: "Ship it".into(),
            node_lifecycle: "planning".into(),
            node_body: None,
            obligations: Vec::new(),
            ancestor_context: String::new(),
            plan_steps: Vec::new(),
            from_state: "planning".into(),
            to_state: "planning".into(),
            criteria: Vec::new(),
        };
        let message = build_on_entry_message(
            &paths,
            &request,
            "## State agent conventions\n\nOn-entry marker text.",
        )
        .unwrap();
        assert!(message.contains("On-entry marker text."));
    }
}
