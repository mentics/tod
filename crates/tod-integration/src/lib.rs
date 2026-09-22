//! External data-source integrations for tod.
//!
//! This crate defines the [`DataSource`] trait and data-source implementations.
//! Transport and API code for external services lives here, not in `tod-agent`
//! (which is exclusively for agent transport).

mod data_source;
pub mod linear;
pub mod linear_query;
pub mod mock;
pub mod preset;

pub use data_source::{
    ConfigField, ConfigFieldType, ConfigSchema, CredentialRequirement, DataSource,
    DataSourceError, DataSourceItem,
};
pub use linear::{
    is_filter_key as is_linear_filter_key, is_relation_filter_field, issue_url as linear_issue_url,
    migrate_legacy_filter_keys, relation_filter, relation_filter_selection, FilterFieldMetadata,
    IntrospectionCache, LinearDataSource,
};
pub use mock::MockDataSource;
pub use preset::{delete_preset, load_presets, rename_preset, save_preset, FilterPreset};
