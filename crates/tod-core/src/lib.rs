//! Core logic shared by the `tod` GUI and the `tod-cli` command line.
//!
//! Owns policy and orchestration: interview flow, process/phase rules, bundled
//! process-doc resolution, path/settings resolution, and the task model.
//! Persistence lives in `tod-store`; agent transport lives in `tod-agent`.

/// A hash of the source `tod-cli` is compiled from (see `build.rs`). The app
/// and a `tod-cli` built from the same source report the same stamp.
pub const CLI_BUILD_STAMP: &str = env!("TOD_CLI_BUILD_STAMP");

/// The git commit this binary was built from (`git rev-parse HEAD`), or
/// `"unknown"` when git or the repository is missing (e.g. an installed
/// build from a tarball with no `.git`). See `build.rs`.
pub const GIT_COMMIT: &str = env!("TOD_GIT_COMMIT");

/// Whether the working tree had uncommitted changes at build time
/// (`git status --porcelain`), as `"true"` / `"false"`, or `"unknown"` when
/// git or the repository is missing. See `build.rs`.
pub const GIT_DIRTY: &str = env!("TOD_GIT_DIRTY");

pub mod agent_context;
pub mod codebase_rules;
pub mod context_recipes;
pub mod conversation;
pub mod dynamic;
pub mod fuzzy;
pub mod gate;
pub mod incoming;
pub mod generator;
pub mod install;
pub mod interview;
pub mod journey;
pub mod lifecycle_next;
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
