use crate::log_level::LogLevel;
use crate::paths::TodPaths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const DEFAULT_REPLENISH_THRESHOLD: u32 = 8;
const DEFAULT_SECOND_QUESTION_MAKER_THRESHOLD: u32 = 2;
const DEFAULT_QUESTION_MAKER_RUNS_PER_SESSION: u32 = 8;
const DEFAULT_SESSION_POOL_SIZE: u32 = 4;
const DEFAULT_ANSWERS_PER_SESSION: u32 = 16;
pub const DEFAULT_LOG_MAX_SIZE_KB: u64 = 51_200;
pub const MIN_LOG_MAX_SIZE_KB: u64 = 1;
pub const MAX_LOG_MAX_SIZE_KB: u64 = 104_857_600;

/// Interview agent platform — persisted in `tod.yml` and shown in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPlatform {
    Cursor,
    #[serde(alias = "anthropic")]
    Claude,
}

impl Default for AgentPlatform {
    fn default() -> Self {
        Self::Claude
    }
}

impl AgentPlatform {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cursor => "Cursor",
            Self::Claude => "Claude",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionMakerSettings {
    #[serde(default = "default_replenish_threshold")]
    pub replenish_threshold: u32,
    #[serde(default = "default_second_question_maker_threshold")]
    pub second_question_maker_threshold: u32,
    #[serde(default = "default_question_maker_runs_per_session")]
    pub runs_per_session: u32,
}

impl Default for QuestionMakerSettings {
    fn default() -> Self {
        Self {
            replenish_threshold: DEFAULT_REPLENISH_THRESHOLD,
            second_question_maker_threshold: DEFAULT_SECOND_QUESTION_MAKER_THRESHOLD,
            runs_per_session: DEFAULT_QUESTION_MAKER_RUNS_PER_SESSION,
        }
    }
}

fn default_replenish_threshold() -> u32 {
    DEFAULT_REPLENISH_THRESHOLD
}

fn default_second_question_maker_threshold() -> u32 {
    DEFAULT_SECOND_QUESTION_MAKER_THRESHOLD
}

fn default_question_maker_runs_per_session() -> u32 {
    DEFAULT_QUESTION_MAKER_RUNS_PER_SESSION
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerProcessorSettings {
    #[serde(default = "default_session_pool_size")]
    pub session_pool_size: u32,
    #[serde(default = "default_answers_per_session")]
    pub answers_per_session: u32,
}

impl Default for AnswerProcessorSettings {
    fn default() -> Self {
        Self {
            session_pool_size: DEFAULT_SESSION_POOL_SIZE,
            answers_per_session: DEFAULT_ANSWERS_PER_SESSION,
        }
    }
}

fn default_session_pool_size() -> u32 {
    DEFAULT_SESSION_POOL_SIZE
}

fn default_answers_per_session() -> u32 {
    DEFAULT_ANSWERS_PER_SESSION
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

/// External terminal program for agent shell sessions (`None` = OS default).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TerminalSettings {
    /// Executable name or path, e.g. `wt.exe`, `powershell.exe`, `/usr/bin/alacritty`.
    #[serde(default)]
    pub program: Option<String>,
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
    pub answer_processor: AnswerProcessorSettings,
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

impl Default for TodSettings {
    fn default() -> Self {
        Self {
            question_maker: QuestionMakerSettings::default(),
            answer_processor: AnswerProcessorSettings::default(),
            log_level: LogLevel::Info,
            log_max_size_kb: DEFAULT_LOG_MAX_SIZE_KB,
            fleet_storage_root: None,
            always_on_top: false,
            worktree_backend: WorktreeBackend::default(),
            treehouse_worktrees_root: None,
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
    /// Launch options for the currently selected interview platform.
    pub fn interview_launch_options(&self) -> crate::agent_launch::AgentLaunchOptions {
        let launch = self.agent_launch.get(self.agent_platform);
        crate::agent_launch::AgentLaunchOptions::from_settings(
            self.agent_platform,
            launch.model.clone(),
            launch.effort.clone(),
        )
    }

    /// Model for the active interview platform.
    pub fn agent_model(&self) -> &str {
        &self.agent_launch.get(self.agent_platform).model
    }

    /// Effort for the active interview platform.
    pub fn agent_effort(&self) -> &str {
        &self.agent_launch.get(self.agent_platform).effort
    }

    /// Update model for the active interview platform.
    pub fn set_agent_model(&mut self, model: impl Into<String>) {
        let platform = self.agent_platform;
        let model = crate::agent_launch::coerce_model(platform, &model.into());
        self.agent_launch.get_mut(platform).model = model;
    }

    /// Update effort for the active interview platform.
    pub fn set_agent_effort(&mut self, effort: impl Into<String>) {
        let platform = self.agent_platform;
        let effort = crate::agent_launch::coerce_effort(platform, &effort.into());
        self.agent_launch.get_mut(platform).effort = effort;
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
        assert_eq!(settings.question_maker.second_question_maker_threshold, 2);
        assert_eq!(settings.question_maker.runs_per_session, 8);
        assert_eq!(settings.answer_processor.session_pool_size, 4);
        assert_eq!(settings.answer_processor.answers_per_session, 16);
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
                second_question_maker_threshold: 3,
                runs_per_session: 8,
            },
            answer_processor: AnswerProcessorSettings::default(),
            log_level: LogLevel::Debug,
            log_max_size_kb: 1024,
            fleet_storage_root: None,
            always_on_top: true,
            worktree_backend: WorktreeBackend::default(),
            treehouse_worktrees_root: None,
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
