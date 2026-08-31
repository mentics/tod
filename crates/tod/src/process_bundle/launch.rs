//! Agent launch context and prompt assembly.

use super::install::TodInstallPaths;
use super::manifest::ProcessManifest;
use tod_store::fleet::repos::agent_config::AgentConfigRepo;
use tod_store::fleet::workspace_cwd_for_agent;
use crate::interview::agent::DeepDiveContext;
use crate::interview::config::{base_interview_phase, path_for_storage};
use crate::interview::db::InterviewSession;
use tod_store::outline::repos::NodeRepo;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Interview agent prompt split for session reuse.
///
/// `session_prefix` (inline instructions + scope + session paths) is sent only on the
/// first prompt of a reused ACP session. `turn` is sent every time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterviewAgentPrompt {
    pub session_prefix: String,
    pub turn: String,
}

impl InterviewAgentPrompt {
    /// Full prompt for a brand-new ACP session.
    pub fn full(&self) -> String {
        self.for_slot(0)
    }

    /// Prompt for a pooled slot that has already handled `responses_received` answers.
    pub fn for_slot(&self, responses_received: u32) -> String {
        if responses_received == 0 && !self.session_prefix.is_empty() {
            format!("{}\n\n{}", self.session_prefix.trim_end(), self.turn)
        } else {
            self.turn.clone()
        }
    }
}

/// Everything needed to spawn an ACP agent run for an interview role.
#[derive(Debug, Clone)]
pub struct AgentLaunchContext {
    pub cwd: PathBuf,
    pub node_id: Uuid,
    pub phase: String,
    pub role_doc: PathBuf,
    pub phase_doc: PathBuf,
    pub base_doc: PathBuf,
    pub scope_paths: Vec<PathBuf>,
    pub session_config: PathBuf,
    pub scratchpad: PathBuf,
    pub prompt: InterviewAgentPrompt,
}

impl AgentLaunchContext {
    /// Build launch context for question maker bootstrap.
    pub fn question_maker_bootstrap(
        conn: &Connection,
        _install: &TodInstallPaths,
        manifest: &ProcessManifest,
        paths: &crate::interview::TodPaths,
        session: &InterviewSession,
        scratchpad: &Path,
    ) -> Result<Self> {
        let node_id = session.node_id;
        let phase = session.phase.clone();
        let base = base_interview_phase(&phase).to_string();
        let export = super::export::resolve_and_export_scope(conn, node_id, scratchpad)?;
        let cwd =
            workspace_cwd_for_interview(conn, session.agent_config_id.as_deref(), node_id, paths)?;
        let role_doc = manifest.question_maker_doc(&base)?;
        let phase_doc = manifest.interview_phase_doc(&base)?;
        let base_doc = manifest.base_doc(&base)?;
        let session_config = scratchpad.join("interview-config.md");
        let scope_paths = export.paths.clone();
        let session_prefix = build_session_prefix(
            &role_doc,
            &phase_doc,
            &base_doc,
            &scope_paths,
            scratchpad,
            &session_config,
        )?;
        let turn = build_bootstrap_turn(session, &cwd, scratchpad, &session_config);
        Ok(Self {
            cwd,
            node_id,
            phase,
            role_doc,
            phase_doc,
            base_doc,
            scope_paths,
            session_config,
            scratchpad: scratchpad.to_path_buf(),
            prompt: InterviewAgentPrompt {
                session_prefix,
                turn,
            },
        })
    }

    /// Build launch context for question maker replenish or action processing.
    pub fn question_maker_followup(
        conn: &Connection,
        _install: &TodInstallPaths,
        manifest: &ProcessManifest,
        paths: &crate::interview::TodPaths,
        node_id: Uuid,
        phase: &str,
        scratchpad: &Path,
        config_path: &Path,
        instruction: &str,
        agent_config_id: Option<&str>,
    ) -> Result<Self> {
        let base = base_interview_phase(phase).to_string();
        let export = super::export::resolve_and_export_scope(conn, node_id, scratchpad)?;
        let cwd = workspace_cwd_for_interview(conn, agent_config_id, node_id, paths)?;
        let role_doc = manifest.question_maker_doc(&base)?;
        let phase_doc = manifest.interview_phase_doc(&base)?;
        let base_doc = manifest.base_doc(&base)?;
        let scope_paths = export.paths.clone();
        let session_prefix = build_session_prefix(
            &role_doc,
            &phase_doc,
            &base_doc,
            &scope_paths,
            scratchpad,
            config_path,
        )?;
        let turn = build_turn_instruction(scratchpad, config_path, instruction);
        Ok(Self {
            cwd,
            node_id,
            phase: phase.to_string(),
            role_doc,
            phase_doc,
            base_doc,
            scope_paths,
            session_config: config_path.to_path_buf(),
            scratchpad: scratchpad.to_path_buf(),
            prompt: InterviewAgentPrompt {
                session_prefix,
                turn,
            },
        })
    }

    /// Build launch context for answer-processor runs.
    pub fn answer_processor(
        conn: &Connection,
        _install: &TodInstallPaths,
        manifest: &ProcessManifest,
        paths: &crate::interview::TodPaths,
        node_id: Uuid,
        phase: &str,
        scratchpad: &Path,
        config_path: &Path,
        instruction: &str,
        agent_config_id: Option<&str>,
    ) -> Result<Self> {
        let base = base_interview_phase(phase).to_string();
        let _export = super::export::resolve_and_export_scope(conn, node_id, scratchpad)?;
        let cwd = workspace_cwd_for_interview(conn, agent_config_id, node_id, paths)?;
        let role_doc = manifest.answer_processor_doc(&base)?;
        let phase_doc = manifest.interview_phase_doc(&base)?;
        let base_doc = manifest.base_doc(&base)?;
        let scope_paths = scope_paths_from_scratchpad(scratchpad);
        let session_prefix = build_session_prefix(
            &role_doc,
            &phase_doc,
            &base_doc,
            &scope_paths,
            scratchpad,
            config_path,
        )?;
        let turn = build_turn_instruction(scratchpad, config_path, instruction);
        Ok(Self {
            cwd,
            node_id,
            phase: phase.to_string(),
            role_doc,
            phase_doc,
            base_doc,
            scope_paths,
            session_config: config_path.to_path_buf(),
            scratchpad: scratchpad.to_path_buf(),
            prompt: InterviewAgentPrompt {
                session_prefix,
                turn,
            },
        })
    }
}

/// Resolve ACP working directory for an interview session (agent worktree when linked).
pub fn workspace_cwd_for_interview(
    conn: &Connection,
    agent_config_id: Option<&str>,
    node_id: Uuid,
    paths: &crate::interview::TodPaths,
) -> Result<PathBuf> {
    if let Some(id) = agent_config_id {
        if let Some(agent) = AgentConfigRepo::new(conn).get(id)? {
            if let Ok(cwd) = workspace_cwd_for_agent(&agent) {
                return Ok(cwd);
            }
        }
    }
    workspace_cwd_for_node(conn, node_id, paths)
}

/// Resolve ACP working directory for a node (repo field or data root fallback).
pub fn workspace_cwd_for_node(
    conn: &Connection,
    node_id: Uuid,
    paths: &crate::interview::TodPaths,
) -> Result<PathBuf> {
    let repo = NodeRepo::new(conn).get_repo(node_id)?;
    if let Some(r) = repo.filter(|s| !s.is_empty()) {
        let p = PathBuf::from(r);
        return Ok(p.canonicalize().unwrap_or(p));
    }
    Ok(paths
        .repo_root()
        .canonicalize()
        .unwrap_or_else(|_| paths.repo_root().to_path_buf()))
}

fn scope_paths_from_scratchpad(scratchpad: &Path) -> Vec<PathBuf> {
    let scope = scratchpad.join("scope");
    ["obligations.md", "context.md"]
        .into_iter()
        .map(|name| scope.join(name))
        .filter(|p| p.is_file())
        .collect()
}

fn read_doc(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("read bundled agent doc {}", path.display()))
}

fn format_inline_scope(scope_paths: &[PathBuf]) -> Result<String> {
    if scope_paths.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::from("## Scope export\n\n");
    for path in scope_paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("scope.md");
        let body = std::fs::read_to_string(path)
            .with_context(|| format!("read scope export {}", path.display()))?;
        out.push_str(&format!("### {name}\n\n{body}\n\n"));
    }
    Ok(out)
}

fn build_session_prefix(
    role_doc: &Path,
    phase_doc: &Path,
    base_doc: &Path,
    scope_paths: &[PathBuf],
    scratchpad: &Path,
    config_path: &Path,
) -> Result<String> {
    let role = read_doc(role_doc)?;
    let phase = read_doc(phase_doc)?;
    let base = read_doc(base_doc)?;
    let scope = format_inline_scope(scope_paths)?;
    let queue = scratchpad.join("queue");
    Ok(format!(
        "## Role\n\n\
         {role}\n\n\
         ## Interview phase\n\n\
         {phase}\n\n\
         ## Shared conventions\n\n\
         {base}\n\n\
         {scope}\
         ## Session paths\n\n\
         Config: {}\n\
         Session scratchpad: {}\n\
         Queue directory: {}\n",
        config_path.display(),
        scratchpad.display(),
        queue.display(),
    ))
}

fn build_bootstrap_turn(
    session: &InterviewSession,
    cwd: &Path,
    scratchpad: &Path,
    config_path: &Path,
) -> String {
    format!(
        "Interview UI kickoff — bootstrap this interview session.\n\
         \n\
         Session id: {}\n\
         Display name: {}\n\
         Node id: {}\n\
         Phase: {}\n\
         Working directory (cwd): {}\n\
         \n\
         Session scratchpad: {}\n\
         Create interview-config at: {}\n\
         \n\
         Follow the bootstrap section in your role instructions:\n\
         1. Create session scratchpad structure under the path above.\n\
         2. Derive session_id from transcript metadata when created.\n\
         3. Create empty queue/ and interview-config.md with absolute paths (no \\\\?\\ prefixes).\n\
         4. Populate queue/ with initial questions for this phase (at least several open questions before you finish).\n\
         5. Update question-maker-status.md only after queue files exist.\n\
         \n\
         Queue files use YAML front matter only for UI fields (context, question, recommend, proposed_text, options with digit keys \"1\"/\"2\"/\"3\", layer, kind, covers); leave the markdown body empty.\n\
         Do not talk to the user. Do not return until queue/ has question files. Return the queue directory path only when done.",
        session.id,
        session.display_name,
        session.node_id,
        session.phase,
        cwd.display(),
        scratchpad.display(),
        config_path.display(),
    )
}

fn build_turn_instruction(scratchpad: &Path, config_path: &Path, instruction: &str) -> String {
    let queue = scratchpad.join("queue");
    format!(
        "Config: {}\n\
         Session scratchpad: {}\n\
         Queue directory: {}\n\
         \n\
         {instruction}",
        config_path.display(),
        scratchpad.display(),
        queue.display(),
    )
}

pub fn node_scratchpad_root(data_root: &Path, node_id: Uuid) -> PathBuf {
    data_root
        .join("agent")
        .join("nodes")
        .join(node_id.to_string())
        .join("scratchpad")
        .join("interviews")
}

pub fn session_scratchpad(data_root: &Path, node_id: Uuid, session_key: &str) -> PathBuf {
    node_scratchpad_root(data_root, node_id).join(session_key)
}

/// Write a starter interview-config.md for a new session.
pub fn write_interview_config(
    path: &Path,
    node_id: Uuid,
    phase: &str,
    session_id: &str,
    scratchpad: &Path,
    role_doc: &Path,
    scope_paths: &[PathBuf],
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let queue = scratchpad.join("queue");
    let mut scope_lines = String::new();
    for p in scope_paths {
        scope_lines.push_str(&format!("  - {}\n", path_for_storage(p)));
    }
    let body = format!(
        "# Interview config\n\
         \n\
         session_id: {session_id}\n\
         node_id: {node_id}\n\
         phase: {phase}\n\
         scratchpad: {}\n\
         queue: {}\n\
         role_doc: {}\n\
         scope:\n{scope_lines}\
         queue_target: 8\n",
        path_for_storage(scratchpad),
        path_for_storage(&queue),
        path_for_storage(role_doc),
    );
    std::fs::write(path, body)
        .with_context(|| format!("write interview config {}", path.display()))?;
    Ok(())
}

/// Read the bundled deep-dive role doc.
pub fn load_deep_dive_role_doc(install: &TodInstallPaths) -> Result<String> {
    let manifest = ProcessManifest::load(install)?;
    read_doc(&manifest.deep_dive_doc())
}

/// Assemble a deep-dive ACP prompt with inlined role doc and question context.
pub fn build_deep_dive_prompt(
    role_doc: &str,
    context: &DeepDiveContext,
    conversation: Option<&str>,
) -> String {
    let mut prompt = format!(
        "## Role\n\n\
         {role_doc}\n\n\
         ## Question context\n\n\
         Project: {}\n\
         Task: {}\n\
         Lifecycle state: {}\n\
         Interview purpose: {}\n\
         Interview phase: {}\n\
         Question id: {}\n\
         Question:\n{}\n",
        context.project,
        context.task,
        context.lifecycle_state,
        context.interview_purpose,
        context.interview_phase,
        context.question_id,
        context.question_body,
    );
    if let Some(conversation) = conversation.filter(|s| !s.trim().is_empty()) {
        prompt.push_str("\n## Conversation\n\n");
        prompt.push_str(conversation);
        prompt.push('\n');
    }
    prompt
}

/// Assemble an ACP prompt for a fleet agent run from bundled state-agent docs.
pub fn build_fleet_agent_prompt(
    manifest: &ProcessManifest,
    task: &tod_store::fleet::repos::task::FleetTask,
    config_id: &str,
    cwd: &Path,
) -> Result<String> {
    let base_path = manifest.state_base_doc();
    let base = read_doc(&base_path).with_context(|| {
        format!(
            "read bundled state agent base doc {}",
            base_path.display()
        )
    })?;
    let state_body = manifest
        .state_doc(&task.lifecycle)
        .map(|path| read_doc(&path))
        .transpose()?
        .unwrap_or_else(|| {
            format!(
                "(No bundled state agent doc for lifecycle `{}` — apply general task work.)",
                task.lifecycle
            )
        });
    let repo = task.repo.as_deref().unwrap_or("(not set)");
    let branch = task.branch.as_deref().unwrap_or("(default)");
    let notes = task.notes.as_deref().unwrap_or("");
    Ok(format!(
        "## State agent conventions\n\n\
         {base}\n\n\
         ## Lifecycle state: {lifecycle}\n\n\
         {state_body}\n\n\
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

    #[test]
    fn prompt_for_slot_omits_prefix_after_first_response() {
        let prompt = InterviewAgentPrompt {
            session_prefix: "PREFIX".into(),
            turn: "TURN".into(),
        };
        assert_eq!(prompt.for_slot(0), "PREFIX\n\nTURN");
        assert_eq!(prompt.for_slot(1), "TURN");
        assert_eq!(prompt.for_slot(15), "TURN");
    }

    #[test]
    fn deep_dive_prompt_inlines_role_and_context() {
        use crate::interview::agent::DeepDiveContext;

        let prompt = build_deep_dive_prompt(
            "# Deep dive\n\nExplore only.",
            &DeepDiveContext {
                project: "tod".into(),
                task: "node-1".into(),
                lifecycle_state: "proposed".into(),
                interview_purpose: "requirements".into(),
                interview_phase: "task-requirements-interview".into(),
                question_id: "q-002".into(),
                question_body: "What is the goal?".into(),
            },
            None,
        );
        assert!(prompt.contains("Explore only."));
        assert!(prompt.contains("q-002"));
        assert!(prompt.contains("What is the goal?"));
    }
}
