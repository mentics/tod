use crate::interview::{TodPaths, TodSettings};
use anyhow::Result;

/// Initialize configuration (config dir, default settings).
pub fn bootstrap() -> Result<()> {
    let paths = TodPaths::discover()?;
    paths.ensure_config_dir()?;
    let settings = TodSettings::load(&paths)?;
    if !paths.settings_path().exists() {
        settings.save(&paths)?;
    }
    if let Err(err) = settings.sync_treehouse_config(&paths) {
        tracing::warn!("failed to sync treehouse config: {err:#}");
    }
    Ok(())
}
