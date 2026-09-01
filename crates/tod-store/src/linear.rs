//! Linear GraphQL client for issue lookup by identifier (e.g. `TOD-142`).

use serde::Deserialize;
use thiserror::Error;

const LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";
const ISSUE_QUERY: &str =
    "query Issue($id: String!) { issue(id: $id) { identifier title description } }";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearIssue {
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, Error)]
pub enum LinearError {
    #[error("Linear API key not configured")]
    MissingApiKey,
    #[error("HTTP request failed: {0}")]
    Http(String),
    #[error("issue {0} not found")]
    NotFound(String),
    #[error("Linear API error: {0}")]
    Api(String),
}

pub fn fetch_issue(api_key: &str, identifier: &str) -> Result<LinearIssue, LinearError> {
    let identifier = identifier.trim();
    if identifier.is_empty() {
        return Err(LinearError::NotFound(String::new()));
    }

    let body = serde_json::json!({
        "query": ISSUE_QUERY,
        "variables": { "id": identifier },
    });

    let mut response = ureq::post(LINEAR_GRAPHQL_URL)
        .header("Authorization", api_key)
        .header("Content-Type", "application/json")
        .send_json(body)
        .map_err(|err| LinearError::Http(err.to_string()))?;

    let status = response.status();
    let payload: GraphQlResponse = response
        .body_mut()
        .read_json()
        .map_err(|err| LinearError::Http(format!("invalid JSON (HTTP {status}): {err}")))?;

    if let Some(errors) = payload.errors.filter(|errors| !errors.is_empty()) {
        let message = errors
            .into_iter()
            .map(|error| error.message)
            .collect::<Vec<_>>()
            .join("; ");
        if status == 401 || message.to_ascii_lowercase().contains("authentication") {
            return Err(LinearError::Api("Invalid Linear API key".into()));
        }
        return Err(LinearError::Api(message));
    }

    let Some(issue) = payload.data.and_then(|data| data.issue) else {
        return Err(LinearError::NotFound(identifier.to_string()));
    };

    Ok(LinearIssue {
        identifier: issue.identifier,
        title: issue.title,
        description: issue.description.filter(|d| !d.trim().is_empty()),
    })
}

#[derive(Debug, Deserialize)]
struct GraphQlResponse {
    data: Option<GraphQlData>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQlData {
    issue: Option<LinearIssueRaw>,
}

#[derive(Debug, Deserialize)]
struct LinearIssueRaw {
    identifier: String,
    title: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
}
