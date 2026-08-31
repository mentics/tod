use crate::fleet::FleetStore;
use crate::interview::{SessionStore, TodPaths, TodSettings};
use anyhow::Result;
use std::sync::Arc;

/// Initialize interview persistence (config dir, default settings).
pub fn bootstrap(fleet: Arc<FleetStore>) -> Result<()> {
    let paths = TodPaths::discover()?;
    paths.ensure_config_dir()?;
    let settings = TodSettings::load(&paths)?;
    if !paths.settings_path().exists() {
        settings.save(&paths)?;
    }
    let _store = SessionStore::open(fleet);
    Ok(())
}
