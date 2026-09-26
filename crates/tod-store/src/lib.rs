//! Durable persistence for fleet tasks, outline trees, and related storage.

pub mod conversation;
pub mod credentials;
pub mod decisions;
pub mod fleet;
pub mod incoming;
pub mod install;
pub mod interview;
pub mod journey_changes;
pub mod journey_rows;
pub mod journey_submissions;
pub mod learn;
pub mod lifecycle_baseline;
pub mod github;
pub mod linear;
pub mod log_level;
pub mod outline;
pub mod path_util;
pub mod paths;
pub mod review;
pub mod settings;
pub mod sync;
pub mod verification;

/// Agent launch options and live traffic counters live in `tod-agent` (they
/// describe how a session is started and how it is doing, not how it is
/// stored); re-exported so store consumers keep a single import path.
pub use tod_agent::agent_launch;
pub use tod_agent::agent_traffic;

pub use agent_launch::{
    AgentLaunchOptions, CLAUDE_EFFORTS, CLAUDE_MODELS, CURSOR_EFFORTS, CURSOR_MODELS,
    DEFAULT_EFFORT, coerce_effort, coerce_model, default_model_for, effort_for_acp, efforts_for,
    models_for, parse_platform, platform_storage,
};
pub use agent_traffic::SharedAgentTrafficLog;
pub use credentials::{
    CredentialBackend, CredentialError, CredentialKind, CredentialStore, resolve_linear_api_key,
};
pub use install::{InstallConfig, load_data_root, load_install_config, save_data_root};
pub use linear::{LinearError, LinearIssue, fetch_issue};
pub use log_level::LogLevel;
pub use path_util::{canonicalize_if_possible, path_for_storage, path_is_under};
pub use paths::{
    TodPaths, clear_data_root_override, is_data_root_configured, resolve_data_root,
    resolve_startup_data_root, set_data_root,
};
pub use settings::{
    AgentLaunchByPlatform, AgentPlatform, AgentRole, AgentRoleSettings, ChatLaunchMode,
    DEFAULT_LOG_MAX_SIZE_KB, InterviewContextSettings, JourneySettings, MAX_LOG_MAX_SIZE_KB,
    MIN_LOG_MAX_SIZE_KB, PlatformLaunchSettings, QuestionMakerSettings, TerminalSettings,
    TodSettings, WindowGeometry, WorktreeBackend,
};
