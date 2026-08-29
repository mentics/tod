use crate::interview::paths::TodPaths;
use crate::logging::LogLevel;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const DEFAULT_REPLENISH_THRESHOLD: u32 = 8;
const DEFAULT_SECOND_RESEARCHER_THRESHOLD: u32 = 2;
const DEFAULT_SESSION_POOL_SIZE: u32 = 4;
const DEFAULT_ANSWERS_PER_SESSION: u32 = 4;
pub const DEFAULT_LOG_MAX_SIZE_KB: u64 = 51_200;
pub const MIN_LOG_MAX_SIZE_KB: u64 = 1;
pub const MAX_LOG_MAX_SIZE_KB: u64 = 104_857_600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearcherSettings {
    #[serde(default = "default_replenish_threshold")]
    pub replenish_threshold: u32,
    #[serde(default = "default_second_researcher_threshold")]
    pub second_researcher_threshold: u32,
}

impl Default for ResearcherSettings {
    fn default() -> Self {
        Self {
            replenish_threshold: DEFAULT_REPLENISH_THRESHOLD,
            second_researcher_threshold: DEFAULT_SECOND_RESEARCHER_THRESHOLD,
        }
    }
}

fn default_replenish_threshold() -> u32 {
    DEFAULT_REPLENISH_THRESHOLD
}

fn default_second_researcher_threshold() -> u32 {
    DEFAULT_SECOND_RESEARCHER_THRESHOLD
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodSettings {
    #[serde(default)]
    pub researcher: ResearcherSettings,
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
}

impl Default for TodSettings {
    fn default() -> Self {
        Self {
            researcher: ResearcherSettings::default(),
            answer_processor: AnswerProcessorSettings::default(),
            log_level: LogLevel::Info,
            log_max_size_kb: DEFAULT_LOG_MAX_SIZE_KB,
            fleet_storage_root: None,
            always_on_top: false,
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
        Ok(())
    }

    pub fn clamp_log_max_size_kb(value: u64) -> u64 {
        value.clamp(MIN_LOG_MAX_SIZE_KB, MAX_LOG_MAX_SIZE_KB)
    }

    /// Resolved fleet storage root: explicit setting or `dirs::data_dir()/tod/fleet`.
    pub fn resolve_fleet_storage_root(&self) -> Result<PathBuf> {
        let root = match &self.fleet_storage_root {
            Some(path) => path.clone(),
            None => default_fleet_storage_root()?,
        };
        crate::fleet::paths::normalize_absolute(&root)
    }
}

fn default_fleet_storage_root() -> Result<PathBuf> {
    let data_dir =
        dirs::data_dir().context("failed to resolve OS data directory for fleet storage")?;
    Ok(data_dir.join("tod").join("fleet"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn defaults_when_missing() {
        let dir = std::env::temp_dir().join(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        let settings = TodSettings::load_from_path(&path).unwrap();
        assert_eq!(settings.researcher.replenish_threshold, 8);
        assert_eq!(settings.researcher.second_researcher_threshold, 2);
        assert_eq!(settings.answer_processor.session_pool_size, 4);
        assert_eq!(settings.answer_processor.answers_per_session, 4);
        assert_eq!(settings.log_level, LogLevel::Info);
        assert_eq!(settings.log_max_size_kb, DEFAULT_LOG_MAX_SIZE_KB);
        assert_eq!(settings.fleet_storage_root, None);
        assert!(!settings.always_on_top);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_fleet_storage_root_uses_default() {
        let settings = TodSettings::default();
        let root = settings.resolve_fleet_storage_root().unwrap();
        assert!(root.is_absolute());
        assert!(root.ends_with("fleet"));
    }

    #[test]
    fn resolve_fleet_storage_root_uses_setting() {
        let settings = TodSettings {
            fleet_storage_root: Some(PathBuf::from("/tmp/custom-fleet-root")),
            ..TodSettings::default()
        };
        let root = settings.resolve_fleet_storage_root().unwrap();
        assert!(root.is_absolute());
        assert!(root.ends_with("custom-fleet-root"));
    }

    #[test]
    fn round_trip_yaml() {
        let dir = std::env::temp_dir().join(format!("tod-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tod.yml");
        let settings = TodSettings {
            researcher: ResearcherSettings {
                replenish_threshold: 10,
                second_researcher_threshold: 3,
            },
            answer_processor: AnswerProcessorSettings::default(),
            log_level: LogLevel::Debug,
            log_max_size_kb: 1024,
            fleet_storage_root: None,
            always_on_top: true,
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
