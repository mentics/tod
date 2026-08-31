pub use tod_store::paths::{
    TodPaths, is_data_root_configured, resolve_startup_data_root,
    set_data_root,
};

#[cfg(test)]
pub use tod_store::paths::clear_data_root_override;
