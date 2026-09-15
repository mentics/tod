//! Fleet persistence — durable on-machine storage for tasks, agents, and related entities.

pub mod code_editor;
pub mod command_log;
pub mod explore;
pub mod launch;
pub mod lock;
pub mod migration;
pub mod mutation_socket;
pub mod node_actions;
pub mod notices;
pub mod paths;
pub mod projection;
pub mod prompt_queue;
pub mod provision;
pub mod reattach;
pub mod reconnect_identity;
pub mod repos;
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

pub use code_editor::{CodeEditor, code_editor, code_editors, open_code_editor_for_node};
pub use command_log::{CommandEntry, CommandLog};
pub use launch::FleetLaunchError;
pub use migration::{FleetMigrationError, HeldWritesApplyResult, MigrationMode};
pub use node_actions::{FilesDirectory, ResolvedAgent, ResolvedFiles};
pub use notices::FleetNoticeHooks;
pub use paths::FleetPaths;
pub use projection::FleetProjection;
pub use prompt_queue::MemoryPromptQueue;
pub use provision::{release_worktree_for_node, resolve_launch_cwd, setup_worktree_for_node};
pub use reattach::ReattachReport;
pub use repos::agent_run::AgentRun;
pub use repos::node_agent::NodeAgent;
pub use repos::node_files::NodeFiles;
pub use repos::notification::FleetNotification;
pub use repos::shell::ShellSession;
pub use repos::task::{FleetTask, NoteItem};
pub use repos::transcript::TranscriptTurn;
pub use runtime::{GuestLivenessCheck, NoopGuestLiveness, PromptDeliveryState};
pub use store::{FleetStore, QuitPromptCounts};
pub use terminal::{
    default_terminal_hint, focus_shell_session, focus_terminal_agent_run, launch_shell_terminal,
    open_shell_for_node, open_terminal_agent_for_node, prune_stale_shell_sessions,
    prune_stale_terminal_agent_runs, read_shell_state, remove_shell_state, shells_dir,
    state_file_path, verify_shell_session,
};
pub use treehouse::{
    TreehouseInvocation, resolve_worktrees_parent, sync_user_config, treehouse_available,
    treehouse_config_path, treehouse_home,
};
pub use worktree::{validate_git_repo, validate_interview_workspace};
pub use writer::{FleetMutation, FleetWriter};
