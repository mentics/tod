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

const STATES_QUERY: &str = "query Issue($id: String!) { issue(id: $id) { id state { id name type } team { states { nodes { id name type position } } } } }";
const SET_STATE_MUTATION: &str = "mutation SetState($id: String!, $stateId: String!) { issueUpdate(id: $id, input: { stateId: $stateId }) { success } }";

/// A workflow state of an issue's team.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
    /// `triage`, `backlog`, `unstarted`, `started`, `completed`, `canceled`.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub position: f64,
}

/// Where a ticket should be at least: a state of `kind`, preferring one whose
/// name contains a `name_hints` entry (case-insensitive), else the team's
/// first of that kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateGoal {
    pub kind: &'static str,
    pub name_hints: &'static [&'static str],
}

/// How far along a workflow-state type is; `None` for `canceled`, which is
/// never moved out of or into.
fn kind_rank(kind: &str) -> Option<u8> {
    match kind {
        "triage" | "backlog" => Some(0),
        "unstarted" => Some(1),
        "started" => Some(2),
        "completed" => Some(3),
        _ => None,
    }
}

fn hint_matches(state: &WorkflowState, goal: &StateGoal) -> bool {
    let name = state.name.to_ascii_lowercase();
    goal.name_hints.iter().any(|hint| name.contains(hint))
}

/// The state to move an issue in `current` to so it is at least at `goal`;
/// `None` when it already is (never moves backward), is canceled, or the
/// team has no state to go to.
pub fn choose_state<'a>(
    current: &WorkflowState,
    team: &'a [WorkflowState],
    goal: &StateGoal,
) -> Option<&'a WorkflowState> {
    let have = kind_rank(&current.kind)?;
    let want = kind_rank(goal.kind)?;
    if have > want || (have == want && (goal.name_hints.is_empty() || hint_matches(current, goal))) {
        return None;
    }
    let mut candidates: Vec<&WorkflowState> = team.iter().filter(|s| s.kind == goal.kind).collect();
    candidates.sort_by(|a, b| a.position.total_cmp(&b.position));
    let hinted = candidates.iter().copied().find(|s| hint_matches(s, goal));
    if have == want {
        // Already that kind: only on to a hinted state further along.
        return hinted.filter(|s| s.position > current.position);
    }
    hinted.or_else(|| candidates.first().copied())
}

/// Moves the issue to `goal` if it is not there or beyond. `Ok(Some(name))`
/// is the state it was moved to.
pub fn ensure_state_at_least(
    api_key: &str,
    identifier: &str,
    goal: &StateGoal,
) -> Result<Option<String>, LinearError> {
    let data = post_graphql(api_key, STATES_QUERY, serde_json::json!({ "id": identifier }))?;
    let issue = data
        .get("issue")
        .filter(|issue| !issue.is_null())
        .ok_or_else(|| LinearError::NotFound(identifier.to_string()))?;
    let parse = |value: Option<&serde_json::Value>| {
        value
            .cloned()
            .and_then(|v| serde_json::from_value::<WorkflowState>(v).ok())
    };
    let issue_id = issue.get("id").and_then(|v| v.as_str()).unwrap_or(identifier);
    let current = parse(issue.get("state"))
        .ok_or_else(|| LinearError::Api("issue has no workflow state".into()))?;
    let team: Vec<WorkflowState> = issue
        .pointer("/team/states/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| nodes.iter().filter_map(|n| parse(Some(n))).collect())
        .unwrap_or_default();
    let Some(target) = choose_state(&current, &team, goal) else {
        return Ok(None);
    };
    post_graphql(
        api_key,
        SET_STATE_MUTATION,
        serde_json::json!({ "id": issue_id, "stateId": target.id }),
    )?;
    Ok(Some(target.name.clone()))
}

/// One GraphQL round trip; the response's `data`.
fn post_graphql(
    api_key: &str,
    query: &str,
    variables: serde_json::Value,
) -> Result<serde_json::Value, LinearError> {
    let mut response = ureq::post(LINEAR_GRAPHQL_URL)
        .header("Authorization", api_key)
        .header("Content-Type", "application/json")
        .send_json(serde_json::json!({ "query": query, "variables": variables }))
        .map_err(|err| LinearError::Http(err.to_string()))?;
    let status = response.status();
    let payload: serde_json::Value = response
        .body_mut()
        .read_json()
        .map_err(|err| LinearError::Http(format!("invalid JSON (HTTP {status}): {err}")))?;
    if let Some(errors) = payload.get("errors").and_then(|e| e.as_array()).filter(|e| !e.is_empty()) {
        let message = errors
            .iter()
            .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
            .collect::<Vec<_>>()
            .join("; ");
        if status == 401 || message.to_ascii_lowercase().contains("authentication") {
            return Err(LinearError::Api("Invalid Linear API key".into()));
        }
        return Err(LinearError::Api(message));
    }
    Ok(payload.get("data").cloned().unwrap_or(serde_json::Value::Null))
}

#[cfg(test)]
mod state_tests {
    use super::*;

    fn st(name: &str, kind: &str, position: f64) -> WorkflowState {
        WorkflowState { id: name.into(), name: name.into(), kind: kind.into(), position }
    }

    fn team() -> Vec<WorkflowState> {
        vec![
            st("Backlog", "backlog", 0.0),
            st("Todo", "unstarted", 1.0),
            st("Up Next", "unstarted", 0.5),
            st("In Progress", "started", 2.0),
            st("In Review", "started", 3.0),
            st("Done", "completed", 4.0),
            st("Canceled", "canceled", 5.0),
        ]
    }

    const UP_NEXT: StateGoal = StateGoal { kind: "unstarted", name_hints: &["up next"] };
    const STARTED: StateGoal = StateGoal { kind: "started", name_hints: &[] };
    const REVIEW: StateGoal = StateGoal { kind: "started", name_hints: &["review"] };
    const DONE: StateGoal = StateGoal { kind: "completed", name_hints: &[] };

    fn pick(current: &str, goal: &StateGoal) -> Option<String> {
        let team = team();
        let current = team.iter().find(|s| s.name == current).unwrap();
        choose_state(current, &team, goal).map(|s| s.name.clone())
    }

    #[test]
    fn moves_forward_to_the_hinted_or_first_state() {
        assert_eq!(pick("Backlog", &UP_NEXT).as_deref(), Some("Up Next"));
        assert_eq!(pick("Todo", &STARTED).as_deref(), Some("In Progress"));
        assert_eq!(pick("In Progress", &REVIEW).as_deref(), Some("In Review"));
        assert_eq!(pick("In Review", &DONE).as_deref(), Some("Done"));
    }

    #[test]
    fn never_moves_backward_or_sideways() {
        assert_eq!(pick("In Progress", &UP_NEXT), None);
        assert_eq!(pick("In Review", &STARTED), None);
        assert_eq!(pick("In Review", &REVIEW), None);
        assert_eq!(pick("Done", &REVIEW), None);
        assert_eq!(pick("Up Next", &UP_NEXT), None);
        assert_eq!(pick("Todo", &UP_NEXT), None);
    }

    #[test]
    fn canceled_is_left_alone() {
        assert_eq!(pick("Canceled", &STARTED), None);
    }
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
