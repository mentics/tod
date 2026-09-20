//! External data-source integrations for tod.
//!
//! This crate defines the [`DataSource`] trait and data-source implementations.
//! Transport and API code for external services lives here, not in `tod-agent`
//! (which is exclusively for agent transport).

mod data_source;
pub mod linear;
pub mod mock;
pub mod preset;

pub use data_source::{
    ConfigField, ConfigFieldType, ConfigSchema, CredentialRequirement, DataSource,
    DataSourceError, DataSourceItem,
};
pub use linear::{FilterFieldMetadata, IntrospectionCache, LinearDataSource};
pub use mock::MockDataSource;
pub use preset::{delete_preset, load_presets, rename_preset, save_preset, FilterPreset};
