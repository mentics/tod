//! Agent prompt assembly from bundled process docs.

use super::manifest::ProcessManifest;
use crate::interview::phase::base_interview_phase;
use anyhow::{Context, Result};
use std::path::Path;
use tod_store::interview::Role;

fn read_doc(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("read bundled agent doc {}", path.display()))
}

/// The byte-stable opening of every interview agent session for `role` in
/// the phase `phase_key`: role doc, phase doc, shared conventions.
pub fn interview_session_prefix(
    manifest: &ProcessManifest,
    role: Role,
    phase_key: &str,
) -> Result<String> {
    let base = base_interview_phase(phase_key);
    let role_doc = match role {
        Role::QuestionMaker => manifest.question_maker_doc(base)?,
        Role::AnswerProcessor => manifest.answer_processor_doc(base)?,
        Role::Drafter => anyhow::bail!("the drafter's opening comes from drafting_session_prefix"),
    };
    Ok(format!(
        "## Role\n\n{}\n\n## Interview phase\n\n{}\n\n## Shared conventions\n\n{}\n",
        read_doc(&role_doc)?.trim(),
        read_doc(&manifest.interview_phase_doc(base)?)?.trim(),
        read_doc(&manifest.base_doc(base)?)?.trim(),
    ))
}

/// The byte-stable opening of every drafter session in `mode`: the mode's
/// doc, then the drafting conventions shared by capture and drafting.
pub fn drafting_session_prefix(
    manifest: &ProcessManifest,
    mode: crate::drafting::DraftingMode,
) -> Result<String> {
    Ok(format!(
        "## Role\n\n{}\n\n## Drafting conventions\n\n{}\n",
        read_doc(&manifest.drafting_mode_doc(mode == crate::drafting::DraftingMode::Capture))?.trim(),
        read_doc(&manifest.drafting_base_doc())?.trim(),
    ))
}

/// The state agent's role doc for `lifecycle`: shared state-agent conventions
/// (`agents/state/base.md`) plus that lifecycle's own doc, when bundled. This
/// is the doc every state-agent turn needs — gate checks, on-entry work, and
/// autonomous fleet runs alike — so every caller shares this one assembly.
pub fn state_role_doc(manifest: &ProcessManifest, lifecycle: &str) -> Result<String> {
    let base_path = manifest.state_base_doc();
    let base = read_doc(&base_path)
        .with_context(|| format!("read bundled state agent base doc {}", base_path.display()))?;
    let state_body = manifest
        .state_doc(lifecycle)
        .map(|path| read_doc(&path))
        .transpose()?
        .unwrap_or_else(|| {
            format!("(No bundled state agent doc for lifecycle `{lifecycle}` — apply general task work.)")
        });
    Ok(format!(
        "## State agent conventions\n\n{}\n\n## Lifecycle state: {lifecycle}\n\n{}",
        base.trim(),
        state_body.trim(),
    ))
}

/// Assemble an ACP prompt for a fleet agent run from bundled state-agent docs.
pub fn build_fleet_agent_prompt(
    manifest: &ProcessManifest,
    task: &tod_store::fleet::repos::task::FleetTask,
    config_id: &str,
    cwd: &Path,
) -> Result<String> {
    let role = state_role_doc(manifest, &task.lifecycle)?;
    let repo = task.repo.as_deref().unwrap_or("(not set)");
    let branch = task.branch.as_deref().unwrap_or("(default)");
    let notes = if task.notes.is_empty() {
        "(none)".to_string()
    } else {
        task.notes
            .iter()
            .map(|n| format!("- {}", n.text))
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(format!(
        "{role}\n\n\
         ## Task\n\n\
         Config id: {config_id}\n\
         Node id: {node_id}\n\
         Title: {title}\n\
         Slug: {slug}\n\
         Lifecycle: {lifecycle}\n\
         Repository: {repo}\n\
         Branch: {branch}\n\
         Working directory: {cwd}\n\
         \n\
         ## Notes\n\n\
         {notes}\n\n\
         ## Instruction\n\n\
         Begin autonomous work for this task in the working directory. \
         Follow the lifecycle state responsibilities above. \
         When you finish this slice of work, summarize what you did and any blockers.",
        lifecycle = task.lifecycle,
        node_id = task.id,
        title = task.title,
        slug = task.slug,
        cwd = cwd.display(),
    ))
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
        let prefix =
            interview_session_prefix(&manifest, Role::AnswerProcessor, "design-interview").unwrap();
        let role = prefix.find("# Answer processor").unwrap();
        let phase = prefix.find("# Phase: design").unwrap();
        let base = prefix.find("## Shared conventions").unwrap();
        assert!(role < phase && phase < base);
    }
}
