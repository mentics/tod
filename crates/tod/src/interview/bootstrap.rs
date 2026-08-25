use crate::interview::{SessionStore, TodPaths, TodSettings};
use anyhow::Result;

/// Initialize interview persistence (config dir, default settings, SQLite schema).
pub fn bootstrap() -> Result<()> {
    let paths = TodPaths::discover()?;
    paths.ensure_config_dir()?;
    let settings = TodSettings::load(&paths)?;
    if !paths.settings_path().exists() {
        settings.save(&paths)?;
    }
    let _store = SessionStore::open(&paths)?;
    Ok(())
}
