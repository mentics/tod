use super::*;
use crate::interview::test_support::fixture;
use tod_store::outline::{Capability, OutlineMutation};

fn dup(id: &str, title: &str) -> Duplicate {
    Duplicate { id: id.into(), title: title.into(), url: format!("https://linear.app/x/issue/{id}") }
}

#[test]
fn no_key_means_no_search_and_says_so() {
    let check = check_duplicates_with(None, "Add retry", |_, _| panic!("must not search"));
    assert!(!check.checked);
    assert!(check.duplicates.is_empty());
    assert_eq!(check.note, NOT_CONFIGURED);
}

#[test]
fn a_failed_search_is_noted_not_fatal() {
    let check = check_duplicates_with(Some("k"), "Add retry", |_, _| Err(LinearError::Http("down".into())));
    assert!(!check.checked);
    assert!(check.note.contains("the Linear search failed"));
    assert!(check.note.contains("down"));
}

#[test]
fn a_search_that_finds_nothing_is_checked() {
    let check = check_duplicates_with(Some("k"), "Add retry", |_, _| Ok(Vec::new()));
    assert!(check.checked && check.duplicates.is_empty() && check.note.is_empty());
}

fn request(node: Uuid) -> FileRequest {
    FileRequest {
        node,
        title: "Add retry to webhooks".into(),
        body: "Retry failed deliveries.\n\nAcceptance: three attempts.".into(),
        conversation: None,
        filed_by: suggestions::FILED_BY_APP,
    }
}

#[test]
fn filing_without_linear_stores_the_reason_and_is_idempotent() {
    let fx = fixture();
    let first = file(&fx.fleet, &request(fx.node)).unwrap();
    assert!(!first.already_open);
    let again = file(&fx.fleet, &request(fx.node)).unwrap();
    assert!(again.already_open);
    assert_eq!(again.id, first.id);
    let rows = fx.fleet.read(|conn| suggestions::list_for_node(conn, fx.node)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].duplicates_checked_at, None);
    assert_eq!(rows[0].duplicates_note, NOT_CONFIGURED);
    assert_eq!(rows[0].filed_by, suggestions::FILED_BY_APP);
}

fn give_ticket(fx: &crate::interview::test_support::Fixture, ticket: &str) {
    fx.fleet
        .enqueue_outline(OutlineMutation::EnableCapabilities { node_id: fx.node, capabilities: vec![Capability::Ticket] })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    fx.fleet
        .enqueue_outline(OutlineMutation::SetNodeTicket {
            node_id: fx.node,
            ticket: Some(ticket.into()),
            linked_prs: Vec::new(),
        })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
}

#[test]
fn accepting_creates_the_ticket_as_written_and_links_it() {
    let fx = fixture();
    give_ticket(&fx, "ENG-100");
    tod_store::environment::store_linear_api_key(&fx.fleet, &fx.root, fx.node, "lin_test").unwrap();
    let id = file(&fx.fleet, &request(fx.node)).unwrap().id;
    let seen = std::cell::RefCell::new(None);
    let created = accept_with(&fx.fleet, id, "user", |key, sibling, title, body| {
        *seen.borrow_mut() = Some((key.to_string(), sibling.to_string(), title.to_string(), body.to_string()));
        Ok(CreatedIssue { identifier: "ENG-200".into(), url: "https://linear.app/x/issue/ENG-200".into() })
    })
    .unwrap();
    assert_eq!(created.identifier, "ENG-200");
    let (key, sibling, title, body) = seen.into_inner().unwrap();
    assert_eq!((key.as_str(), sibling.as_str()), ("lin_test", "ENG-100"));
    assert_eq!(title, "Add retry to webhooks");
    assert_eq!(body, "Retry failed deliveries.\n\nAcceptance: three attempts.");
    let s = fx.fleet.read(|conn| suggestions::get(conn, id)).unwrap().unwrap();
    assert_eq!(s.status, suggestions::STATUS_ACCEPTED);
    assert_eq!(s.result_id, "ENG-200");
    // Accepting twice is refused, never a second ticket.
    let again = accept_with(&fx.fleet, id, "user", |_, _, _, _| panic!("must not create again"));
    assert!(again.is_err());
}

#[test]
fn a_failed_creation_leaves_the_suggestion_open_with_the_error() {
    let fx = fixture();
    give_ticket(&fx, "ENG-100");
    tod_store::environment::store_linear_api_key(&fx.fleet, &fx.root, fx.node, "lin_test").unwrap();
    let id = file(&fx.fleet, &request(fx.node)).unwrap().id;
    let err = accept_with(&fx.fleet, id, "user", |_, _, _, _| Err(LinearError::Api("boom".into()))).unwrap_err();
    assert!(err.to_string().contains("boom"), "{err}");
    let s = fx.fleet.read(|conn| suggestions::get(conn, id)).unwrap().unwrap();
    assert!(s.is_open());
}

#[test]
fn accepting_needs_a_ticket_to_take_the_team_from() {
    let fx = fixture();
    tod_store::environment::store_linear_api_key(&fx.fleet, &fx.root, fx.node, "lin_test").unwrap();
    let id = file(&fx.fleet, &request(fx.node)).unwrap().id;
    let err = accept_with(&fx.fleet, id, "user", |_, _, _, _| panic!("no ticket, no creation")).unwrap_err();
    assert!(err.to_string().contains("Linear ticket"), "{err}");
}

#[test]
fn dismissing_records_the_decision() {
    let fx = fixture();
    let id = file(&fx.fleet, &request(fx.node)).unwrap().id;
    dismiss(&fx.fleet, id, "user").unwrap();
    let s = fx.fleet.read(|conn| suggestions::get(conn, id)).unwrap().unwrap();
    assert_eq!(s.status, suggestions::STATUS_DISMISSED);
    assert_eq!(s.resolved_by.as_deref(), Some("user"));
    assert!(dismiss(&fx.fleet, id, "user").is_err());
}

#[test]
fn likely_duplicates_are_kept_on_the_suggestion() {
    let fx = fixture();
    let mut new = NewSuggestion::follow_up_ticket(fx.node, "t", "b", suggestions::FILED_BY_TRIAGE);
    new.duplicates = vec![dup("ENG-7", "same thing")];
    new.duplicates_checked = true;
    let reply = fx.fleet.interview("triage", InterviewCommand::FileSuggestion(new)).unwrap();
    let id = Uuid::parse_str(reply["id"].as_str().unwrap()).unwrap();
    let s = fx.fleet.read(|conn| suggestions::get(conn, id)).unwrap().unwrap();
    assert_eq!(s.duplicates[0].id, "ENG-7");
    assert!(s.duplicates_checked_at.is_some());
}
