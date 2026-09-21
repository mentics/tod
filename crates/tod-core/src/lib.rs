//! Core logic shared by the `tod` GUI and the `tod-cli` command line.
//!
//! Owns policy and orchestration: interview flow, process/phase rules, bundled
//! process-doc resolution, path/settings resolution, and the task model.
//! Persistence lives in `tod-store`; agent transport lives in `tod-agent`.

pub mod agent_context;
pub mod context_recipes;
pub mod conversation;
pub mod dynamic;
pub mod fuzzy;
pub mod gate;
pub mod generator;
pub mod install;
pub mod interview;
pub mod lifecycle_validity;
pub mod linear_import;
pub mod logging;
pub mod media;
pub mod node_context;
pub mod process;
pub mod process_bundle;
pub mod run_transcript;
pub mod session_name;
pub mod task;

pub use interview::{TodPaths, set_data_root};

/// Persisted settings types live in `tod-store`; re-exported here so core and
/// CLI callers have a single import path. GUI-only geometry helpers stay in the
/// UI crate.
pub mod settings {
    pub use tod_store::settings::{
        InterviewContextSettings, MAX_LOG_MAX_SIZE_KB, MIN_LOG_MAX_SIZE_KB, QuestionMakerSettings,
        TodSettings, WindowGeometry, WorktreeBackend,
    };
}
