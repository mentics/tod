use crate::interview::paths::TodPaths;
use crate::logging::LogLevel;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

const DEFAULT_REPLENISH_THRESHOLD: u32 = 8;
const DEFAULT_SECOND_RESEARCHER_THRESHOLD: u32 = 2;
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
    #[serde(default = "default_log_level")]
    pub log_level: LogLevel,
    #[serde(default = "default_log_max_size_kb")]
    pub log_max_size_kb: u64,
}

impl Default for TodSettings {
    fn default() -> Self {
        Self {
            researcher: ResearcherSettings::default(),
            log_level: LogLevel::Info,
            log_max_size_kb: DEFAULT_LOG_MAX_SIZE_KB,
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
        assert_eq!(settings.log_level, LogLevel::Info);
        assert_eq!(settings.log_max_size_kb, DEFAULT_LOG_MAX_SIZE_KB);
        let _ = fs::remove_dir_all(dir);
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
            log_level: LogLevel::Debug,
            log_max_size_kb: 1024,
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
