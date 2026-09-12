//! Assembles the one-shot message sent to a gate-check agent turn.
//!
//! Mirrors `crate::agent_context`'s shape (static docs from `media/context/`
//! then a dynamic block) but adds the `gate_check:` structured section that
//! `assets/process/agents/state/base.md` documents: forward state, criteria
//! (id/slug/label), and prior evaluations, when any exist for this node.

use crate::media::{MediaPaths, load_static_context};
use anyhow::Result;
use std::path::Path;
use tod_store::outline::{GateCriterion, NodeGateEvaluation};
use uuid::Uuid;

/// Context key under `media/context/` for the gate-check static doc.
pub const GATE_CHECK_CONTEXT_KEY: &str = "gate_check";

/// Everything the gate-check turn needs to describe the node under check.
#[derive(Debug, Clone)]
pub struct GateCheckRequest<'a> {
    pub data_root: &'a Path,
    pub node_id: Uuid,
    pub node_title: String,
    pub node_lifecycle: String,
    /// Node body/details, when the node has any (e.g. design or plan content).
    pub node_body: Option<String>,
    /// Inherited + own purpose, most general first (see `agent_context::ContextRequest`).
    pub purposes: Vec<String>,
    pub from_state: String,
    pub to_state: String,
    /// Criteria for this transition paired with the node's most recent
    /// evaluation of each, if any. Empty when the transition has no seeded
    /// checklist — the gate is still run, governed by prose rules alone.
    pub criteria: Vec<(GateCriterion, Option<NodeGateEvaluation>)>,
}

/// Build the full gate-check message: static layers, then the live node and
/// structured gate-check block.
pub fn build_gate_check_message(paths: &MediaPaths, request: &GateCheckRequest<'_>) -> Result<String> {
    let mut out = load_static_context(paths, GATE_CHECK_CONTEXT_KEY)?;
    out.push_str("\n\n---\n\n");
    out.push_str(&render_dynamic(request));
    Ok(out)
}

fn render_dynamic(request: &GateCheckRequest<'_>) -> String {
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
    out.push_str(&format!("- **Id:** `{}`\n", request.node_id));
    out.push_str(&format!("- **Title:** {}\n", request.node_title.trim()));
    out.push_str(&format!("- **Lifecycle state:** {}\n", request.node_lifecycle));
    out.push_str("- **mode:** interactive\n");
    out.push_str("- **phase_purpose:** gate_check\n");
    match request.node_body.as_deref().map(str::trim) {
        Some(body) if !body.is_empty() => {
            out.push_str("\n**Details:**\n\n");
            out.push_str(body);
            out.push('\n');
        }
        _ => {}
    }

    out.push_str("\n## Gate check\n\n");
    out.push_str(&render_gate_check_yaml(request));
    out.push_str(
        "\nEvaluate your forward gate for this transition now and reply in the \
         format your role doc specifies. Do not write to the database yourself \
         — the app persists `gate_results` and applies the lifecycle change.\n",
    );

    out
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
            purposes: Vec::new(),
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
            purposes: Vec::new(),
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
                }),
            )],
        };
        let text = render_dynamic(&request);
        assert!(text.contains(&id.to_string()));
        assert!(text.contains("design-planning.done-criteria-clear"));
        assert!(text.contains("outcome: pass"));
        assert!(text.contains("looks good"));
    }
}
