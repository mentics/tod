//! Map a fetched Linear issue onto outline / fleet node fields.

use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::outline::types::Capability;
use tod_store::outline::{EXTRA_CONTENT_GOAL, OutlineMutation};
use uuid::Uuid;

/// Extract a ticket id (e.g. `TOD-142`) from a bare id or Linear issue URL.
pub fn parse_ticket_reference(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if is_bare_ticket_id(text) {
        return Some(text.to_string());
    }
    if text.contains("linear.app") || text.starts_with("http://") || text.starts_with("https://") {
        return ticket_from_url(text);
    }
    None
}

fn is_bare_ticket_id(text: &str) -> bool {
    is_ticket_id_segment(text) && !text.contains('/') && !text.contains(':')
}

fn is_ticket_id_segment(segment: &str) -> bool {
    let Some((prefix, suffix)) = segment.rsplit_once('-') else {
        return false;
    };
    segment.len() >= 3
        && !prefix.is_empty()
        && prefix
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '_')
        && !suffix.is_empty()
        && suffix.chars().all(|c| c.is_ascii_digit())
}

fn ticket_from_url(text: &str) -> Option<String> {
    let path = text.split(['?', '#']).next()?;
    for segment in path.split('/').rev() {
        if segment.is_empty() {
            continue;
        }
        if is_ticket_id_segment(segment) {
            return Some(segment.to_string());
        }
    }
    None
}

/// Apply ticket linkage and optional title / purpose from a Linear issue.
pub fn apply_linear_fields_to_node(
    fleet: &FleetStore,
    node_id: Uuid,
    ticket: &str,
    title: Option<&str>,
    description: Option<&str>,
    tags: Option<Vec<String>>,
    enable_work_capabilities: bool,
) -> Result<(), String> {
    if let Some(title) = title {
        fleet
            .enqueue_outline(OutlineMutation::UpdateNodeTitle {
                node_id,
                title: title.to_string(),
            })
            .map_err(|err| err.to_string())?;
    }
    fleet
        .enqueue(FleetMutation::UpdateTaskLinkedIssues {
            id: node_id.to_string(),
            linked_issues: vec![ticket.to_string()],
        })
        .map_err(|err| err.to_string())?;
    if let Some(tags) = tags {
        fleet
            .enqueue(FleetMutation::UpdateTaskTags {
                id: node_id.to_string(),
                tags,
            })
            .map_err(|err| err.to_string())?;
    }
    if enable_work_capabilities {
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id,
                capabilities: vec![Capability::Spec, Capability::Lifecycle],
            })
            .map_err(|err| err.to_string())?;
    }
    if let Some(body) = description.filter(|d| !d.trim().is_empty()) {
        fleet
            .enqueue_outline(OutlineMutation::SetExtraContent {
                node_id,
                content_type: EXTRA_CONTENT_GOAL.to_string(),
                body: body.to_string(),
            })
            .map_err(|err| err.to_string())?;
    }
    fleet.writer().flush().map_err(|err| err.to_string())?;
    Ok(())
}

pub fn tags_with_linear(existing: &[String]) -> Vec<String> {
    if existing.iter().any(|t| t.eq_ignore_ascii_case("linear")) {
        existing.to_vec()
    } else {
        let mut tags = existing.to_vec();
        tags.push("linear".into());
        tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_ticket_ids() {
        assert_eq!(parse_ticket_reference("TOD-142"), Some("TOD-142".into()));
        assert_eq!(parse_ticket_reference("  tod-99  "), Some("tod-99".into()));
        assert!(parse_ticket_reference("fix-login-bug").is_none());
        assert!(parse_ticket_reference("ab").is_none());
    }

    #[test]
    fn linear_urls() {
        assert_eq!(
            parse_ticket_reference("https://linear.app/mentics/issue/TOD-142/fix-login"),
            Some("TOD-142".into())
        );
        assert_eq!(
            parse_ticket_reference("https://linear.app/mentics/issue/TOD-142"),
            Some("TOD-142".into())
        );
        assert_eq!(
            parse_ticket_reference("linear.app/team/issue/ERR-500"),
            Some("ERR-500".into())
        );
        assert_eq!(
            parse_ticket_reference("https://linear.app/mentics/issue/TOD-142/slug?foo=bar#frag"),
            Some("TOD-142".into())
        );
    }
}
