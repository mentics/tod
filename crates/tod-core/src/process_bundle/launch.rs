//! Agent prompt assembly from bundled process docs.

use super::manifest::ProcessManifest;
use crate::context_recipes::{self, FLEET_AUTONOMOUS};
use crate::dynamic::{DynamicContext, NodeSelection, Workspace};
use crate::media::MediaPaths;
use anyhow::{Context, Result};
use std::path::Path;
use uuid::Uuid;

fn read_doc(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("read bundled agent doc {}", path.display()))
}

/// The state agent's role doc for `lifecycle`: shared state-agent conventions
/// (`agents/state/base.md`) plus that lifecycle's own doc, when bundled. This
/// is the doc a phase agent, its evaluator, and an autonomous fleet run need,
/// so every caller shares this one assembly.
pub fn state_role_doc(manifest: &ProcessManifest, lifecycle: &str) -> Result<String> {
    let base_path = manifest.state_base_doc();
    let base = read_doc(&base_path)
        .with_context(|| format!("read bundled state agent base doc {}", base_path.display()))?;
    Ok(format!(
        "## State agent conventions

{}

{}",
        base.trim(),
        state_lifecycle_doc(manifest, lifecycle)?,
    ))
}

/// Only `lifecycle`'s own doc, without the shared state-agent conventions.
/// Those conventions describe how a phase agent certifies its work; a
/// protocol conversation with its own notion of done (verification, say)
/// takes this instead.
pub fn state_lifecycle_doc(manifest: &ProcessManifest, lifecycle: &str) -> Result<String> {
    let state_body = manifest
        .state_doc(lifecycle)
        .map(|path| read_doc(&path))
        .transpose()?
        .unwrap_or_else(|| {
            format!("(No bundled state agent doc for lifecycle `{lifecycle}` — apply general task work.)")
        });
    Ok(format!("## Lifecycle state: {lifecycle}

{}", state_body.trim()))
}

/// Assemble an ACP prompt for a fleet agent run from bundled state-agent docs.
pub fn build_fleet_agent_prompt(
    manifest: &ProcessManifest,
    media: &MediaPaths,
    data_root: &Path,
    task: &tod_store::fleet::repos::task::FleetTask,
    cwd: &Path,
) -> Result<String> {
    let role = state_role_doc(manifest, &task.lifecycle)?;
    let node = NodeSelection {
        id: Uuid::parse_str(&task.id).unwrap_or_default(),
        title: task.title.clone(),
        body: None,
        lifecycle: Some(task.lifecycle.clone()),
        slug: Some(task.slug.clone()),
    };
    let workspace = Workspace {
        repo: task.repo.clone(),
        branch: task.branch.clone(),
        cwd: cwd.display().to_string(),
        notes: task.notes.iter().map(|n| n.text.clone()).collect(),
    };
    let links: Vec<&str> = task
        .linked_prs
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let pull_requests = crate::pr_context::render_links(&links);
    context_recipes::build_message(
        media,
        &FLEET_AUTONOMOUS,
        Some(&role),
        &DynamicContext {
            data_root: Some(data_root),
            node: Some(&node),
            workspace: Some(&workspace),
            pull_requests: &pull_requests,
            ..Default::default()
        },
        "\n## Instruction\n\n\
         Begin autonomous work for this task in the working directory. \
         Follow the lifecycle state responsibilities above. \
         When you finish this slice of work, summarize what you did and any \
         blockers.\n",
    )
    .map(|prompt| crate::codebase_rules::with_codebase_rules(prompt, cwd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_bundle::TodInstallPaths;
    use std::path::PathBuf;

    /// The verification conversation once carried the shared state-agent
    /// conventions, whose YAML response envelope the agent followed instead
    /// of the surface's short plain reply.
    /// No state doc tells its agent to reply in the old gate-check envelope.
    #[test]
    fn state_docs_carry_no_response_envelope() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("assets")
            .join("process");
        if !root.join("README.md").is_file() {
            return;
        }
        let install = TodInstallPaths::from_process_root(root).unwrap();
        let manifest = ProcessManifest::load(&install).unwrap();
        for state in crate::task::model::LIFECYCLE_STATES {
            let full = state_role_doc(&manifest, state).unwrap();
            let only = state_lifecycle_doc(&manifest, state).unwrap();
            assert!(only.starts_with(&format!("## Lifecycle state: {state}")));
            assert!(full.ends_with(&only));
            for stale in ["gate_results", "forward_lifecycle", "## Response format", "## Forward gate rules"] {
                assert!(!full.contains(stale), "{state}'s doc still says {stale}");
            }
        }
    }
}
