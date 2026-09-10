pub use tod_store::paths::{
    TodPaths, is_data_root_configured, resolve_startup_data_root, set_data_root,
};

/// Test-support helper: drop the process-wide data-root override.
///
/// Unconditionally re-exported (it is already public in `tod-store`) so tests
/// in dependent crates can reach it.
pub use tod_store::paths::clear_data_root_override;
