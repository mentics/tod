//! The [`DataSource`] trait abstracts fetching items by filter criteria,
//! so that generator nodes are not coupled to any single data source.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// An item returned by a data source, forming a tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSourceItem {
    /// Unique identifier in the external system (e.g. "TOD-142").
    pub external_id: String,
    /// Display title.
    pub title: String,
    /// Tags / labels from the external system.
    pub tags: Vec<String>,
    /// Body text (description, details). May contain markdown.
    pub body: String,
    /// Optional metadata (priority, state, assignee, etc.) as JSON value.
    /// Data sources store display-ready strings here; the generator framework
    /// treats this as opaque and passes it through to managed nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Child items, forming a tree structure (e.g. sub-issues).
    pub children: Vec<DataSourceItem>,
}

/// A credential the data source needs to operate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialRequirement {
    /// Machine-readable key (e.g. "linear_api_key").
    pub key: String,
    /// Human-readable label shown in the credential prompt (e.g. "Linear API key").
    pub label: String,
}

/// Field type for configuration schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConfigFieldType {
    /// Single-line text input.
    Text,
    /// Multi-line text input.
    TextArea,
    /// Boolean toggle.
    Boolean,
    /// Select from a fixed set of options.
    Select { options: Vec<String> },
    /// Data-source-specific custom field type with rendering hints.
    /// The UI checks the data source type and introspection metadata
    /// to render these appropriately.
    Custom {
        /// Type hint for the UI (e.g., "linear_filter_field").
        type_hint: String,
        /// Additional metadata as JSON for UI-specific rendering.
        metadata: serde_json::Value,
    },
}

/// One field in a data source's configuration schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigField {
    /// Machine-readable field name (used as key in the config JSON object).
    pub name: String,
    /// Human-readable label for the form.
    pub label: String,
    /// Help text shown below the field.
    pub help: String,
    /// Field type.
    pub field_type: ConfigFieldType,
    /// Whether the field is required.
    pub required: bool,
}

/// Schema describing the configuration fields for a data source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigSchema {
    pub fields: Vec<ConfigField>,
}

/// Errors from data source operations.
#[derive(Debug, thiserror::Error)]
pub enum DataSourceError {
    /// Configuration is invalid.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// Authentication failed or credentials missing.
    #[error("authentication error: {0}")]
    Auth(String),

    /// Network or transport error.
    #[error("fetch error: {0}")]
    Fetch(String),

    /// The external service returned an error.
    #[error("service error: {0}")]
    Service(String),

    /// Any other error.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Trait abstracting an external data source for generator nodes.
///
/// Each implementation (Linear, GitHub Issues, etc.) provides its own
/// configuration schema, validation, and fetch logic. The generator framework
/// treats the configuration as opaque JSON; only the data source interprets it.
pub trait DataSource: Send + Sync {
    /// Human-readable name (e.g. "Linear Issues").
    fn display_name(&self) -> &str;

    /// Short description of what this data source provides.
    fn description(&self) -> &str;

    /// Credentials this data source requires to fetch data.
    fn credential_requirements(&self) -> Vec<CredentialRequirement>;

    /// Schema describing the configuration fields for this data source.
    /// The generator framework uses this to render the configuration form.
    fn configuration_schema(&self) -> ConfigSchema;

    /// Validate a configuration before it is persisted.
    /// Returns `Ok(())` if valid, or `Err(DataSourceError::InvalidConfig(..))`.
    fn validate_config(&self, config: &serde_json::Value) -> Result<(), DataSourceError>;

    /// Fetch items matching the configuration query.
    ///
    /// The implementation handles pagination internally and returns the complete
    /// result tree. `credentials` maps credential keys (from [`credential_requirements`])
    /// to their resolved values.
    ///
    /// This is a blocking call. The caller is responsible for running it off
    /// the main thread if needed.
    fn fetch(
        &self,
        config: &serde_json::Value,
        credentials: &HashMap<String, String>,
    ) -> Result<Vec<DataSourceItem>, DataSourceError>;

    /// Get introspection metadata for this data source, if available.
    /// Returns None for data sources that don't support introspection.
    fn introspection_metadata(&self) -> Option<serde_json::Value> {
        None
    }

    /// Check if introspection cache exists.
    fn has_introspection_cache(&self) -> bool {
        false
    }

    /// Force refresh of introspection metadata (e.g., schema from API).
    /// Returns an error if introspection is not supported or refresh fails.
    fn refresh_introspection(&self, _api_key: &str) -> Result<(), DataSourceError> {
        Err(DataSourceError::InvalidConfig(
            "introspection not supported".into(),
        ))
    }
}
