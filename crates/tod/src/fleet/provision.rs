//! Auto-provision interview-mode agent configs with worktrees.

use crate::fleet::FleetStore;
use crate::fleet::repos::agent_config::{AgentConfigRepo, AgentConfigRow, NewAgentConfig};
use crate::fleet::worktree::{self, WorktreeHandle, validate_git_repo};
use crate::fleet::writer::FleetMutation;
use crate::interview::settings::TodSettings;
use crate::interview::{TodPaths, config::path_for_storage};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct InterviewAgentContext {
    pub agent: AgentConfigRow,
    pub cwd: PathBuf,
}

fn task_repo_branch(fleet: &FleetStore, node_id: &str) -> Result<(PathBuf, String)> {
    let task = fleet
        .get_task(node_id)?
        .with_context(|| format!("task {node_id} not found"))?;
    let repo = task
        .repo
        .filter(|s| !s.trim().is_empty())
        .with_context(|| "set repository on task before starting interview")?;
    let repo_path = validate_git_repo(PathBuf::from(&repo).as_path())?;
    let branch = task.branch.unwrap_or_default();
    Ok((repo_path, branch))
}

fn ensure_worktree_path_valid(agent: &AgentConfigRow) -> Result<()> {
    if agent.use_worktree {
        if let Some(path) = agent.worktree_path.as_deref() {
            if std::path::Path::new(path).is_dir() {
                return Ok(());
            }
        }
        bail!("interview agent worktree missing; re-provision required");
    }
    Ok(())
}

pub fn workspace_cwd_for_agent(agent: &AgentConfigRow) -> Result<PathBuf> {
    if agent.use_worktree {
        if let Some(path) = agent.worktree_path.as_deref() {
            let p = PathBuf::from(path);
            if p.is_dir() {
                return Ok(p);
            }
        }
        bail!("agent worktree path missing or invalid");
    }
    if let Some(dir) = agent.work_directory.as_deref().filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    bail!("agent has no worktree or work directory")
}

fn repo_root_from_task(fleet: &FleetStore, node_id: &str) -> Result<PathBuf> {
    let task = fleet
        .get_task(node_id)?
        .with_context(|| format!("task {node_id} not found"))?;
    let repo = task
        .repo
        .filter(|s| !s.trim().is_empty())
        .with_context(|| "set repository on the task (task edit) before using this agent config")?;
    validate_git_repo(PathBuf::from(&repo).as_path())
}

/// Resolve the workspace directory for an agent config.
///
/// - Worktree enabled: provision or reuse an isolated worktree under the data root.
/// - Otherwise: optional explicit `work_directory`, else the task's git repository root.
pub fn resolve_agent_workspace(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    agent: &AgentConfigRow,
    node_id: &str,
) -> Result<PathBuf> {
    if agent.use_worktree {
        return ensure_worktree_for_agent_config(fleet, paths, settings, &agent.id, node_id);
    }
    if let Some(dir) = agent.work_directory.as_deref().filter(|s| !s.is_empty()) {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return Ok(path);
        }
        bail!("work directory does not exist: {}", path.display());
    }
    repo_root_from_task(fleet, node_id)
}

fn ensure_worktree_for_agent_config(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    config_id: &str,
    node_id: &str,
) -> Result<PathBuf> {
    fleet.reload_if_stale().ok();
    if let Some(agent) = fleet.get_agent(config_id)? {
        if let Some(path) = agent.worktree_path.as_deref() {
            let p = PathBuf::from(path);
            if p.is_dir() {
                return Ok(p);
            }
        }
    }

    let task = fleet
        .get_task(node_id)?
        .with_context(|| format!("task {node_id} not found"))?;
    let repo = task
        .repo
        .filter(|s| !s.trim().is_empty())
        .with_context(|| "set repository on the task before enabling a worktree")?;
    let repo_path = validate_git_repo(PathBuf::from(&repo).as_path())?;
    let branch = task.branch.unwrap_or_default();
    let lease_holder = format!("tod-{config_id}");
    let data_root = settings.resolve_fleet_storage_root(paths)?;

    let handle: WorktreeHandle = {
        let projection = fleet.projection();
        let guard = projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        worktree::ensure_worktree(
            &conn,
            settings.worktree_backend,
            &data_root,
            &repo_path,
            &branch,
            &lease_holder,
        )?
    };

    fleet.enqueue(FleetMutation::UpdateAgentWorktreeDetails {
        id: config_id.to_string(),
        worktree_path: Some(path_for_storage(&handle.path)),
        worktree_lease_id: handle.lease.as_ref().map(|l| l.lease_id.clone()),
        worktree_lease_holder: handle.lease.as_ref().map(|l| l.lease_holder.clone()),
    })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;

    let path = handle.path;
    if !path.is_dir() {
        bail!("provisioned worktree missing at {}", path.display());
    }
    Ok(path)
}

/// Describe where this config would run (for UI), without provisioning a worktree.
pub fn describe_agent_workspace(
    fleet: &FleetStore,
    agent: &AgentConfigRow,
    node_id: &str,
) -> String {
    if agent.use_worktree {
        if let Some(path) = agent.worktree_path.as_deref().filter(|p| !p.is_empty()) {
            return format!("Worktree: {path}");
        }
        return "Worktree: (created on first launch)".into();
    }
    if let Some(dir) = agent.work_directory.as_deref().filter(|s| !s.is_empty()) {
        return format!("Override: {dir}");
    }
    match repo_root_from_task(fleet, node_id) {
        Ok(path) => format!("Repository: {}", path.display()),
        Err(err) => format!("{err:#}"),
    }
}

/// Ensure an interview-mode agent config exists for `node_id`, with a worktree when needed.
pub fn ensure_interview_agent_for_node(
    fleet: &FleetStore,
    paths: &TodPaths,
    settings: &TodSettings,
    node_id: &str,
) -> Result<InterviewAgentContext> {
    fleet.reload_if_stale().ok();
    {
        let projection = fleet.projection();
        let guard = projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        if let Some(existing) = AgentConfigRepo::new(&conn).find_interview_for_node(node_id)? {
            ensure_worktree_path_valid(&existing)?;
            let cwd = workspace_cwd_for_agent(&existing)?;
            return Ok(InterviewAgentContext {
                agent: existing,
                cwd,
            });
        }
    }

    let (repo_path, branch) = task_repo_branch(fleet, node_id)?;
    let config_id = format!("interview-{}", Uuid::new_v4());
    let lease_holder = format!("tod-{config_id}");
    let data_root = settings.resolve_fleet_storage_root(paths)?;
    let backend = settings.worktree_backend;

    let handle: WorktreeHandle = {
        let projection = fleet.projection();
        let guard = projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        worktree::ensure_worktree(
            &conn,
            backend,
            &data_root,
            &repo_path,
            &branch,
            &lease_holder,
        )?
    };

    fleet.enqueue(FleetMutation::InsertAgent {
        agent: NewAgentConfig {
            id: config_id.clone(),
            node_id: node_id.to_string(),
            env_type: "local".into(),
            mode: "interview".into(),
            work_directory: None,
            use_worktree: true,
        },
    })?;
    fleet.enqueue(FleetMutation::UpdateAgentWorktreeDetails {
        id: config_id.clone(),
        worktree_path: Some(path_for_storage(&handle.path)),
        worktree_lease_id: handle.lease.as_ref().map(|l| l.lease_id.clone()),
        worktree_lease_holder: handle.lease.as_ref().map(|l| l.lease_holder.clone()),
    })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;

    let agent = fleet
        .get_agent(&config_id)?
        .with_context(|| "provisioned interview agent not found")?;
    let cwd = workspace_cwd_for_agent(&agent)?;
    Ok(InterviewAgentContext { agent, cwd })
}

/// Resolve ACP cwd from agent config id (for interview sessions).
pub fn workspace_cwd_for_interview_agent(
    fleet: &FleetStore,
    agent_config_id: &str,
    paths: &TodPaths,
    node_id: uuid::Uuid,
) -> Result<PathBuf> {
    if let Some(agent) = fleet.get_agent(agent_config_id)? {
        if let Ok(cwd) = workspace_cwd_for_agent(&agent) {
            return Ok(cwd);
        }
    }
    workspace_cwd_for_node_fallback(fleet, node_id, paths)
}

fn workspace_cwd_for_node_fallback(
    fleet: &FleetStore,
    node_id: uuid::Uuid,
    paths: &TodPaths,
) -> Result<PathBuf> {
    let projection = fleet.projection();
    let guard = projection.lock().expect("fleet projection mutex");
    let conn = guard.connection();
    crate::process_bundle::workspace_cwd_for_node(&conn, node_id, paths)
}
