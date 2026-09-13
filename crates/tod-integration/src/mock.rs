//! Mock [`DataSource`] for testing.

use crate::{
    ConfigField, ConfigFieldType, ConfigSchema, CredentialRequirement, DataSource,
    DataSourceError, DataSourceItem,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A configurable mock data source for store-layer and UI smoke tests.
///
/// Set `items`, `error`, and `credential_requirements` before use.
#[derive(Clone)]
pub struct MockDataSource {
    inner: Arc<Mutex<MockState>>,
}

struct MockState {
    items: Vec<DataSourceItem>,
    error: Option<DataSourceError>,
    credentials: Vec<CredentialRequirement>,
    fetch_count: usize,
}

impl MockDataSource {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockState {
                items: Vec::new(),
                error: None,
                credentials: Vec::new(),
                fetch_count: 0,
            })),
        }
    }

    /// Set the items that will be returned by [`DataSource::fetch`].
    pub fn with_items(self, items: Vec<DataSourceItem>) -> Self {
        self.inner.lock().unwrap().items = items;
        self
    }

    /// Set an error that will be returned by [`DataSource::fetch`].
    pub fn with_error(self, error: DataSourceError) -> Self {
        self.inner.lock().unwrap().error = Some(error);
        self
    }

    /// Set the credential requirements.
    pub fn with_credentials(self, creds: Vec<CredentialRequirement>) -> Self {
        self.inner.lock().unwrap().credentials = creds;
        self
    }

    /// How many times `fetch` has been called.
    pub fn fetch_count(&self) -> usize {
        self.inner.lock().unwrap().fetch_count
    }

    /// Replace the items returned by fetch (for simulating data changes between refreshes).
    pub fn set_items(&self, items: Vec<DataSourceItem>) {
        self.inner.lock().unwrap().items = items;
    }

    /// Clear any configured error.
    pub fn clear_error(&self) {
        self.inner.lock().unwrap().error = None;
    }
}

impl Default for MockDataSource {
    fn default() -> Self {
        Self::new()
    }
}

impl DataSource for MockDataSource {
    fn display_name(&self) -> &str {
        "Mock"
    }

    fn description(&self) -> &str {
        "A mock data source for testing"
    }

    fn credential_requirements(&self) -> Vec<CredentialRequirement> {
        self.inner.lock().unwrap().credentials.clone()
    }

    fn configuration_schema(&self) -> ConfigSchema {
        ConfigSchema {
            fields: vec![ConfigField {
                name: "query".into(),
                label: "Query".into(),
                help: "Mock query string".into(),
                field_type: ConfigFieldType::Text,
                required: false,
            }],
        }
    }

    fn validate_config(&self, config: &serde_json::Value) -> Result<(), DataSourceError> {
        if let Some(obj) = config.as_object() {
            if obj
                .get("invalid")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return Err(DataSourceError::InvalidConfig(
                    "mock validation failure".into(),
                ));
            }
        }
        Ok(())
    }

    fn fetch(
        &self,
        _config: &serde_json::Value,
        _credentials: &HashMap<String, String>,
    ) -> Result<Vec<DataSourceItem>, DataSourceError> {
        let mut state = self.inner.lock().unwrap();
        state.fetch_count += 1;
        if let Some(ref err) = state.error {
            return Err(DataSourceError::Fetch(err.to_string()));
        }
        Ok(state.items.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_returns_configured_items() {
        let ds = MockDataSource::new().with_items(vec![DataSourceItem {
            external_id: "TEST-1".into(),
            title: "Test item".into(),
            tags: vec!["tag1".into()],
            body: "body".into(),
            children: vec![],
        }]);
        let result = ds
            .fetch(&serde_json::json!({}), &HashMap::new())
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].external_id, "TEST-1");
        assert_eq!(ds.fetch_count(), 1);
    }

    #[test]
    fn mock_returns_configured_error() {
        let ds =
            MockDataSource::new().with_error(DataSourceError::Fetch("test error".into()));
        let result = ds.fetch(&serde_json::json!({}), &HashMap::new());
        assert!(result.is_err());
    }

    #[test]
    fn mock_validates_config() {
        let ds = MockDataSource::new();
        assert!(ds.validate_config(&serde_json::json!({})).is_ok());
        assert!(ds
            .validate_config(&serde_json::json!({"invalid": true}))
            .is_err());
    }

    #[test]
    fn mock_items_can_be_updated_between_fetches() {
        let ds = MockDataSource::new().with_items(vec![DataSourceItem {
            external_id: "A-1".into(),
            title: "First".into(),
            tags: vec![],
            body: "".into(),
            children: vec![],
        }]);
        let r1 = ds.fetch(&serde_json::json!({}), &HashMap::new()).unwrap();
        assert_eq!(r1.len(), 1);

        ds.set_items(vec![
            DataSourceItem {
                external_id: "A-1".into(),
                title: "First (updated)".into(),
                tags: vec![],
                body: "".into(),
                children: vec![],
            },
            DataSourceItem {
                external_id: "A-2".into(),
                title: "Second".into(),
                tags: vec![],
                body: "".into(),
                children: vec![],
            },
        ]);
        let r2 = ds.fetch(&serde_json::json!({}), &HashMap::new()).unwrap();
        assert_eq!(r2.len(), 2);
        assert_eq!(r2[0].title, "First (updated)");
        assert_eq!(ds.fetch_count(), 2);
    }
}
