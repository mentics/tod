//! Bundled process documentation shipped with the tod application.

mod install;
mod launch;
mod manifest;

pub use install::TodInstallPaths;
pub use launch::{build_fleet_agent_prompt, interview_session_prefix, state_lifecycle_doc, state_role_doc, state_working_doc};
pub use manifest::ProcessManifest;
