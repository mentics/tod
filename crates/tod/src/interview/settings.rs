use crate::interview::agent::AgentPlatform;
use crate::interview::paths::TodPaths;
use crate::logging::LogLevel;
use anyhow::{Context, Result, bail};
use gpui::{Bounds, Pixels, Window, WindowBounds, point, px, size};
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

    fn to_bounds(&self) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(self.x), px(self.y)),
            size: size(px(self.width), px(self.height)),
        }
    }

    pub fn to_window_bounds(&self) -> WindowBounds {
        let bounds = self.to_bounds();
        if self.maximized {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        }
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
            agent_platform: AgentPlatform::default(),
            window_geometry: None,
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

    pub fn geometry_from_window(window: &Window) -> WindowGeometry {
        match window.window_bounds() {
            WindowBounds::Windowed(bounds) | WindowBounds::Maximized(bounds) => {
                let maximized = matches!(window.window_bounds(), WindowBounds::Maximized(_));
                WindowGeometry {
                    x: f32::from(bounds.origin.x),
                    y: f32::from(bounds.origin.y),
                    width: f32::from(bounds.size.width),
                    height: f32::from(bounds.size.height),
                    maximized,
                }
            }
            WindowBounds::Fullscreen(bounds) => WindowGeometry {
                x: f32::from(bounds.origin.x),
                y: f32::from(bounds.origin.y),
                width: f32::from(bounds.size.width),
                height: f32::from(bounds.size.height),
                maximized: false,
            },
        }
    }

    pub fn persist_window_geometry(window: &Window, paths: &TodPaths) {
        let geometry = Self::geometry_from_window(window);
        match Self::load(paths) {
            Ok(mut settings) => {
                settings.window_geometry = Some(geometry);
                if let Err(err) = settings.save(paths) {
                    tracing::error!("failed to save window geometry: {err:#}");
                }
            }
            Err(err) => {
                tracing::error!("failed to load settings for window geometry save: {err:#}");
            }
        }
    }

    pub fn resolve_open_window_bounds(
        &self,
        default_width: f32,
        default_height: f32,
        width_from_cli: bool,
        height_from_cli: bool,
    ) -> WindowBounds {
        let Some(mut geometry) = self.window_geometry.clone() else {
            return WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(default_width), px(default_height)),
            });
        };
        if width_from_cli {
            geometry.width = default_width;
        }
        if height_from_cli {
            geometry.height = default_height;
        }
        geometry.to_window_bounds()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::paths::{clear_data_root_override, set_data_root};
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
    fn resolve_open_window_bounds_uses_saved_geometry() {
        let settings = TodSettings {
            window_geometry: Some(WindowGeometry {
                x: 100.0,
                y: 200.0,
                width: 1600.0,
                height: 900.0,
                maximized: false,
            }),
            ..TodSettings::default()
        };
        let bounds = settings.resolve_open_window_bounds(1280.0, 768.0, false, false);
        let WindowBounds::Windowed(saved) = bounds else {
            panic!("expected windowed bounds");
        };
        assert_eq!(f32::from(saved.origin.x), 100.0);
        assert_eq!(f32::from(saved.origin.y), 200.0);
        assert_eq!(f32::from(saved.size.width), 1600.0);
        assert_eq!(f32::from(saved.size.height), 900.0);
    }

    #[test]
    fn resolve_open_window_bounds_honors_cli_size_overrides() {
        let settings = TodSettings {
            window_geometry: Some(WindowGeometry {
                x: 100.0,
                y: 200.0,
                width: 1600.0,
                height: 900.0,
                maximized: false,
            }),
            ..TodSettings::default()
        };
        let bounds = settings.resolve_open_window_bounds(1024.0, 600.0, true, true);
        let WindowBounds::Windowed(saved) = bounds else {
            panic!("expected windowed bounds");
        };
        assert_eq!(f32::from(saved.origin.x), 100.0);
        assert_eq!(f32::from(saved.origin.y), 200.0);
        assert_eq!(f32::from(saved.size.width), 1024.0);
        assert_eq!(f32::from(saved.size.height), 600.0);
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
