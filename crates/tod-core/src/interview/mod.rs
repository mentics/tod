//! Interview orchestration — turning a conversation into obligations and tasks.
//!
//! Decides *what* should happen and *when* to persist it. Agent transport is
//! reached through `tod-agent`; storage through `tod-store`.

pub mod bootstrap;
pub mod client;
pub mod context;
pub mod db;
pub mod driver;
pub mod mock;
pub mod paths;
pub mod phase;
pub mod question_feedback;
pub mod routing;

#[cfg(test)]
pub(crate) mod test_support;

pub use routing::{TaskListProceedContext, interview_complete, interview_work_remains};

pub use bootstrap::bootstrap;
pub use db::{InterviewSession, InterviewSessionStatus, NewInterviewSession, SessionStore};
pub use paths::{TodPaths, set_data_root};
pub use tod_store::settings::TodSettings;

/// The `tod-cli` executable installed next to the running binary.
pub fn tod_cli_path() -> std::path::PathBuf {
    let name = if cfg!(windows) { "tod-cli.exe" } else { "tod-cli" };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .unwrap_or_else(|| std::path::PathBuf::from(name))
}
