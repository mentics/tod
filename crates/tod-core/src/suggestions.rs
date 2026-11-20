//! Suggestions: filing one with its tracker duplicates found, and accepting
//! one (`doc/requests/design.md` section 9; the store is
//! [`tod_store::suggestions`]).
//!
//! Everything here goes to Linear, so it blocks: callers run it off the UI
//! thread (the CLI process, the daemon, or a background task of the app).
//!
//! **Filing** ([`file`], [`file_with_client`]) searches the tracker for
//! tickets that look like the suggestion before it is stored, mechanically
//! (`tod_store::linear::find_duplicates`: the title's terms, ranked by how
//! many a ticket shares). When Linear is not configured for the node (no
//! `linear_api_key` in its Environment) or the search fails, nothing is
//! stored as a duplicate and the suggestion says why
//! ([`tod_store::suggestions::Suggestion::duplicates_note`]); the filing
//! never fails for it.
//!
//! **Accepting** ([`accept`]) creates the ticket exactly as written, in the
//! team of the node's own ticket (or the nearest ancestor's), and records it
//! on the suggestion. A failure is returned for the user to see; nothing is
//! recorded as accepted that was not created.

use crate::interview::client::InterviewClient;
use crate::linear_import::parse_ticket_reference;
use std::path::Path;
use tod_store::credentials::CredentialStore;
use tod_store::environment::{self, Resolved};
use tod_store::fleet::FleetStore;
use tod_store::interview::InterviewCommand;
use tod_store::linear::{self, CreatedIssue, LinearError};
use tod_store::suggestions::{self, Duplicate, Filed, NewSuggestion};
use uuid::Uuid;

/// What the tracker search found for a suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateCheck {
    pub duplicates: Vec<Duplicate>,
    /// The search ran (an empty list then means no likely duplicates).
    pub checked: bool,
    /// Why it did not run or failed (empty when it ran).
    pub note: String,
}

/// Said when no Linear key is set for the node.
pub const NOT_CONFIGURED: &str =
    "duplicates were not checked: Linear is not configured for this task (no linear_api_key in its Environment)";

/// Searches the tracker with `key`, through `search`; `None` is no key.
pub fn check_duplicates_with(
    key: Option<&str>,
    title: &str,
    search: impl FnOnce(&str, &str) -> Result<Vec<Duplicate>, LinearError>,
) -> DuplicateCheck {
    let Some(key) = key else {
        return DuplicateCheck { duplicates: Vec::new(), checked: false, note: NOT_CONFIGURED.into() };
    };
    match search(key, title) {
        Ok(duplicates) => DuplicateCheck { duplicates, checked: true, note: String::new() },
        Err(err) => DuplicateCheck {
            duplicates: Vec::new(),
            checked: false,
            note: format!("duplicates were not checked: the Linear search failed: {err}"),
        },
    }
}

/// The tracker search for `title`, with the key of `resolved` (the node's
/// Environment). Reads the credential store: not on the UI thread.
pub fn check_duplicates(resolved: &[Resolved], data_root: &Path, title: &str) -> DuplicateCheck {
    let key = environment::linear_api_key(resolved, &CredentialStore::from_data_root(data_root));
    check_duplicates_with(key.as_deref(), title, linear::find_duplicates)
}

/// A suggestion to file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRequest {
    pub node: Uuid,
    pub title: String,
    /// The ticket's description and acceptance criteria.
    pub body: String,
    /// The conversation whose agent files it, if one does.
    pub conversation: Option<Uuid>,
    /// `agent`, `app` or `triage` (`tod_store::suggestions::FILERS`).
    pub filed_by: &'static str,
}

fn new_suggestion(request: &FileRequest, check: DuplicateCheck) -> NewSuggestion {
    let mut new = NewSuggestion::follow_up_ticket(request.node, &request.title, &request.body, request.filed_by);
    new.conversation_id = request.conversation;
    new.duplicates = check.duplicates;
    new.duplicates_checked = check.checked;
    new.duplicates_note = check.note;
    new
}

/// Files a follow-up ticket suggestion on `request.node` through the app's
/// store, with its duplicates searched first. Blocks (the tracker): never
/// call it on the UI thread. An identical open suggestion is returned rather
/// than filed again.
pub fn file(fleet: &FleetStore, request: &FileRequest) -> anyhow::Result<Filed> {
    let resolved = fleet.read(|conn| environment::resolve(conn, request.node))?;
    let check = check_duplicates(&resolved, fleet.paths().root(), &request.title);
    let reply = fleet
        .interview(request.filed_by, InterviewCommand::FileSuggestion(new_suggestion(request, check)))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    filed_from(&reply)
}

/// The same through a [`InterviewClient`] (`tod-cli`, which reaches the data
/// through the running app when there is one).
pub fn file_with_client(client: &InterviewClient, request: &FileRequest) -> anyhow::Result<(Filed, DuplicateCheck)> {
    let resolved = client.read(|conn| environment::resolve(conn, request.node))?;
    let check = check_duplicates(&resolved, client.data_root(), &request.title);
    let reply = client.interview(InterviewCommand::FileSuggestion(new_suggestion(request, check.clone())))?;
    Ok((filed_from(&reply)?, check))
}

fn filed_from(reply: &serde_json::Value) -> anyhow::Result<Filed> {
    let id = reply
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or_else(|| anyhow::anyhow!("the suggestion's id was not returned"))?;
    Ok(Filed { id, already_open: reply.get("already_open").and_then(|v| v.as_bool()).unwrap_or(false) })
}

/// The ticket a node's own work is on, else the nearest ancestor's: the
/// reference whose team a follow-up goes into.
pub fn team_ticket(fleet: &FleetStore, node: Uuid) -> anyhow::Result<Option<String>> {
    let chain = fleet.read(|conn| environment::ancestry(conn, node))?;
    for id in chain {
        if let Some(ticket) = fleet
            .get_node(&id.to_string())?
            .and_then(|task| task.ticket)
            .and_then(|t| parse_ticket_reference(&t))
        {
            return Ok(Some(ticket));
        }
    }
    Ok(None)
}

/// Accepts a suggestion: creates the ticket exactly as written, then records
/// it (and who accepted) on the suggestion. Blocks (Linear): off the UI thread.
pub fn accept(fleet: &FleetStore, id: Uuid, by: &str) -> anyhow::Result<CreatedIssue> {
    accept_with(fleet, id, by, linear::create_issue_in_team_of)
}

/// [`accept`] with the creation replaced (tests).
pub fn accept_with(
    fleet: &FleetStore,
    id: Uuid,
    by: &str,
    create: impl FnOnce(&str, &str, &str, &str) -> Result<CreatedIssue, LinearError>,
) -> anyhow::Result<CreatedIssue> {
    let suggestion = fleet
        .read(|conn| suggestions::get(conn, id))?
        .ok_or_else(|| anyhow::anyhow!("no suggestion {id}"))?;
    if !suggestion.is_open() {
        anyhow::bail!("this suggestion is already {}", suggestion.status);
    }
    let Some(sibling) = team_ticket(fleet, suggestion.node_id)? else {
        anyhow::bail!(
            "cannot create the ticket: neither this task nor any task above it has a Linear ticket to take the team from"
        );
    };
    let resolved = fleet.read(|conn| environment::resolve(conn, suggestion.node_id))?;
    let Some(key) = environment::linear_api_key(&resolved, &CredentialStore::from_data_root(fleet.paths().root())) else {
        anyhow::bail!("cannot create the ticket: no linear_api_key in this task's Environment");
    };
    let created = create(&key, &sibling, &suggestion.title, &suggestion.body)
        .map_err(|e| anyhow::anyhow!("Linear did not create the ticket: {e}"))?;
    fleet
        .interview(
            by,
            InterviewCommand::ResolveSuggestion {
                id,
                status: suggestions::STATUS_ACCEPTED.into(),
                result_id: created.identifier.clone(),
                result_url: created.url.clone(),
            },
        )
        .map_err(|e| {
            anyhow::anyhow!("the ticket {} was created in Linear but could not be recorded on the suggestion: {e}", created.identifier)
        })?;
    Ok(created)
}

/// Dismisses a suggestion: recorded as declined.
pub fn dismiss(fleet: &FleetStore, id: Uuid, by: &str) -> anyhow::Result<()> {
    fleet
        .interview(
            by,
            InterviewCommand::ResolveSuggestion {
                id,
                status: suggestions::STATUS_DISMISSED.into(),
                result_id: String::new(),
                result_url: String::new(),
            },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests;
