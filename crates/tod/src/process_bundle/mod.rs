//! Bundled process documentation shipped with the tod application.

mod export;
mod install;
mod launch;
mod manifest;

pub use export::{ScopeExport, resolve_and_export_scope};
pub use install::TodInstallPaths;
pub use launch::{
    AgentLaunchContext, InterviewAgentPrompt, build_deep_dive_prompt, build_fleet_agent_prompt,
    load_deep_dive_role_doc, node_scratchpad_root, session_scratchpad, workspace_cwd_for_interview,
    workspace_cwd_for_node, write_interview_config,
};
pub use manifest::{PhaseAgents, ProcessManifest};
