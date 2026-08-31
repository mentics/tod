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

/// Linear integration — API key can also be supplied via `LINEAR_API_KEY`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LinearSettings {
    #[serde(default)]
    pub api_key: Option<String>,
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
    /// Which agent platform runs interview question-maker / answer-processor work.
    #[serde(default = "default_agent_platform")]
    pub agent_platform: AgentPlatform,
    /// Terminal emulator for interactive shell sessions.
    #[serde(default = "default_terminal_settings")]
    pub terminal: TerminalSettings,
    /// Last known main-window placement.
    #[serde(default)]
    pub window_geometry: Option<WindowGeometry>,
    #[serde(default)]
    pub linear: LinearSettings,
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
            agent_platform: AgentPlatform::default(),
            terminal: TerminalSettings::default(),
            window_geometry: None,
            linear: LinearSettings::default(),
        }
    }
}

impl TodSettings {
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
        let settings: Self = serde_yaml::from_str(&contents)
            .with_context(|| format!("failed to parse settings YAML from {}", path.display()))?;
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
            agent_platform: AgentPlatform::Claude,
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
            linear: LinearSettings::default(),
        };
        settings.save_to_path(&path).unwrap();
        let loaded = TodSettings::load_from_path(&path).unwrap();
        assert_eq!(loaded, settings);
        let _ = fs::remove_dir_all(dir);
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
