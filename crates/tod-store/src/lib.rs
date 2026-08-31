//! Durable persistence for fleet tasks, outline trees, and related storage.

pub mod agent_traffic;
pub mod fleet;
pub mod install;
pub mod linear;
pub mod log_level;
pub mod outline;
pub mod path_util;
pub mod paths;
pub mod settings;

pub use agent_traffic::SharedAgentTrafficLog;
pub use install::{InstallConfig, load_data_root, load_install_config, save_data_root};
pub use linear::{LinearError, LinearIssue, fetch_issue, resolve_api_key};
pub use log_level::LogLevel;
pub use path_util::path_for_storage;
pub use paths::{
    TodPaths, clear_data_root_override, is_data_root_configured, resolve_data_root,
    resolve_startup_data_root, set_data_root,
};
pub use settings::{
    AgentPlatform, AnswerProcessorSettings, DEFAULT_LOG_MAX_SIZE_KB, LinearSettings,
    MAX_LOG_MAX_SIZE_KB, MIN_LOG_MAX_SIZE_KB, QuestionMakerSettings, TerminalSettings, TodSettings,
    WindowGeometry, WorktreeBackend,
};
