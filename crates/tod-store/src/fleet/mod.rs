//! Fleet persistence — durable on-machine storage for tasks, agents, and related entities.

pub mod command_log;
pub mod explore;
pub mod launch;
pub mod lock;
pub mod migration;
pub mod notices;
pub mod open_zed;
pub mod paths;
pub mod projection;
pub mod prompt_queue;
pub mod provision;
pub mod reattach;
pub mod reconnect_identity;
pub mod repos;
pub mod resolve_agent_config;
pub mod runtime;
pub mod schema;
pub mod store;
pub mod terminal;
pub mod treehouse;
pub mod undo;
pub mod worktree;
pub mod writer;

#[cfg(test)]
mod test_util;

#[cfg(test)]
mod tests;

pub use command_log::{CommandEntry, CommandLog};
pub use launch::FleetLaunchError;
pub use migration::{FleetMigrationError, HeldWritesApplyResult, MigrationMode};
pub use notices::FleetNoticeHooks;
pub use open_zed::open_zed_for_agent_config;
pub use paths::FleetPaths;
pub use projection::FleetProjection;
pub use prompt_queue::MemoryPromptQueue;
pub use provision::{
    InterviewAgentContext, describe_agent_workspace, ensure_interview_agent_for_node,
    resolve_agent_workspace, workspace_cwd_for_agent, workspace_cwd_for_interview_agent,
};
pub use reattach::ReattachReport;
pub use repos::agent_config::{AgentConfig, AgentConfigRow, FleetAgent, NewAgent, NewAgentConfig};
pub use repos::agent_run::AgentRun;
pub use repos::notification::FleetNotification;
pub use repos::shell::ShellSession;
pub use repos::task::FleetTask;
pub use repos::transcript::TranscriptTurn;
pub use resolve_agent_config::ResolvedAgentConfigs;
pub use runtime::{GuestLivenessCheck, NoopGuestLiveness, PromptDeliveryState};
pub use store::{FleetStore, QuitPromptCounts};
pub use terminal::{
    default_terminal_hint, focus_shell_session, focus_terminal_agent_run, launch_shell_terminal,
    open_shell_for_agent_config, open_shell_with_command, open_terminal_agent_for_config,
    prune_stale_shell_sessions, prune_stale_terminal_agent_runs, read_shell_state,
    remove_shell_state, shells_dir, state_file_path, verify_shell_session,
};
pub use treehouse::{
    TreehouseInvocation, resolve_worktrees_parent, sync_user_config, treehouse_available,
    treehouse_config_path, treehouse_home,
};
pub use worktree::{validate_git_repo, validate_interview_workspace};
pub use writer::{FleetMutation, FleetWriter};
