//! Core logic shared by the `tod` GUI and the `tod-cli` command line.
//!
//! Owns policy and orchestration: interview flow, process/phase rules, bundled
//! process-doc resolution, path/settings resolution, and the task model.
//! Persistence lives in `tod-store`; agent transport lives in `tod-agent`.

pub mod agent_context;
pub mod install;
pub mod interview;
pub mod linear_import;
pub mod logging;
pub mod media;
pub mod process;
pub mod process_bundle;
pub mod task;

pub use interview::{TodPaths, set_data_root};

/// Persisted settings types live in `tod-store`; re-exported here so core and
/// CLI callers have a single import path. GUI-only geometry helpers stay in the
/// UI crate.
pub mod settings {
    use tod_agent::SessionPoolConfig;

    /// Pool config for question-maker replenishment.
    ///
    /// `replenish_threshold` and `second_question_maker_threshold` are interview
    /// policy and stay in core (see `interview::replenishment`); only session
    /// reuse reaches the transport layer.
    pub fn question_maker_pool(settings: &QuestionMakerSettings) -> SessionPoolConfig {
        SessionPoolConfig::new(
            tod_agent::RESEARCHER_SESSION_POOL_SIZE,
            settings.runs_per_session,
        )
    }

    /// Pool config for answer processing.
    pub fn answer_processor_pool(settings: &AnswerProcessorSettings) -> SessionPoolConfig {
        SessionPoolConfig::new(settings.session_pool_size, settings.answers_per_session)
    }

    pub use tod_store::settings::{
        AnswerProcessorSettings, MAX_LOG_MAX_SIZE_KB, MIN_LOG_MAX_SIZE_KB, QuestionMakerSettings,
        TodSettings, WindowGeometry, WorktreeBackend,
    };
}
