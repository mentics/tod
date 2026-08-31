//! Cross-platform install bootstrap (`install.toml` under the OS app config dir).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const INSTALL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallConfig {
    pub data_root: PathBuf,
    #[serde(default = "default_version")]
    pub version: u32,
}

fn default_version() -> u32 {
    INSTALL_VERSION
}

/// OS-standard app config directory (`…/tod/`).
pub fn app_config_dir() -> Result<PathBuf> {
    dirs::config_dir()
        .map(|dir| dir.join("tod"))
        .context("failed to resolve OS config directory")
}

/// Suggested default data root on first launch (same directory as `install.toml`).
pub fn default_data_root() -> Result<PathBuf> {
    app_config_dir()
}

pub fn install_config_path() -> Result<PathBuf> {
    Ok(app_config_dir()?.join("install.toml"))
}

pub fn load_data_root() -> Option<PathBuf> {
    load_install_config()
        .ok()
        .flatten()
        .map(|config| config.data_root)
}

pub fn load_install_config() -> Result<Option<InstallConfig>> {
    let path = install_config_path()?;
    if !path.is_file() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if contents.trim().is_empty() {
        return Ok(None);
    }
    let config: InstallConfig = toml::from_str(&contents)
        .with_context(|| format!("failed to parse install config {}", path.display()))?;
    Ok(Some(config))
}

pub fn save_data_root(data_root: &Path) -> Result<()> {
    let config_dir = app_config_dir()?;
    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("failed to create {}", config_dir.display()))?;
    let normalized = crate::fleet::paths::normalize_absolute(data_root)?;
    let config = InstallConfig {
        data_root: normalized,
        version: INSTALL_VERSION,
    };
    let contents = toml::to_string_pretty(&config).context("failed to serialize install config")?;
    let path = install_config_path()?;
    std::fs::write(&path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn round_trip_install_config() {
        let _lock = env_lock();
        let base = std::env::temp_dir().join(format!("tod-install-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let prev = std::env::var("XDG_CONFIG_HOME").ok();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &base);
        }

        let data_root = base.join("tod-data");
        save_data_root(&data_root).unwrap();
        let loaded = load_install_config().unwrap().expect("install config");
        assert_eq!(
            loaded.data_root,
            data_root.canonicalize().unwrap_or(data_root)
        );
        assert_eq!(loaded.version, INSTALL_VERSION);

        unsafe {
            if let Some(v) = prev {
                std::env::set_var("XDG_CONFIG_HOME", v);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
        }
        let _ = std::fs::remove_dir_all(base);
    }
}
