//! Fleet persistence — durable on-machine storage for tasks, agents, and related entities.

pub mod explore;
pub mod launch;
pub mod lock;
pub mod migration;
pub mod notices;
pub mod paths;
pub mod projection;
pub mod prompt_queue;
pub mod reattach;
pub mod reconnect_identity;
pub mod repos;
pub mod runtime;
pub mod schema;
pub mod store;
pub mod writer;

#[cfg(test)]
mod test_util;

#[cfg(test)]
mod tests;

pub use launch::FleetLaunchError;
pub use migration::{FleetMigrationError, HeldWritesApplyResult, MigrationMode};
pub use notices::FleetNoticeHooks;
pub use paths::FleetPaths;
pub use projection::FleetProjection;
pub use prompt_queue::MemoryPromptQueue;
pub use reattach::ReattachReport;
pub use repos::agent::{FleetAgent, NewAgent};
pub use repos::notification::FleetNotification;
pub use repos::shell::ShellSession;
pub use repos::task::FleetTask;
pub use repos::transcript::TranscriptTurn;
pub use runtime::{GuestLivenessCheck, NoopGuestLiveness, PromptDeliveryState};
pub use store::{FleetStore, QuitPromptCounts};
pub use writer::{FleetMutation, FleetWriter};
