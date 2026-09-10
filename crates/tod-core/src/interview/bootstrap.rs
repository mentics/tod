use crate::interview::{SessionStore, TodPaths, TodSettings};
use anyhow::Result;
use std::sync::Arc;
use tod_store::fleet::FleetStore;

/// Initialize interview persistence (config dir, default settings).
pub fn bootstrap(fleet: Arc<FleetStore>) -> Result<()> {
    let paths = TodPaths::discover()?;
    paths.ensure_config_dir()?;
    let settings = TodSettings::load(&paths)?;
    if !paths.settings_path().exists() {
        settings.save(&paths)?;
    }
    if let Err(err) = settings.sync_treehouse_config(&paths) {
        tracing::warn!("failed to sync treehouse config: {err:#}");
    }
    let _store = SessionStore::open(fleet);
    Ok(())
}
