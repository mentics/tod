//! Linear [`DataSource`] adapter with dynamic filter discovery via introspection.
//!
//! Fetches Linear issues matching user-configured filter criteria as a tree of
//! [`DataSourceItem`]s. Filter fields are discovered dynamically via GraphQL
//! introspection and cached for reuse.

use crate::{
    ConfigField, ConfigFieldType, ConfigSchema, CredentialRequirement, DataSource,
    DataSourceError, DataSourceItem,
};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

const LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";
const LINEAR_API_KEY: &str = "linear_api_key";
const PAGE_SIZE: i32 = 50;
const DEFAULT_RESULT_CAP: usize = 200;
#[allow(dead_code)] // Used in retry logic
const MAX_RETRIES: u32 = 3;
const INITIAL_RETRY_DELAY_SECS: u64 = 5;

/// Fetches Linear issues via the DataSource trait with dynamic filter discovery.
pub struct LinearDataSource {
    /// Optional data root for the introspection cache.
    /// If None, introspection is disabled and only basic config is supported.
    data_root: Option<PathBuf>,
}

impl LinearDataSource {
    pub fn new() -> Self {
        Self { data_root: None }
    }

    /// Create a LinearDataSource with a data root for introspection caching.
    pub fn with_data_root(data_root: PathBuf) -> Self {
        Self {
            data_root: Some(data_root),
        }
    }

    /// Get or fetch introspection metadata from cache.
    #[allow(dead_code)] // Will be used by UI configuration form
    fn get_introspection(&self, api_key: &str) -> Result<IntrospectionCache, DataSourceError> {
        let Some(ref data_root) = self.data_root else {
            return Err(DataSourceError::InvalidConfig(
                "introspection requires data root".into(),
            ));
        };

        let cache_path = data_root.join("linear_introspection_cache.json");

        // Try to load from cache first
        if let Ok(contents) = std::fs::read_to_string(&cache_path) {
            if let Ok(cache) = serde_json::from_str::<IntrospectionCache>(&contents) {
                return Ok(cache);
            }
        }

        // Cache miss or invalid - fetch from Linear API
        let cache = fetch_introspection(api_key)?;

        // Persist cache
        if let Ok(json) = serde_json::to_string_pretty(&cache) {
            let _ = std::fs::write(&cache_path, json);
        }

        Ok(cache)
    }

    /// Get cached introspection metadata path.
    fn cache_path(&self) -> Option<PathBuf> {
        self.data_root
            .as_ref()
            .map(|root| root.join("linear_introspection_cache.json"))
    }

    /// Get introspection cache if it exists.
    pub fn get_cached_introspection(&self) -> Option<IntrospectionCache> {
        let cache_path = self.cache_path()?;
        let contents = std::fs::read_to_string(&cache_path).ok()?;
        serde_json::from_str(&contents).ok()
    }

}

impl Default for LinearDataSource {
    fn default() -> Self {
        Self::new()
    }
}

impl DataSource for LinearDataSource {
    fn display_name(&self) -> &str {
        "Linear Tickets"
    }

    fn description(&self) -> &str {
        "Query Linear tickets by filter criteria. Supports dynamic filter discovery via introspection."
    }

    fn credential_requirements(&self) -> Vec<CredentialRequirement> {
        vec![CredentialRequirement {
            key: LINEAR_API_KEY.into(),
            label: "Linear API key".into(),
        }]
    }

    fn configuration_schema(&self) -> ConfigSchema {
        // Basic schema with result_cap and a placeholder for Linear filter fields.
        // The UI will use introspection_metadata() to render the actual filter form.
        ConfigSchema {
            fields: vec![
                ConfigField {
                    name: "_linear_filters".into(),
                    label: "Filter criteria".into(),
                    help: "Configure Linear issue filters (requires introspection)".into(),
                    field_type: ConfigFieldType::Custom {
                        type_hint: "linear_filter_fields".into(),
                        metadata: serde_json::json!({}),
                    },
                    required: false,
                },
                ConfigField {
                    name: "result_cap".into(),
                    label: "Result cap".into(),
                    help: format!(
                        "Maximum total items to fetch (default {})",
                        DEFAULT_RESULT_CAP
                    ),
                    field_type: ConfigFieldType::Text,
                    required: false,
                },
            ],
        }
    }

    fn validate_config(&self, config: &serde_json::Value) -> Result<(), DataSourceError> {
        // Result cap validation
        if let Some(obj) = config.as_object() {
            if let Some(cap_value) = obj.get("result_cap") {
                if let Some(cap_num) = cap_value.as_u64() {
                    if cap_num == 0 {
                        return Err(DataSourceError::InvalidConfig(
                            "result_cap must be greater than 0".into(),
                        ));
                    }
                } else if !cap_value.is_null() {
                    return Err(DataSourceError::InvalidConfig(
                        "result_cap must be a number".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_config_with_test_query(
        &self,
        config: &serde_json::Value,
        credentials: &HashMap<String, String>,
    ) -> Result<(), DataSourceError> {
        // First, do basic validation
        self.validate_config(config)?;

        // Get API key from credentials
        let api_key = credentials
            .get(LINEAR_API_KEY)
            .ok_or_else(|| DataSourceError::Auth("Linear API key not configured".into()))?;

        // Build filter from config
        let filter = build_filter_from_config(config)?;

        // Send a test query with first=1 to validate filter syntax
        let client = build_client()?;
        let query = r#"
            query TestFilter($filter: IssueFilter, $first: Int!) {
                issues(first: $first, filter: $filter) {
                    nodes {
                        id
                    }
                }
            }
        "#;

        let body = serde_json::json!({
            "query": query,
            "variables": {
                "filter": filter,
                "first": 1,
            },
        });

        let response = client
            .post(LINEAR_GRAPHQL_URL)
            .headers(build_headers(api_key)?)
            .json(&body)
            .timeout(Duration::from_secs(30))
            .send()
            .map_err(|e| DataSourceError::Fetch(format!("Test query failed: {}", e)))?;

        let status = response.status();

        if !status.is_success() && status != StatusCode::OK {
            return Err(DataSourceError::Fetch(format!(
                "HTTP error {}: test query failed",
                status
            )));
        }

        let payload: GraphQlResponse<serde_json::Value> = response.json().map_err(|e| {
            DataSourceError::Fetch(format!("Invalid JSON response (HTTP {}): {}", status, e))
        })?;

        handle_graphql_errors(&payload, status)?;

        Ok(())
    }

    fn fetch(
        &self,
        config: &serde_json::Value,
        credentials: &HashMap<String, String>,
    ) -> Result<Vec<DataSourceItem>, DataSourceError> {
        let api_key = credentials.get(LINEAR_API_KEY).ok_or_else(|| {
            DataSourceError::Auth("Linear API key not configured".into())
        })?;

        let result_cap = config
            .as_object()
            .and_then(|obj| obj.get("result_cap"))
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_RESULT_CAP as u64) as usize;

        // Extract filter from config
        let filter = build_filter_from_config(config)?;

        // Fetch workspace slug (needed for browser URLs)
        let workspace_slug = fetch_workspace_slug(api_key)?;

        // Fetch all issues with pagination
        let flat = fetch_all_issues(api_key, &filter, result_cap)?;

        // Build tree and add workspace_slug to metadata
        let items = build_tree(flat, &workspace_slug);

        Ok(items)
    }

    fn introspection_metadata(&self) -> Option<serde_json::Value> {
        self.get_cached_introspection()
            .and_then(|cache| serde_json::to_value(&cache).ok())
    }

    fn has_introspection_cache(&self) -> bool {
        self.cache_path()
            .map(|p| p.exists())
            .unwrap_or(false)
    }

    fn refresh_introspection(&self, api_key: &str) -> Result<(), DataSourceError> {
        let Some(ref data_root) = self.data_root else {
            return Err(DataSourceError::InvalidConfig(
                "introspection requires data root".into(),
            ));
        };

        let cache = fetch_introspection(api_key)?;
        let cache_path = data_root.join("linear_introspection_cache.json");
        let json = serde_json::to_string_pretty(&cache)
            .map_err(|e| DataSourceError::Other(e.into()))?;
        std::fs::write(&cache_path, json).map_err(|e| DataSourceError::Other(e.into()))?;

        Ok(())
    }
}

/// Build a Linear GraphQL filter object from config JSON.
fn build_filter_from_config(
    config: &serde_json::Value,
) -> Result<serde_json::Value, DataSourceError> {
    let Some(obj) = config.as_object() else {
        return Ok(serde_json::json!({}));
    };

    let mut filter = serde_json::Map::new();

    // Extract filter fields (everything except result_cap and special keys)
    for (key, value) in obj {
        if key == "result_cap" || key == "workspace_slug" || value.is_null() {
            continue;
        }

        // For now, pass through the filter structure as-is
        // The UI is responsible for building the correct GraphQL filter structure
        filter.insert(key.clone(), value.clone());
    }

    Ok(serde_json::Value::Object(filter))
}

/// Fetch workspace URL slug from Linear API.
fn fetch_workspace_slug(api_key: &str) -> Result<String, DataSourceError> {
    let client = build_client()?;
    let query = r#"
        query {
            viewer {
                organization {
                    urlKey
                }
            }
        }
    "#;

    let body = serde_json::json!({
        "query": query,
    });

    let response = client
        .post(LINEAR_GRAPHQL_URL)
        .headers(build_headers(api_key)?)
        .json(&body)
        .send()
        .map_err(|e| DataSourceError::Fetch(format!("Failed to fetch workspace slug: {}", e)))?;

    let status = response.status();

    if !status.is_success() && status != StatusCode::OK {
        return Err(DataSourceError::Fetch(format!(
            "HTTP error {}: failed to fetch workspace slug",
            status
        )));
    }

    let payload: GraphQlResponse<WorkspaceData> = response.json().map_err(|e| {
        DataSourceError::Fetch(format!("Invalid JSON response (HTTP {}): {}", status, e))
    })?;

    handle_graphql_errors(&payload, status)?;

    let data = payload
        .data
        .ok_or_else(|| DataSourceError::Service("empty response".into()))?;

    Ok(data.viewer.organization.url_key)
}

/// Introspection cache stored as JSON in the data root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntrospectionCache {
    pub workspace_slug: String,
    pub filter_fields: Vec<FilterFieldMetadata>,
    pub enums: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterFieldMetadata {
    pub name: String,
    pub description: Option<String>,
    pub field_type: String,
    pub is_nullable: bool,
}

/// Fetch introspection metadata from Linear API.
fn fetch_introspection(api_key: &str) -> Result<IntrospectionCache, DataSourceError> {
    let workspace_slug = fetch_workspace_slug(api_key)?;

    let client = build_client()?;
    let query = r#"
        query {
            __type(name: "IssueFilter") {
                inputFields {
                    name
                    description
                    type {
                        name
                        kind
                        ofType {
                            name
                            kind
                        }
                    }
                }
            }
        }
    "#;

    let body = serde_json::json!({
        "query": query,
    });

    let response = client
        .post(LINEAR_GRAPHQL_URL)
        .headers(build_headers(api_key)?)
        .json(&body)
        .send()
        .map_err(|e| DataSourceError::Fetch(format!("Failed to fetch introspection: {}", e)))?;

    let status = response.status();

    if !status.is_success() && status != StatusCode::OK {
        return Err(DataSourceError::Fetch(format!(
            "HTTP error {}: failed to fetch introspection",
            status
        )));
    }

    let payload: GraphQlResponse<IntrospectionData> = response.json().map_err(|e| {
        DataSourceError::Fetch(format!("Invalid JSON response (HTTP {}): {}", status, e))
    })?;

    handle_graphql_errors(&payload, status)?;

    let data = payload
        .data
        .ok_or_else(|| DataSourceError::Service("empty introspection response".into()))?;

    let type_info = data
        .__type
        .ok_or_else(|| DataSourceError::Service("IssueFilter type not found".into()))?;

    let filter_fields = type_info
        .input_fields
        .into_iter()
        .map(|field| FilterFieldMetadata {
            name: field.name,
            description: field.description,
            field_type: field.type_info.name.unwrap_or_else(|| {
                field
                    .type_info
                    .of_type
                    .as_ref()
                    .and_then(|t| t.name.clone())
                    .unwrap_or_else(|| "String".into())
            }),
            is_nullable: field.type_info.kind != "NON_NULL",
        })
        .collect();

    // TODO: Fetch enum types for dropdowns
    let enums = HashMap::new();

    Ok(IntrospectionCache {
        workspace_slug,
        filter_fields,
        enums,
    })
}

#[derive(Debug, Deserialize)]
struct WorkspaceData {
    viewer: Viewer,
}

#[derive(Debug, Deserialize)]
struct Viewer {
    organization: Organization,
}

#[derive(Debug, Deserialize)]
struct Organization {
    #[serde(rename = "urlKey")]
    url_key: String,
}

#[derive(Debug, Deserialize)]
struct IntrospectionData {
    __type: Option<TypeInfo>,
}

#[derive(Debug, Deserialize)]
struct TypeInfo {
    #[serde(rename = "inputFields")]
    input_fields: Vec<InputField>,
}

#[derive(Debug, Deserialize)]
struct InputField {
    name: String,
    description: Option<String>,
    #[serde(rename = "type")]
    type_info: TypeRef,
}

#[derive(Debug, Deserialize)]
struct TypeRef {
    name: Option<String>,
    kind: String,
    #[serde(rename = "ofType")]
    of_type: Option<Box<TypeRef>>,
}

struct FlatIssue {
    identifier: String,
    title: String,
    description: Option<String>,
    priority: Option<i32>,
    state: Option<String>,
    assignee: Option<String>,
    labels: Vec<String>,
    parent_identifier: Option<String>,
}

/// Fetch all issues matching the filter, with pagination and result cap.
fn fetch_all_issues(
    api_key: &str,
    filter: &serde_json::Value,
    result_cap: usize,
) -> Result<Vec<FlatIssue>, DataSourceError> {
    let client = build_client()?;
    let mut all = Vec::new();
    let mut after: Option<String> = None;

    loop {
        let query = r#"
            query Issues($filter: IssueFilter, $after: String, $first: Int!) {
                issues(first: $first, after: $after, filter: $filter) {
                    nodes {
                        identifier
                        title
                        description
                        priority
                        state { name }
                        assignee { name }
                        labels { nodes { name } }
                        parent { identifier }
                    }
                    pageInfo {
                        hasNextPage
                        endCursor
                    }
                }
            }
        "#;

        let body = serde_json::json!({
            "query": query,
            "variables": {
                "filter": filter,
                "after": after,
                "first": PAGE_SIZE,
            },
        });

        // Retry loop for rate limiting
        let mut attempt = 0;
        let mut retry_delay_secs = INITIAL_RETRY_DELAY_SECS;
        let response = loop {
            attempt += 1;
            let response = client
                .post(LINEAR_GRAPHQL_URL)
                .headers(build_headers(api_key)?)
                .json(&body)
                .timeout(Duration::from_secs(30))
                .send()
                .map_err(|e| DataSourceError::Fetch(format!("Request failed: {}", e)))?;

            let status = response.status();

            if status == StatusCode::TOO_MANY_REQUESTS {
                if attempt >= MAX_RETRIES {
                    return Err(DataSourceError::Service(format!(
                        "Rate limited after {} attempts",
                        MAX_RETRIES
                    )));
                }

                // Extract Retry-After header if present
                let wait_secs = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(retry_delay_secs);

                std::thread::sleep(Duration::from_secs(wait_secs));
                retry_delay_secs *= 2; // Exponential backoff
                continue;
            }

            break response;
        };

        let status = response.status();

        if !status.is_success() && status != StatusCode::OK {
            return Err(DataSourceError::Fetch(format!(
                "HTTP error {}: failed to fetch issues",
                status
            )));
        }

        let payload: GraphQlResponse<IssuesData> = response.json().map_err(|e| {
            DataSourceError::Fetch(format!("Invalid JSON response (HTTP {}): {}", status, e))
        })?;

        handle_graphql_errors(&payload, status)?;

        let data = payload
            .data
            .ok_or_else(|| DataSourceError::Service("empty response".into()))?;

        let has_next = data.issues.page_info.has_next_page;
        let cursor = data.issues.page_info.end_cursor;

        for node in data.issues.nodes {
            all.push(FlatIssue {
                identifier: node.identifier,
                title: node.title,
                description: node.description.filter(|d| !d.trim().is_empty()),
                priority: node.priority,
                state: node.state.map(|s| s.name),
                assignee: node.assignee.map(|a| a.name),
                labels: node.labels.nodes.into_iter().map(|l| l.name).collect(),
                parent_identifier: node.parent.map(|p| p.identifier),
            });

            // Check result cap
            if all.len() >= result_cap {
                return Ok(all);
            }
        }

        if !has_next {
            break;
        }

        after = cursor;
        if after.is_none() {
            break;
        }
    }

    Ok(all)
}

/// Build a tree of [`DataSourceItem`]s from flat issues.
fn build_tree(flat: Vec<FlatIssue>, workspace_slug: &str) -> Vec<DataSourceItem> {
    let mut children_by_parent: HashMap<String, Vec<String>> = HashMap::new();
    let mut by_id: HashMap<String, FlatIssue> = HashMap::new();

    for issue in flat {
        if let Some(parent) = &issue.parent_identifier {
            children_by_parent
                .entry(parent.clone())
                .or_default()
                .push(issue.identifier.clone());
        }
        by_id.insert(issue.identifier.clone(), issue);
    }

    fn to_item(
        id: &str,
        by_id: &HashMap<String, FlatIssue>,
        children_by_parent: &HashMap<String, Vec<String>>,
        workspace_slug: &str,
    ) -> DataSourceItem {
        let issue = &by_id[id];
        let children = children_by_parent
            .get(id)
            .into_iter()
            .flatten()
            .map(|child_id| to_item(child_id, by_id, children_by_parent, workspace_slug))
            .collect();

        // Build metadata JSON with priority, state, assignee, workspace_slug
        let mut meta = serde_json::Map::new();

        let priority_name = issue.priority.map(|pri| match pri {
            0 => "No priority",
            1 => "Urgent",
            2 => "High",
            3 => "Medium",
            4 => "Low",
            _ => "Unknown",
        });

        meta.insert(
            "priority".into(),
            priority_name
                .map(|s| serde_json::Value::String(s.into()))
                .unwrap_or(serde_json::Value::Null),
        );

        meta.insert(
            "state".into(),
            issue
                .state
                .as_ref()
                .map(|s| serde_json::Value::String(s.clone()))
                .unwrap_or(serde_json::Value::Null),
        );

        meta.insert(
            "assignee".into(),
            issue
                .assignee
                .as_ref()
                .map(|a| serde_json::Value::String(a.clone()))
                .unwrap_or(serde_json::Value::Null),
        );

        meta.insert(
            "workspace_slug".into(),
            serde_json::Value::String(workspace_slug.into()),
        );

        let metadata = Some(serde_json::Value::Object(meta));

        // Title includes identifier prefix (e.g., "TOD-142: Fix bug")
        let prefixed_title = format!("{}: {}", issue.identifier, issue.title);

        DataSourceItem {
            external_id: issue.identifier.clone(),
            title: prefixed_title,
            tags: issue.labels.clone(),
            body: issue.description.clone().unwrap_or_default(),
            metadata,
            children,
        }
    }

    let root_ids: Vec<String> = by_id
        .values()
        .filter(|issue| {
            issue
                .parent_identifier
                .as_ref()
                .is_none_or(|parent| !by_id.contains_key(parent))
        })
        .map(|issue| issue.identifier.clone())
        .collect();

    root_ids
        .iter()
        .map(|id| to_item(id, &by_id, &children_by_parent, workspace_slug))
        .collect()
}

fn build_client() -> Result<Client, DataSourceError> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| DataSourceError::Fetch(format!("Failed to build HTTP client: {}", e)))
}

fn build_headers(api_key: &str) -> Result<HeaderMap, DataSourceError> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let bearer = format!("Bearer {}", api_key);
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&bearer)
            .map_err(|e| DataSourceError::Auth(format!("Invalid API key format: {}", e)))?,
    );
    Ok(headers)
}

#[derive(Debug, Deserialize)]
struct GraphQlResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
}

fn handle_graphql_errors<T>(
    payload: &GraphQlResponse<T>,
    status: StatusCode,
) -> Result<(), DataSourceError> {
    if let Some(errors) = &payload.errors {
        if !errors.is_empty() {
            let message = errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");

            if status == StatusCode::UNAUTHORIZED
                || message.to_ascii_lowercase().contains("authentication")
            {
                return Err(DataSourceError::Auth("Invalid Linear API key".into()));
            }

            return Err(DataSourceError::Service(message));
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct IssuesData {
    issues: IssuesConnection,
}

#[derive(Debug, Deserialize)]
struct IssuesConnection {
    nodes: Vec<IssueNode>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct PageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IssueNode {
    identifier: String,
    title: String,
    description: Option<String>,
    priority: Option<i32>,
    state: Option<StateRef>,
    assignee: Option<AssigneeRef>,
    labels: LabelConnection,
    parent: Option<ParentRef>,
}

#[derive(Debug, Deserialize)]
struct LabelConnection {
    nodes: Vec<LabelNode>,
}

#[derive(Debug, Deserialize)]
struct LabelNode {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ParentRef {
    identifier: String,
}

#[derive(Debug, Deserialize)]
struct StateRef {
    name: String,
}

#[derive(Debug, Deserialize)]
struct AssigneeRef {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_is_linear_tickets() {
        let ds = LinearDataSource::new();
        assert_eq!(ds.display_name(), "Linear Tickets");
    }

    #[test]
    fn validate_config_accepts_empty() {
        let ds = LinearDataSource::new();
        assert!(ds.validate_config(&serde_json::json!({})).is_ok());
    }

    #[test]
    fn validate_config_accepts_valid_result_cap() {
        let ds = LinearDataSource::new();
        assert!(ds
            .validate_config(&serde_json::json!({"result_cap": 100}))
            .is_ok());
    }

    #[test]
    fn validate_config_rejects_zero_result_cap() {
        let ds = LinearDataSource::new();
        assert!(ds
            .validate_config(&serde_json::json!({"result_cap": 0}))
            .is_err());
    }

    #[test]
    fn build_tree_nests_sub_issues() {
        let flat = vec![
            FlatIssue {
                identifier: "TOD-1".into(),
                title: "Parent".into(),
                description: None,
                priority: None,
                state: None,
                assignee: None,
                labels: vec![],
                parent_identifier: None,
            },
            FlatIssue {
                identifier: "TOD-2".into(),
                title: "Child".into(),
                description: None,
                priority: None,
                state: None,
                assignee: None,
                labels: vec![],
                parent_identifier: Some("TOD-1".into()),
            },
        ];
        let tree = build_tree(flat, "test-workspace");
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].external_id, "TOD-1");
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].external_id, "TOD-2");
    }

    #[test]
    fn build_tree_orphaned_parent_becomes_root() {
        let flat = vec![FlatIssue {
            identifier: "TOD-2".into(),
            title: "Child".into(),
            description: None,
            priority: None,
            state: None,
            assignee: None,
            labels: vec![],
            parent_identifier: Some("TOD-1".into()),
        }];
        let tree = build_tree(flat, "test-workspace");
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].external_id, "TOD-2");
    }

    #[test]
    fn title_includes_identifier_prefix() {
        let flat = vec![FlatIssue {
            identifier: "TOD-142".into(),
            title: "Fix bug".into(),
            description: None,
            priority: None,
            state: None,
            assignee: None,
            labels: vec![],
            parent_identifier: None,
        }];
        let tree = build_tree(flat, "test-workspace");
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].title, "TOD-142: Fix bug");
    }

    #[test]
    fn metadata_includes_workspace_slug() {
        let flat = vec![FlatIssue {
            identifier: "TOD-1".into(),
            title: "Test".into(),
            description: None,
            priority: Some(2),
            state: Some("In Progress".into()),
            assignee: Some("Alice".into()),
            labels: vec![],
            parent_identifier: None,
        }];
        let tree = build_tree(flat, "my-workspace");
        assert_eq!(tree.len(), 1);
        let meta = tree[0].metadata.as_ref().unwrap();
        assert_eq!(
            meta.get("workspace_slug").and_then(|v| v.as_str()),
            Some("my-workspace")
        );
    }
}
