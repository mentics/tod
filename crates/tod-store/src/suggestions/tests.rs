use super::*;
use crate::fleet::schema;
use crate::outline::repos::NodeRepo;

fn setup() -> (Connection, Uuid) {
    let dir = tod_test_util::scratch_dir(format!("tod-suggestions-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
    let node = Uuid::new_v4();
    NodeRepo::new(&conn).create_with_id(node, "n", "N").unwrap();
    (conn, node)
}

fn new(node: Uuid, title: &str, body: &str) -> NewSuggestion {
    NewSuggestion::follow_up_ticket(node, title, body, FILED_BY_AGENT)
}

#[test]
fn files_and_lists_a_suggestion() {
    let (conn, node) = setup();
    let filed = file(&conn, &new(node, "Add retry", "Retry the call.\n\nAcceptance: it retries.")).unwrap();
    assert!(!filed.already_open);
    let all = list_for_node(&conn, node).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, filed.id);
    assert_eq!(all[0].status, STATUS_OPEN);
    assert_eq!(all[0].duplicates_checked_at, None);
    assert!(all[0].duplicates.is_empty());
}

#[test]
fn an_identical_open_suggestion_is_not_filed_twice() {
    let (conn, node) = setup();
    let first = file(&conn, &new(node, "Add retry", "Retry  the call.")).unwrap();
    let again = file(&conn, &new(node, "add RETRY", "retry the call.")).unwrap();
    assert!(again.already_open);
    assert_eq!(again.id, first.id);
    assert_eq!(list_for_node(&conn, node).unwrap().len(), 1);
    // A different body is a different suggestion.
    assert!(!file(&conn, &new(node, "Add retry", "Something else.")).unwrap().already_open);
    // Once decided, the same text may be filed again.
    resolve(&conn, first.id, STATUS_DISMISSED, "", "", "user").unwrap();
    assert!(!file(&conn, &new(node, "Add retry", "Retry the call.")).unwrap().already_open);
}

#[test]
fn refuses_what_is_not_a_suggestion() {
    let (conn, node) = setup();
    assert!(file(&conn, &new(node, " ", "body")).is_err());
    assert!(file(&conn, &new(node, "title", " ")).is_err());
    let mut bad = new(node, "t", "b");
    bad.kind = "other".into();
    assert!(file(&conn, &bad).is_err());
    let mut bad = new(node, "t", "b");
    bad.filed_by = "someone".into();
    assert!(file(&conn, &bad).is_err());
}

#[test]
fn duplicates_are_stored_with_whether_the_search_ran() {
    let (conn, node) = setup();
    let mut n = new(node, "Add retry", "Retry.");
    n.duplicates = vec![Duplicate { id: "ENG-1".into(), title: "Retry calls".into(), url: "https://x/ENG-1".into() }];
    n.duplicates_checked = true;
    let id = file(&conn, &n).unwrap().id;
    let s = get(&conn, id).unwrap().unwrap();
    assert!(s.duplicates_checked_at.is_some());
    assert_eq!(s.duplicates[0].id, "ENG-1");

    let id2 = file(&conn, &new(node, "Other", "x")).unwrap().id;
    set_duplicates(&conn, id2, &[], false, "Linear is not configured for this task").unwrap();
    let s2 = get(&conn, id2).unwrap().unwrap();
    assert_eq!(s2.duplicates_checked_at, None);
    assert_eq!(s2.duplicates_note, "Linear is not configured for this task");
}

#[test]
fn resolving_is_once_and_needs_a_ticket_to_accept() {
    let (conn, node) = setup();
    let id = file(&conn, &new(node, "t", "b")).unwrap().id;
    assert!(resolve(&conn, id, STATUS_ACCEPTED, "", "", "user").is_err());
    resolve(&conn, id, STATUS_ACCEPTED, "ENG-9", "https://x/ENG-9", "user").unwrap();
    let s = get(&conn, id).unwrap().unwrap();
    assert_eq!((s.status.as_str(), s.result_id.as_str()), (STATUS_ACCEPTED, "ENG-9"));
    assert!(resolve(&conn, id, STATUS_DISMISSED, "", "", "user").is_err());
    assert!(open_for_node(&conn, node).unwrap().is_empty());
    assert_eq!(list_for_node(&conn, node).unwrap().len(), 1);
}

#[test]
fn changes_are_journaled() {
    let (conn, node) = setup();
    let id = file(&conn, &new(node, "t", "b")).unwrap().id;
    resolve(&conn, id, STATUS_DISMISSED, "", "", "user").unwrap();
    let ops: Vec<String> = conn
        .prepare("SELECT op FROM journey_changes WHERE tbl = 'node_suggestions' ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(ops, ["insert", "update"]);
}
