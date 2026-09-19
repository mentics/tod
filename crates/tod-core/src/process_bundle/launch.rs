//! Agent prompt assembly from bundled process docs.

use super::manifest::ProcessManifest;
use crate::context_recipes::{self, FLEET_AUTONOMOUS, INTERVIEW_AGENT};
use crate::dynamic::{DynamicContext, NodeSelection, Workspace};
use crate::interview::phase::base_interview_phase;
use crate::media::{MediaPaths, load_static_context};
use anyhow::{Context, Result};
use std::path::Path;
use tod_store::interview::Role;
use uuid::Uuid;

/// A surface's static context fragments, followed by the process-bundle docs
/// that are its role. Kept separate from `context_recipes::build_message`
/// because these surfaces render their dynamic half themselves.
fn with_static_context(
    paths: &MediaPaths,
    recipe: &context_recipes::ContextRecipe,
    docs: String,
) -> Result<String> {
    let mut out = load_static_context(paths, recipe.layers)?;
    out.push_str("\n\n---\n\n");
    out.push_str(docs.trim());
    out.push('\n');
    Ok(out)
}

fn read_doc(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("read bundled agent doc {}", path.display()))
}

/// The byte-stable opening of every interview agent session for `role` in
/// the phase `phase_key`: role doc, phase doc, shared conventions.
pub fn interview_session_prefix(
    manifest: &ProcessManifest,
    media: &MediaPaths,
    role: Role,
    phase_key: &str,
) -> Result<String> {
    let base = base_interview_phase(phase_key);
    let role_doc = match role {
        Role::QuestionMaker => manifest.question_maker_doc(base)?,
        Role::AnswerProcessor => manifest.answer_processor_doc(base)?,
        Role::Drafter => anyhow::bail!("the drafter role is no longer used"),
    };
    with_static_context(
        media,
        &INTERVIEW_AGENT,
        format!(
            "## Role\n\n{}\n\n## Interview phase\n\n{}\n\n## Shared conventions\n\n{}\n",
            read_doc(&role_doc)?.trim(),
            read_doc(&manifest.interview_phase_doc(base)?)?.trim(),
            read_doc(&manifest.base_doc(base)?)?.trim(),
        ),
    )
}

/// The state agent's role doc for `lifecycle`: shared state-agent conventions
/// (`agents/state/base.md`) plus that lifecycle's own doc, when bundled. This
/// is the doc every state-agent turn needs — gate checks, on-entry work, and
/// autonomous fleet runs alike — so every caller shares this one assembly.
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
/// Those conventions describe the app's structured turns (on-entry, gate
/// check) and their YAML response envelope; a conversation that works a
/// state's responsibilities replies in plain text, so it takes this instead.
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
    context_recipes::build_message(
        media,
        &FLEET_AUTONOMOUS,
        Some(&role),
        &DynamicContext {
            data_root: Some(data_root),
            node: Some(&node),
            workspace: Some(&workspace),
            ..Default::default()
        },
        "\n## Instruction\n\n\
         Begin autonomous work for this task in the working directory. \
         Follow the lifecycle state responsibilities above. \
         When you finish this slice of work, summarize what you did and any \
         blockers.\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_bundle::TodInstallPaths;
    use std::path::PathBuf;

    #[test]
    fn interview_prefix_puts_role_before_phase_and_conventions() {
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
        let media = MediaPaths::discover().unwrap();
        let prefix =
            interview_session_prefix(&manifest, &media, Role::AnswerProcessor, "design-interview")
                .unwrap();
        let role = prefix.find("# Answer processor").unwrap();
        let phase = prefix.find("# Phase: design").unwrap();
        let base = prefix.find("## Shared conventions").unwrap();
        assert!(role < phase && phase < base);
    }

    /// The interview agents got no media context at all until the recipe
    /// registry took them on — no domain model, and no `tod-cli` reference
    /// despite their role docs telling them to use it.
    #[test]
    fn interview_prefix_carries_its_media_fragments_before_the_role_docs() {
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
        let media = MediaPaths::discover().unwrap();
        let prefix =
            interview_session_prefix(&manifest, &media, Role::AnswerProcessor, "design-interview")
                .unwrap();
        let stance = prefix
            .find("Your counterpart is **another agent**")
            .unwrap();
        let cli = prefix.find("tod-cli --data-root").unwrap();
        let role = prefix.find("# Answer processor").unwrap();
        assert!(
            stance < cli && cli < role,
            "media fragments must precede the process-bundle role docs"
        );
    }

    /// The verification conversation once carried the shared state-agent
    /// conventions, whose YAML response envelope the agent followed instead
    /// of the surface's short plain reply.
    #[test]
    fn lifecycle_doc_leaves_out_the_structured_response_envelope() {
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
        let full = state_role_doc(&manifest, "verifying").unwrap();
        let only = state_lifecycle_doc(&manifest, "verifying").unwrap();
        assert!(full.contains("## Response format"));
        assert!(!only.contains("## Response format"));
        assert!(only.starts_with("## Lifecycle state: verifying"));
        assert!(full.ends_with(&only));
    }
}
