use crate::log_level::LogLevel;
use crate::paths::TodPaths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const DEFAULT_REPLENISH_THRESHOLD: u32 = 8;
const DEFAULT_CONTEXT_BUDGET_TOKENS: u64 = 100_000;
const DEFAULT_PROMPT_CACHE_IDLE_MINUTES: u64 = 5;
const DEFAULT_ANSWERED_HISTORY_CAP: u32 = 100;
pub const DEFAULT_LOG_MAX_SIZE_KB: u64 = 51_200;
pub const MIN_LOG_MAX_SIZE_KB: u64 = 1;
pub const MAX_LOG_MAX_SIZE_KB: u64 = 104_857_600;

/// Agent platform lives in `tod-agent` (it describes the agent, not storage);
/// re-exported here because it is persisted in `tod.yml` and agent config rows.
pub use tod_agent::AgentPlatform;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionMakerSettings {
    /// Target number of open questions; a question maker run starts below it,
    /// and the answer processor fans out when unprocessed answers exceed half.
    #[serde(default = "default_replenish_threshold")]
    pub replenish_threshold: u32,
}

impl Default for QuestionMakerSettings {
    fn default() -> Self {
        Self {
            replenish_threshold: DEFAULT_REPLENISH_THRESHOLD,
        }
    }
}

fn default_replenish_threshold() -> u32 {
    DEFAULT_REPLENISH_THRESHOLD
}

/// How interview agent sessions keep their context small.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterviewContextSettings {
    /// Rotate a session to a fresh snapshot once its estimated context exceeds this.
    #[serde(default = "default_context_budget_tokens")]
    pub context_budget_tokens: u64,
    /// Idle time after which a resumed session would miss the provider's prompt cache.
    #[serde(default = "default_prompt_cache_idle_minutes")]
    pub prompt_cache_idle_minutes: u64,
    /// Most answered questions (current phase) included in a snapshot.
    #[serde(default = "default_answered_history_cap")]
    pub answered_history_cap: u32,
}

impl Default for InterviewContextSettings {
    fn default() -> Self {
        Self {
            context_budget_tokens: DEFAULT_CONTEXT_BUDGET_TOKENS,
            prompt_cache_idle_minutes: DEFAULT_PROMPT_CACHE_IDLE_MINUTES,
            answered_history_cap: DEFAULT_ANSWERED_HISTORY_CAP,
        }
    }
}

fn default_context_budget_tokens() -> u64 {
    DEFAULT_CONTEXT_BUDGET_TOKENS
}

fn default_prompt_cache_idle_minutes() -> u64 {
    DEFAULT_PROMPT_CACHE_IDLE_MINUTES
}

fn default_answered_history_cap() -> u32 {
    DEFAULT_ANSWERED_HISTORY_CAP
}

fn default_log_level() -> LogLevel {
    LogLevel::Info
}

fn default_log_max_size_kb() -> u64 {
    DEFAULT_LOG_MAX_SIZE_KB
}

fn default_worktree_backend() -> WorktreeBackend {
    WorktreeBackend::TreehouseWithGitFallback
}

fn default_agent_platform() -> AgentPlatform {
    AgentPlatform::Claude
}

fn default_terminal_settings() -> TerminalSettings {
    TerminalSettings::default()
}

/// Model and effort for one agent platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformLaunchSettings {
    pub model: String,
    pub effort: String,
}

impl PlatformLaunchSettings {
    pub fn for_platform(platform: AgentPlatform) -> Self {
        Self {
            model: crate::agent_launch::default_model_for(platform).to_string(),
            effort: crate::agent_launch::DEFAULT_EFFORT.to_string(),
        }
    }
}

/// Per-platform interview launch defaults (model / effort).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLaunchByPlatform {
    #[serde(default = "default_claude_launch")]
    pub claude: PlatformLaunchSettings,
    #[serde(default = "default_cursor_launch")]
    pub cursor: PlatformLaunchSettings,
}

fn default_claude_launch() -> PlatformLaunchSettings {
    PlatformLaunchSettings::for_platform(AgentPlatform::Claude)
}

fn default_cursor_launch() -> PlatformLaunchSettings {
    PlatformLaunchSettings::for_platform(AgentPlatform::Cursor)
}

impl Default for AgentLaunchByPlatform {
    fn default() -> Self {
        Self {
            claude: default_claude_launch(),
            cursor: default_cursor_launch(),
        }
    }
}

impl AgentLaunchByPlatform {
    pub fn get(&self, platform: AgentPlatform) -> &PlatformLaunchSettings {
        match platform {
            AgentPlatform::Claude => &self.claude,
            AgentPlatform::Cursor => &self.cursor,
        }
    }

    pub fn get_mut(&mut self, platform: AgentPlatform) -> &mut PlatformLaunchSettings {
        match platform {
            AgentPlatform::Claude => &mut self.claude,
            AgentPlatform::Cursor => &mut self.cursor,
        }
    }
}

/// Which agent a set of platform / model / effort settings is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    /// Coding agents launched from a node (unset Agent capability values), and
    /// anything without a more specific setting.
    Default,
    /// Chatting with an agent from a panel (unset Agent capability values).
    Chat,
    /// Interview question-maker / answer-processor work.
    Interview,
}

impl AgentRole {
    /// Settings display order.
    pub const ALL: [AgentRole; 3] = [Self::Default, Self::Chat, Self::Interview];

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default agent",
            Self::Chat => "Chat with agent",
            Self::Interview => "Interview agent",
        }
    }
}

/// Platform plus per-platform model / effort for one [`AgentRole`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentRoleSettings {
    #[serde(default = "default_agent_platform")]
    pub platform: AgentPlatform,
    #[serde(default)]
    pub launch: AgentLaunchByPlatform,
}

/// External terminal program for agent shell sessions (`None` = OS default).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TerminalSettings {
    /// Executable name or path, e.g. `wt.exe`, `powershell.exe`, `/usr/bin/alacritty`.
    #[serde(default)]
    pub program: Option<String>,
}

/// Where "chat with agent" opens a session: the app's own window, or an
/// external terminal running the platform CLI directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatLaunchMode {
    Window,
    Terminal,
}

impl ChatLaunchMode {
    pub const ALL: [ChatLaunchMode; 2] = [Self::Window, Self::Terminal];

    pub fn label(self) -> &'static str {
        match self {
            Self::Window => "App window",
            Self::Terminal => "Terminal",
        }
    }
}

impl Default for ChatLaunchMode {
    fn default() -> Self {
        Self::Window
    }
}

fn default_chat_launch_mode() -> ChatLaunchMode {
    ChatLaunchMode::default()
}

/// Default for [`TodSettings::max_parallel_agent_sessions`].
pub const DEFAULT_MAX_PARALLEL_AGENT_SESSIONS: u32 = 4;
/// Bounds for [`TodSettings::max_parallel_agent_sessions`].
pub const MAX_PARALLEL_AGENT_SESSIONS_RANGE: (u32, u32) = (1, 16);

fn default_max_parallel_agent_sessions() -> u32 {
    DEFAULT_MAX_PARALLEL_AGENT_SESSIONS
}

/// How Tod provisions git worktrees for interview / agent workspaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeBackend {
    TreehouseWithGitFallback,
    TreehouseRequired,
    GitOnly,
}

impl Default for WorktreeBackend {
    fn default() -> Self {
        Self::TreehouseWithGitFallback
    }
}

/// Saved main-window placement restored on the next launch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub maximized: bool,
}

impl WindowGeometry {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("x", self.x),
            ("y", self.y),
            ("width", self.width),
            ("height", self.height),
        ] {
            if !value.is_finite() {
                bail!("window_geometry.{name} must be finite, got {value}");
            }
        }
        if self.width <= 0.0 {
            bail!("window_geometry.width must be positive, got {}", self.width);
        }
        if self.height <= 0.0 {
            bail!(
                "window_geometry.height must be positive, got {}",
                self.height
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodSettings {
    #[serde(default, alias = "researcher")]
    pub question_maker: QuestionMakerSettings,
    #[serde(default)]
    pub interview_context: InterviewContextSettings,
    #[serde(default = "default_log_level")]
    pub log_level: LogLevel,
    #[serde(default = "default_log_max_size_kb")]
    pub log_max_size_kb: u64,
    /// Fleet persistence storage root. When unset, uses the OS default on first resolve.
    #[serde(default)]
    pub fleet_storage_root: Option<PathBuf>,
    /// Keep the main window above other windows (Windows only).
    #[serde(default)]
    pub always_on_top: bool,
    /// Worktree provisioning backend for interview agents.
    #[serde(default = "default_worktree_backend")]
    pub worktree_backend: WorktreeBackend,
    /// Parent directory for Treehouse worktree pools (`TREEHOUSE_WORKTREES`). When unset, pools live under `TREEHOUSE_HOME`.
    #[serde(default)]
    pub treehouse_worktrees_root: Option<PathBuf>,
    /// The Treehouse executable. When unset, `treehouse` is looked up on PATH.
    #[serde(default)]
    pub treehouse_executable: Option<PathBuf>,
    /// Platform / model / effort for coding agents, where the node's Agent capability leaves them unset.
    #[serde(default)]
    pub default_agent: AgentRoleSettings,
    /// Platform / model / effort for "chat with agent", where the node's Agent capability leaves them unset.
    #[serde(default)]
    pub chat_agent: AgentRoleSettings,
    /// Where "chat with agent" opens a session: app window or external terminal.
    #[serde(default = "default_chat_launch_mode")]
    pub chat_launch_mode: ChatLaunchMode,
    /// How many agent sessions batch work (checking incoming changes on
    /// several nodes) runs at once.
    #[serde(default = "default_max_parallel_agent_sessions")]
    pub max_parallel_agent_sessions: u32,
    /// Which agent platform runs interview question-maker / answer-processor work.
    #[serde(default = "default_agent_platform")]
    pub agent_platform: AgentPlatform,
    /// Per-platform model and effort for interview agent launches.
    #[serde(default)]
    pub agent_launch: AgentLaunchByPlatform,
    /// Legacy flat model from older `tod.yml`; migrated into `agent_launch` on load.
    #[serde(default, rename = "agent_model", skip_serializing)]
    pub(crate) legacy_agent_model: Option<String>,
    /// Legacy flat effort from older `tod.yml`; migrated into `agent_launch` on load.
    #[serde(default, rename = "agent_effort", skip_serializing)]
    pub(crate) legacy_agent_effort: Option<String>,
    /// Terminal emulator for interactive shell sessions.
    #[serde(default = "default_terminal_settings")]
    pub terminal: TerminalSettings,
    /// Last known main-window placement.
    #[serde(default)]
    pub window_geometry: Option<WindowGeometry>,
}

/// The Treehouse executable when none is configured: found on PATH.
pub const DEFAULT_TREEHOUSE_EXECUTABLE: &str = "treehouse";

impl Default for TodSettings {
    fn default() -> Self {
        Self {
            question_maker: QuestionMakerSettings::default(),
            interview_context: InterviewContextSettings::default(),
            log_level: LogLevel::Info,
            log_max_size_kb: DEFAULT_LOG_MAX_SIZE_KB,
            fleet_storage_root: None,
            always_on_top: false,
            worktree_backend: WorktreeBackend::default(),
            treehouse_worktrees_root: None,
            treehouse_executable: None,
            default_agent: AgentRoleSettings::default(),
            chat_agent: AgentRoleSettings::default(),
            chat_launch_mode: ChatLaunchMode::default(),
            max_parallel_agent_sessions: DEFAULT_MAX_PARALLEL_AGENT_SESSIONS,
            agent_platform: AgentPlatform::default(),
            agent_launch: AgentLaunchByPlatform::default(),
            legacy_agent_model: None,
            legacy_agent_effort: None,
            terminal: TerminalSettings::default(),
            window_geometry: None,
        }
    }
}

impl TodSettings {
    /// Interview settings predate roles and keep their flat `tod.yml` keys.
    fn role_slot(&self, role: AgentRole) -> (AgentPlatform, &AgentLaunchByPlatform) {
        match role {
            AgentRole::Default => (self.default_agent.platform, &self.default_agent.launch),
            AgentRole::Chat => (self.chat_agent.platform, &self.chat_agent.launch),
            AgentRole::Interview => (self.agent_platform, &self.agent_launch),
        }
    }

    fn role_slot_mut(
        &mut self,
        role: AgentRole,
    ) -> (&mut AgentPlatform, &mut AgentLaunchByPlatform) {
        match role {
            AgentRole::Default => (
                &mut self.default_agent.platform,
                &mut self.default_agent.launch,
            ),
            AgentRole::Chat => (&mut self.chat_agent.platform, &mut self.chat_agent.launch),
            AgentRole::Interview => (&mut self.agent_platform, &mut self.agent_launch),
        }
    }

    pub fn platform_for(&self, role: AgentRole) -> AgentPlatform {
        self.role_slot(role).0
    }

    pub fn set_platform_for(&mut self, role: AgentRole, platform: AgentPlatform) {
        *self.role_slot_mut(role).0 = platform;
    }

    /// Model for the role's active platform.
    pub fn model_for(&self, role: AgentRole) -> &str {
        let (platform, launch) = self.role_slot(role);
        &launch.get(platform).model
    }

    /// Effort for the role's active platform.
    pub fn effort_for(&self, role: AgentRole) -> &str {
        let (platform, launch) = self.role_slot(role);
        &launch.get(platform).effort
    }

    /// Update model for the role's active platform.
    pub fn set_model_for(&mut self, role: AgentRole, model: impl Into<String>) {
        let (platform, launch) = self.role_slot_mut(role);
        let platform = *platform;
        launch.get_mut(platform).model = crate::agent_launch::coerce_model(platform, &model.into());
    }

    /// Update effort for the role's active platform.
    pub fn set_effort_for(&mut self, role: AgentRole, effort: impl Into<String>) {
        let (platform, launch) = self.role_slot_mut(role);
        let platform = *platform;
        launch.get_mut(platform).effort =
            crate::agent_launch::coerce_effort(platform, &effort.into());
    }

    /// Launch options for the role's active platform.
    pub fn launch_options_for(&self, role: AgentRole) -> crate::agent_launch::AgentLaunchOptions {
        crate::agent_launch::AgentLaunchOptions::from_settings(
            self.platform_for(role),
            self.model_for(role),
            self.effort_for(role),
        )
    }

    /// Launch options for the currently selected interview platform.
    pub fn interview_launch_options(&self) -> crate::agent_launch::AgentLaunchOptions {
        self.launch_options_for(AgentRole::Interview)
    }

    /// Model for the active interview platform.
    pub fn agent_model(&self) -> &str {
        self.model_for(AgentRole::Interview)
    }

    /// Effort for the active interview platform.
    pub fn agent_effort(&self) -> &str {
        self.effort_for(AgentRole::Interview)
    }

    /// Update model for the active interview platform.
    pub fn set_agent_model(&mut self, model: impl Into<String>) {
        self.set_model_for(AgentRole::Interview, model);
    }

    /// Update effort for the active interview platform.
    pub fn set_agent_effort(&mut self, effort: impl Into<String>) {
        self.set_effort_for(AgentRole::Interview, effort);
    }

    /// Apply legacy flat `agent_model` / `agent_effort` into the active platform slot.
    fn migrate_legacy_agent_launch(&mut self) {
        let platform = self.agent_platform;
        if let Some(model) = self.legacy_agent_model.take() {
            self.agent_launch.get_mut(platform).model =
                crate::agent_launch::coerce_model(platform, &model);
        }
        if let Some(effort) = self.legacy_agent_effort.take() {
            self.agent_launch.get_mut(platform).effort =
                crate::agent_launch::coerce_effort(platform, &effort);
        }
    }

    pub fn load(paths: &TodPaths) -> Result<Self> {
        paths.ensure_config_dir()?;
        Self::load_from_path(&paths.settings_path())
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read settings from {}", path.display()))?;
        if contents.trim().is_empty() {
            return Ok(Self::default());
        }
        let value: serde_yaml::Value = serde_yaml::from_str(&contents)
            .with_context(|| format!("failed to parse settings YAML from {}", path.display()))?;
        let has_agent_launch = value
            .as_mapping()
            .is_some_and(|m| m.contains_key(serde_yaml::Value::from("agent_launch")));
        let mut settings: Self = serde_yaml::from_value(value).with_context(|| {
            format!(
                "failed to deserialize settings YAML from {}",
                path.display()
            )
        })?;
        if has_agent_launch {
            settings.legacy_agent_model = None;
            settings.legacy_agent_effort = None;
        } else {
            settings.migrate_legacy_agent_launch();
        }
        settings.validate()?;
        Ok(settings)
    }

    pub fn save(&self, paths: &TodPaths) -> Result<()> {
        paths.ensure_config_dir()?;
        self.save_to_path(&paths.settings_path())
    }

    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let contents = serde_yaml::to_string(self).context("failed to serialize settings YAML")?;
        std::fs::write(path, contents)
            .with_context(|| format!("failed to write settings to {}", path.display()))
    }

    pub fn validate(&self) -> Result<()> {
        if !(MIN_LOG_MAX_SIZE_KB..=MAX_LOG_MAX_SIZE_KB).contains(&self.log_max_size_kb) {
            bail!(
                "log_max_size_kb must be between {MIN_LOG_MAX_SIZE_KB} and {MAX_LOG_MAX_SIZE_KB}, got {}",
                self.log_max_size_kb
            );
        }
        if let Some(geometry) = &self.window_geometry {
            geometry.validate()?;
        }
        Ok(())
    }

    /// The parallel-session cap, within its bounds.
    pub fn parallel_agent_sessions(&self) -> usize {
        let (lo, hi) = MAX_PARALLEL_AGENT_SESSIONS_RANGE;
        self.max_parallel_agent_sessions.clamp(lo, hi) as usize
    }

    pub fn clamp_log_max_size_kb(value: u64) -> u64 {
        value.clamp(MIN_LOG_MAX_SIZE_KB, MAX_LOG_MAX_SIZE_KB)
    }

    /// Resolved fleet storage root: explicit setting or data root.
    pub fn resolve_fleet_storage_root(&self, paths: &TodPaths) -> Result<PathBuf> {
        let root = match &self.fleet_storage_root {
            Some(path) => path.clone(),
            None => paths.fleet_storage_root(),
        };
        crate::fleet::paths::normalize_absolute(&root)
    }

    /// The Treehouse executable to run: the configured one, else `treehouse`
    /// from PATH.
    pub fn treehouse_program(&self) -> PathBuf {
        self.treehouse_executable
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_TREEHOUSE_EXECUTABLE))
    }

    /// Write Treehouse user config under the data root after settings change.
    pub fn sync_treehouse_config(&self, paths: &TodPaths) -> Result<()> {
        crate::fleet::treehouse::sync_user_config(self, paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{clear_data_root_override, set_data_root};
    use std::fs;

    #[test]
    fn defaults_when_missing() {
        let dir = std::env::temp_dir().join(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        let settings = TodSettings::load_from_path(&path).unwrap();
        assert_eq!(settings.question_maker.replenish_threshold, 8);
        assert_eq!(settings.interview_context.context_budget_tokens, 100_000);
        assert_eq!(settings.interview_context.answered_history_cap, 100);
        assert_eq!(settings.log_level, LogLevel::Info);
        assert_eq!(settings.log_max_size_kb, DEFAULT_LOG_MAX_SIZE_KB);
        assert_eq!(settings.fleet_storage_root, None);
        assert!(!settings.always_on_top);
        assert_eq!(
            settings.worktree_backend,
            WorktreeBackend::TreehouseWithGitFallback
        );
        assert_eq!(settings.agent_platform, AgentPlatform::Claude);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_fleet_storage_root_uses_data_root() {
        let sandbox =
            std::env::temp_dir().join(format!("tod-fleet-sandbox-{}", uuid::Uuid::new_v4()));
        set_data_root(sandbox.clone());
        let paths = TodPaths::discover().unwrap();
        let settings = TodSettings::default();
        let root = settings.resolve_fleet_storage_root(&paths).unwrap();
        assert_eq!(root, sandbox);
        clear_data_root_override();
        let _ = fs::remove_dir_all(sandbox);
    }

    #[test]
    fn resolve_fleet_storage_root_uses_setting() {
        let settings = TodSettings {
            fleet_storage_root: Some(PathBuf::from("/tmp/custom-fleet-root")),
            ..TodSettings::default()
        };
        let sandbox =
            std::env::temp_dir().join(format!("tod-fleet-sandbox-{}", uuid::Uuid::new_v4()));
        set_data_root(sandbox.clone());
        let paths = TodPaths::discover().unwrap();
        let root = settings.resolve_fleet_storage_root(&paths).unwrap();
        assert!(root.is_absolute());
        assert!(root.ends_with("custom-fleet-root"));
        clear_data_root_override();
        let _ = fs::remove_dir_all(sandbox);
    }

    #[test]
    fn round_trip_yaml() {
        let dir = std::env::temp_dir().join(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        let settings = TodSettings {
            question_maker: QuestionMakerSettings {
                replenish_threshold: 10,
            },
            interview_context: InterviewContextSettings::default(),
            log_level: LogLevel::Debug,
            log_max_size_kb: 1024,
            fleet_storage_root: None,
            always_on_top: true,
            worktree_backend: WorktreeBackend::default(),
            treehouse_worktrees_root: None,
            treehouse_executable: Some(PathBuf::from("C:/tools/treehouse.exe")),
            default_agent: AgentRoleSettings {
                platform: AgentPlatform::Cursor,
                launch: AgentLaunchByPlatform {
                    claude: PlatformLaunchSettings::for_platform(AgentPlatform::Claude),
                    cursor: PlatformLaunchSettings {
                        model: "composer-2.5".into(),
                        effort: "low".into(),
                    },
                },
            },
            chat_agent: AgentRoleSettings::default(),
            chat_launch_mode: ChatLaunchMode::Terminal,
            max_parallel_agent_sessions: 2,
            agent_platform: AgentPlatform::Claude,
            agent_launch: AgentLaunchByPlatform {
                claude: PlatformLaunchSettings {
                    model: "opus".into(),
                    effort: "high".into(),
                },
                cursor: PlatformLaunchSettings::for_platform(AgentPlatform::Cursor),
            },
            legacy_agent_model: None,
            legacy_agent_effort: None,
            terminal: TerminalSettings {
                program: Some(r"C:\app\dev\Git\git-bash.exe".into()),
            },
            window_geometry: Some(WindowGeometry {
                x: 120.0,
                y: 80.0,
                width: 1440.0,
                height: 900.0,
                maximized: false,
            }),
        };
        settings.save_to_path(&path).unwrap();
        let loaded = TodSettings::load_from_path(&path).unwrap();
        assert_eq!(loaded, settings);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn migrates_legacy_flat_model_effort_into_active_platform() {
        let dir = std::env::temp_dir().join(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        fs::write(
            &path,
            "agent_platform: cursor\nagent_model: composer-2.5\nagent_effort: high\n",
        )
        .unwrap();
        let loaded = TodSettings::load_from_path(&path).unwrap();
        assert_eq!(loaded.agent_platform, AgentPlatform::Cursor);
        assert_eq!(loaded.agent_launch.cursor.model, "composer-2.5");
        assert_eq!(loaded.agent_launch.cursor.effort, "high");
        assert_eq!(
            loaded.agent_launch.claude.model,
            crate::agent_launch::default_model_for(AgentPlatform::Claude)
        );
        // Re-save should write per-platform block, not flat legacy keys.
        loaded.save_to_path(&path).unwrap();
        let yaml = fs::read_to_string(&path).unwrap();
        assert!(yaml.contains("agent_launch:"));
        assert!(!yaml.contains("agent_model:"));
        assert!(!yaml.contains("agent_effort:"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn keeps_independent_platform_launch_settings() {
        let mut settings = TodSettings::default();
        settings.agent_platform = AgentPlatform::Claude;
        settings.set_agent_model("opus");
        settings.set_agent_effort("high");
        settings.agent_platform = AgentPlatform::Cursor;
        settings.set_agent_model("composer-2.5");
        settings.set_agent_effort("medium");
        assert_eq!(settings.agent_launch.claude.model, "opus");
        assert_eq!(settings.agent_launch.claude.effort, "high");
        assert_eq!(settings.agent_launch.cursor.model, "composer-2.5");
        assert_eq!(settings.agent_launch.cursor.effort, "medium");
        settings.agent_platform = AgentPlatform::Claude;
        assert_eq!(settings.agent_model(), "opus");
        assert_eq!(settings.agent_effort(), "high");
    }

    #[test]
    fn agent_roles_are_independent() {
        let mut settings = TodSettings::default();
        settings.set_platform_for(AgentRole::Chat, AgentPlatform::Cursor);
        settings.set_model_for(AgentRole::Chat, "composer-2.5");
        settings.set_effort_for(AgentRole::Default, "high");
        settings.set_model_for(AgentRole::Interview, "opus");

        let chat = settings.launch_options_for(AgentRole::Chat);
        assert_eq!(chat.platform, AgentPlatform::Cursor);
        assert_eq!(chat.model, "composer-2.5");
        assert_eq!(chat.effort, "auto");

        let default = settings.launch_options_for(AgentRole::Default);
        assert_eq!(default.platform, AgentPlatform::Claude);
        assert_eq!(default.model, "default");
        assert_eq!(default.effort, "high");

        assert_eq!(settings.agent_platform, AgentPlatform::Claude);
        assert_eq!(settings.agent_model(), "opus");
        assert_eq!(settings.agent_effort(), "auto");
    }

    #[test]
    fn rejects_out_of_range_max_size_on_load() {
        let dir = std::env::temp_dir().join(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        fs::write(&path, "log_max_size_kb: 0\n").unwrap();
        assert!(TodSettings::load_from_path(&path).is_err());
        fs::write(&path, "log_max_size_kb: 104857601\n").unwrap();
        assert!(TodSettings::load_from_path(&path).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_out_of_range_max_size_on_save() {
        let dir = std::env::temp_dir().join(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        let settings = TodSettings {
            log_max_size_kb: 0,
            ..TodSettings::default()
        };
        assert!(settings.save_to_path(&path).is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
