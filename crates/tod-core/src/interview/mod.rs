//! Interview orchestration — turning a conversation into obligations and tasks.
//!
//! Decides *what* should happen and *when* to persist it. Agent transport is
//! reached through `tod-agent`; storage through `tod-store`.

pub mod bootstrap;
pub mod config;
pub mod db;
pub mod kickoff;
pub mod paths;
pub mod question_feedback;
pub mod queue;
pub mod queue_watcher;
pub mod replenishment;
pub mod routing;
pub mod transcript;

pub use routing::{TaskListProceedContext, interview_work_remains};

pub use bootstrap::bootstrap;
pub use db::{InterviewSession, InterviewSessionStatus, NewInterviewSession, SessionStore};
pub use paths::{TodPaths, set_data_root};
pub use tod_store::settings::TodSettings;
