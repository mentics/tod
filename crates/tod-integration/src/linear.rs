//! Linear [`DataSource`] adapter: fetches issues (and their sub-issues) for a
//! team, optionally filtered by a title search term, as a tree of
//! [`DataSourceItem`]s.

use crate::{
    ConfigField, ConfigFieldType, ConfigSchema, CredentialRequirement, DataSource,
    DataSourceError, DataSourceItem,
};
use serde::Deserialize;
use std::collections::HashMap;

const LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";
const LINEAR_API_KEY: &str = "linear_api_key";
const PAGE_SIZE: u32 = 50;

const ISSUES_QUERY: &str = "
query Issues($teamKey: String!, $query: String, $after: String, $first: Int!) {
  issues(
    first: $first
    after: $after
    filter: { team: { key: { eq: $teamKey } }, title: { containsIgnoreCase: $query } }
  ) {
    nodes {
      identifier
      title
      description
      labels { nodes { name } }
      parent { identifier }
    }
    pageInfo { hasNextPage endCursor }
  }
}";

/// Fetches Linear issues for a team via the DataSource trait.
pub struct LinearDataSource;

impl LinearDataSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LinearDataSource {
    fn default() -> Self {
        Self::new()
    }
}

impl DataSource for LinearDataSource {
    fn display_name(&self) -> &str {
        "Linear Issues"
    }

    fn description(&self) -> &str {
        "Issues from a Linear team, with sub-issues mapped as children."
    }

    fn credential_requirements(&self) -> Vec<CredentialRequirement> {
        vec![CredentialRequirement {
            key: LINEAR_API_KEY.into(),
            label: "Linear API key".into(),
        }]
    }

    fn configuration_schema(&self) -> ConfigSchema {
        ConfigSchema {
            fields: vec![
                ConfigField {
                    name: "team_key".into(),
                    label: "Team key".into(),
                    help: "The Linear team key, e.g. \"TOD\".".into(),
                    field_type: ConfigFieldType::Text,
                    required: true,
                },
                ConfigField {
                    name: "query".into(),
                    label: "Title contains".into(),
                    help: "Only fetch issues whose title contains this text. Leave blank for all issues in the team.".into(),
                    field_type: ConfigFieldType::Text,
                    required: false,
                },
            ],
        }
    }

    fn validate_config(&self, config: &serde_json::Value) -> Result<(), DataSourceError> {
        team_key(config)?;
        Ok(())
    }

    fn fetch(
        &self,
        config: &serde_json::Value,
        credentials: &HashMap<String, String>,
    ) -> Result<Vec<DataSourceItem>, DataSourceError> {
        let team_key = team_key(config)?;
        let query = config
            .as_object()
            .and_then(|obj| obj.get("query"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty());

        let api_key = credentials.get(LINEAR_API_KEY).ok_or_else(|| {
            DataSourceError::Auth("Linear API key not configured".into())
        })?;

        let flat = fetch_all_issues(api_key, &team_key, query)?;
        Ok(build_tree(flat))
    }
}

fn team_key(config: &serde_json::Value) -> Result<String, DataSourceError> {
    config
        .as_object()
        .and_then(|obj| obj.get("team_key"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| DataSourceError::InvalidConfig("team_key is required".into()))
}

struct FlatIssue {
    identifier: String,
    title: String,
    description: Option<String>,
    labels: Vec<String>,
    parent_identifier: Option<String>,
}

fn fetch_all_issues(
    api_key: &str,
    team_key: &str,
    query: Option<&str>,
) -> Result<Vec<FlatIssue>, DataSourceError> {
    let mut all = Vec::new();
    let mut after: Option<String> = None;

    loop {
        let body = serde_json::json!({
            "query": ISSUES_QUERY,
            "variables": {
                "teamKey": team_key,
                "query": query,
                "after": after,
                "first": PAGE_SIZE,
            },
        });

        let mut response = ureq::post(LINEAR_GRAPHQL_URL)
            .header("Authorization", api_key)
            .header("Content-Type", "application/json")
            .send_json(body)
            .map_err(|err| DataSourceError::Fetch(err.to_string()))?;

        let status = response.status();
        let payload: GraphQlResponse = response
            .body_mut()
            .read_json()
            .map_err(|err| DataSourceError::Fetch(format!("invalid JSON (HTTP {status}): {err}")))?;

        if let Some(errors) = payload.errors.filter(|errors| !errors.is_empty()) {
            let message = errors
                .into_iter()
                .map(|error| error.message)
                .collect::<Vec<_>>()
                .join("; ");
            if status == 401 || message.to_ascii_lowercase().contains("authentication") {
                return Err(DataSourceError::Auth("Invalid Linear API key".into()));
            }
            return Err(DataSourceError::Service(message));
        }

        let Some(data) = payload.data else {
            return Err(DataSourceError::Service("empty response".into()));
        };

        let has_next = data.issues.page_info.has_next_page;
        let cursor = data.issues.page_info.end_cursor;

        for node in data.issues.nodes {
            all.push(FlatIssue {
                identifier: node.identifier,
                title: node.title,
                description: node.description.filter(|d| !d.trim().is_empty()),
                labels: node.labels.nodes.into_iter().map(|l| l.name).collect(),
                parent_identifier: node.parent.map(|p| p.identifier),
            });
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

/// Build a tree of [`DataSourceItem`]s from a flat list of issues linked by
/// parent identifier. Issues whose parent is outside the fetched set (or has
/// none) become roots.
fn build_tree(flat: Vec<FlatIssue>) -> Vec<DataSourceItem> {
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
    ) -> DataSourceItem {
        let issue = &by_id[id];
        let children = children_by_parent
            .get(id)
            .into_iter()
            .flatten()
            .map(|child_id| to_item(child_id, by_id, children_by_parent))
            .collect();
        DataSourceItem {
            external_id: issue.identifier.clone(),
            title: issue.title.clone(),
            tags: issue.labels.clone(),
            body: issue.description.clone().unwrap_or_default(),
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
        .map(|id| to_item(id, &by_id, &children_by_parent))
        .collect()
}

#[derive(Debug, Deserialize)]
struct GraphQlResponse {
    data: Option<GraphQlData>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQlData {
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
struct GraphQlError {
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_requires_team_key() {
        let ds = LinearDataSource::new();
        assert!(ds.validate_config(&serde_json::json!({})).is_err());
        assert!(ds
            .validate_config(&serde_json::json!({"team_key": "TOD"}))
            .is_ok());
    }

    #[test]
    fn fetch_requires_credentials() {
        let ds = LinearDataSource::new();
        let result = ds.fetch(&serde_json::json!({"team_key": "TOD"}), &HashMap::new());
        assert!(matches!(result, Err(DataSourceError::Auth(_))));
    }

    #[test]
    fn build_tree_nests_sub_issues() {
        let flat = vec![
            FlatIssue {
                identifier: "TOD-1".into(),
                title: "Parent".into(),
                description: None,
                labels: vec![],
                parent_identifier: None,
            },
            FlatIssue {
                identifier: "TOD-2".into(),
                title: "Child".into(),
                description: None,
                labels: vec![],
                parent_identifier: Some("TOD-1".into()),
            },
        ];
        let tree = build_tree(flat);
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
            labels: vec![],
            parent_identifier: Some("TOD-1".into()),
        }];
        let tree = build_tree(flat);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].external_id, "TOD-2");
    }
}
