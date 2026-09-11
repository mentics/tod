//! Interview persistence: the question queue and its history, agent memory,
//! the change log interview agents' context deltas are built from, and the
//! agent sessions that consume those deltas.
//!
//! Every write goes through [`InterviewCommand`], executed on the fleet
//! writer. The acting party (the user, the app, or an interview agent
//! session) is recorded with each change so a session never receives its own
//! writes back as news.

mod command;
mod repo;
mod types;

pub use command::{InterviewCommand, execute};
pub use repo::{InterviewRepo, short_id};
pub use types::*;
