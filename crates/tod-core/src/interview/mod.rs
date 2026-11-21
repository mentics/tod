//! Shared clients and paths, plus support for legacy interview data.

pub mod bootstrap;
pub mod client;
pub mod context;
pub mod mock;
pub mod paths;

#[cfg(test)]
pub(crate) mod test_support;


pub use bootstrap::bootstrap;
pub use paths::{TodPaths, set_data_root};
pub use tod_store::settings::TodSettings;

/// The `tod-cli` executable installed next to the running binary.
pub fn tod_cli_path() -> std::path::PathBuf {
    tod_store::fleet::cli_relay::tod_cli_path()
}
