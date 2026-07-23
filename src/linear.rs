//! Linear GraphQL API client (issue lookup by identifier).

use color_eyre::eyre::{Context, eyre};
use serde::Deserialize;
use serde_json::json;

const LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";

/// Issue fields needed when creating a task from a Linear identifier.
#[derive(Debug, Clone)]
pub struct LinearIssue {
    pub identifier: String,
    pub title: String,
}

#[derive(Debug, Deserialize)]
struct GraphqlResponse {
    data: Option<GraphqlData>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphqlData {
    issue: Option<IssueNode>,
    issues: Option<IssueConnection>,
}

#[derive(Debug, Deserialize)]
struct IssueConnection {
    nodes: Vec<IssueNode>,
}

#[derive(Debug, Deserialize)]
struct IssueNode {
    title: String,
    identifier: String,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

/// Look up a Linear issue by human identifier (`TEAM-123`).
///
/// Tries `issue(id:)` first (Linear accepts identifiers), then falls back to
/// filtering by team key + number.
pub fn fetch_issue_by_identifier(
    api_key: &str,
    identifier: &str,
) -> color_eyre::Result<LinearIssue> {
    let (team_key, number) = parse_identifier(identifier)?;

    match fetch_via_issue_id(api_key, identifier) {
        Ok(issue) => Ok(issue),
        Err(err) => {
            // Fall through to filter; keep the first error if filter also fails.
            match fetch_via_filter(api_key, &team_key, number) {
                Ok(issue) => Ok(issue),
                Err(filter_err) => Err(eyre!(
                    "Linear lookup for {identifier} failed ({err:#}); filter fallback also failed: {filter_err:#}"
                )),
            }
        }
    }
}

fn parse_identifier(identifier: &str) -> color_eyre::Result<(String, i64)> {
    let Some((team, num)) = identifier.rsplit_once('-') else {
        return Err(eyre!("invalid Linear identifier: {identifier}"));
    };
    let number: i64 = num
        .parse()
        .wrap_err_with(|| format!("invalid issue number in {identifier}"))?;
    Ok((team.to_string(), number))
}

fn fetch_via_issue_id(api_key: &str, identifier: &str) -> color_eyre::Result<LinearIssue> {
    let query = r#"
        query Issue($id: String!) {
            issue(id: $id) {
                title
                identifier
            }
        }
    "#;
    let body = json!({
        "query": query,
        "variables": { "id": identifier },
    });
    let response = graphql_post(api_key, &body)?;
    if let Some(errors) = response.errors {
        let msg = errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(eyre!("GraphQL errors: {msg}"));
    }
    let issue = response
        .data
        .and_then(|d| d.issue)
        .ok_or_else(|| eyre!("issue not found"))?;
    Ok(LinearIssue {
        identifier: issue.identifier,
        title: issue.title,
    })
}

fn fetch_via_filter(api_key: &str, team_key: &str, number: i64) -> color_eyre::Result<LinearIssue> {
    let query = r#"
        query IssueByTeamNumber($teamKey: String!, $number: Float!) {
            issues(
                filter: {
                    number: { eq: $number }
                    team: { key: { eq: $teamKey } }
                }
                first: 1
            ) {
                nodes {
                    title
                    identifier
                }
            }
        }
    "#;
    let body = json!({
        "query": query,
        "variables": {
            "teamKey": team_key,
            "number": number as f64,
        },
    });
    let response = graphql_post(api_key, &body)?;
    if let Some(errors) = response.errors {
        let msg = errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(eyre!("GraphQL errors: {msg}"));
    }
    let nodes = response
        .data
        .and_then(|d| d.issues)
        .map(|c| c.nodes)
        .unwrap_or_default();
    let issue = nodes
        .into_iter()
        .next()
        .ok_or_else(|| eyre!("no issue matched team {team_key} number {number}"))?;
    Ok(LinearIssue {
        identifier: issue.identifier,
        title: issue.title,
    })
}

fn graphql_post(api_key: &str, body: &serde_json::Value) -> color_eyre::Result<GraphqlResponse> {
    let response = ureq::post(LINEAR_GRAPHQL_URL)
        .header("Authorization", api_key)
        .header("Content-Type", "application/json")
        .send_json(body)
        .wrap_err("calling Linear GraphQL API")?;

    let status = response.status();
    if !status.is_success() {
        let text = response
            .into_body()
            .read_to_string()
            .unwrap_or_else(|_| String::new());
        return Err(eyre!("Linear HTTP {status}: {text}"));
    }

    response
        .into_body()
        .read_json::<GraphqlResponse>()
        .wrap_err("decoding Linear GraphQL response")
}
