//! Outline integration tests.

use crate::fleet::store::FleetStore;
use crate::paths::{clear_data_root_override, set_data_root};
use crate::outline::types::Capability;
use crate::outline::{CreatePosition, OutlineMutation, resolve_obligations};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

#[test]
fn import_from_git_repo_while_data_root_is_sandboxed() {
    let git_root = std::env::current_dir().unwrap();
    if !git_root.join("doc").join("process").is_dir() {
        return;
    }
    let data = std::env::temp_dir().join(format!("tod-import-sandbox-{}", Uuid::new_v4()));
    fs::create_dir_all(&data).unwrap();
    set_data_root(data.clone());
    let store = FleetStore::open(&data).unwrap();
    store.import_doc_process(&git_root).unwrap();
    store.reload_if_stale().unwrap();
    let lists = store.list_outline_lists().unwrap();
    assert!(!lists.is_empty());
    let rows = store.flatten_outline(lists[0].id).unwrap();
    assert!(
        !rows.is_empty(),
        "import should read doc/process from git checkout, not --data-root"
    );
    clear_data_root_override();
    drop(store);
    let _ = fs::remove_dir_all(data);
}

#[test]
#[ignore = "manual: reimport doc/process into repo .local/data"]
fn reimport_local_data() {
    let git_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root");
    let data = git_root.join(".local").join("data");
    fs::create_dir_all(&data).unwrap();
    set_data_root(data.clone());
    let store = FleetStore::open(&data).unwrap();
    store.import_doc_process(&git_root).unwrap();
    store.projection().lock().unwrap().reload().unwrap();
    let lists = store.list_outline_lists().unwrap();
    let rows = store.flatten_outline(lists[0].id).unwrap();
    eprintln!(
        "reimported {} outline rows into {}",
        rows.len(),
        data.display()
    );
    assert!(!rows.is_empty());
    clear_data_root_override();
}

#[test]
fn import_and_resolve_obligations_round_trip() {
    let root = std::env::temp_dir().join(format!("tod-outline-it-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store.import_doc_process(&root).ok();
    store.reload_if_stale().ok();
    let lists = store.list_outline_lists().unwrap();
    if lists.is_empty() {
        drop(store);
        let _ = fs::remove_dir_all(root);
        return;
    }
    let rows = store.flatten_outline(lists[0].id).unwrap();
    if let Some(row) = rows
        .iter()
        .find(|r| r.capabilities.contains(&Capability::Spec))
    {
        let projection = store.projection();
        let guard = projection.lock().unwrap();
        let conn = guard.connection();
        let resolved = resolve_obligations(&conn, row.node.id).unwrap();
        assert!(resolved.is_empty() || !resolved[0].obligation.body.is_empty());
    }
    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn reparent_and_loop_detection() {
    let root = std::env::temp_dir().join(format!("tod-outline-loop-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "loop".into(),
            title: "Loop".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let list_id = store.list_outline_lists().unwrap()[0].id;
    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: None,
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "A".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();
    let rows = store.flatten_outline(list_id).unwrap();
    assert_eq!(rows.len(), 1);
    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn reorder_sibling_past_subtree() {
    use crate::outline::ReorderDirection;

    let root = std::env::temp_dir().join(format!("tod-outline-reorder-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "r".into(),
            title: "R".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let list_id = store.list_outline_lists().unwrap()[0].id;

    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: None,
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "A".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();
    let a_id = store.flatten_outline(list_id).unwrap()[0].node.id;

    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: None,
            list_id,
            parent_id: None,
            anchor_id: Some(a_id),
            position: CreatePosition::Below,
            title: "B".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();
    let rows = store.flatten_outline(list_id).unwrap();
    let b_id = rows.iter().find(|r| r.node.title == "B").unwrap().node.id;

    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: None,
            list_id,
            parent_id: Some(a_id),
            anchor_id: Some(a_id),
            position: CreatePosition::Child,
            title: "A1".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();

    store
        .enqueue_outline(OutlineMutation::ReorderSibling {
            node_id: b_id,
            direction: ReorderDirection::Up,
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();

    let titles: Vec<_> = store
        .flatten_outline(list_id)
        .unwrap()
        .into_iter()
        .map(|r| r.node.title)
        .collect();
    assert_eq!(titles, vec!["B", "A", "A1"]);

    store
        .enqueue_outline(OutlineMutation::ReorderSibling {
            node_id: a_id,
            direction: ReorderDirection::Down,
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();

    let titles: Vec<_> = store
        .flatten_outline(list_id)
        .unwrap()
        .into_iter()
        .map(|r| r.node.title)
        .collect();
    assert_eq!(titles, vec!["B", "A", "A1"]);

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn reorder_sibling_past_parent_to_next_aunt() {
    use crate::outline::ReorderDirection;

    let root = std::env::temp_dir().join(format!("tod-outline-aunt-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "aunt".into(),
            title: "Aunt".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let list_id = store.list_outline_lists().unwrap()[0].id;

    let create_top = |title: &str, anchor: Option<Uuid>| -> Uuid {
        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: anchor,
                position: if anchor.is_some() {
                    CreatePosition::Below
                } else {
                    CreatePosition::Below
                },
                title: title.into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        store
            .flatten_outline(list_id)
            .unwrap()
            .into_iter()
            .find(|r| r.node.title == title)
            .unwrap()
            .node
            .id
    };

    let tod_id = create_top("tod", None);
    let _ = create_top("Archive", Some(tod_id));
    let child1_id = {
        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: Some(tod_id),
                anchor_id: Some(tod_id),
                position: CreatePosition::Child,
                title: "Interview".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        store
            .flatten_outline(list_id)
            .unwrap()
            .into_iter()
            .find(|r| r.node.title == "Interview")
            .unwrap()
            .node
            .id
    };
    let situational_id = {
        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: Some(tod_id),
                anchor_id: Some(child1_id),
                position: CreatePosition::Below,
                title: "Situational UI".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        store
            .flatten_outline(list_id)
            .unwrap()
            .into_iter()
            .find(|r| r.node.title == "Situational UI")
            .unwrap()
            .node
            .id
    };

    store
        .enqueue_outline(OutlineMutation::ReorderSibling {
            node_id: situational_id,
            direction: ReorderDirection::Down,
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();

    let rows = store.flatten_outline(list_id).unwrap();
    let archive_id = rows
        .iter()
        .find(|r| r.node.title == "Archive")
        .unwrap()
        .node
        .id;
    let parent_id = {
        let projection = store.projection();
        let guard = projection.lock().unwrap();
        let conn = guard.connection();
        crate::outline::repos::OutlineRepo::new(&conn)
            .get_entry(situational_id)
            .unwrap()
            .unwrap()
            .parent_id
    };
    assert_eq!(parent_id, Some(archive_id));

    let titles: Vec<_> = rows.into_iter().map(|r| r.node.title).collect();
    assert_eq!(
        titles,
        vec!["tod", "Interview", "Archive", "Situational UI"]
    );

    store
        .enqueue_outline(OutlineMutation::ReorderSibling {
            node_id: situational_id,
            direction: ReorderDirection::Up,
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();

    let parent_id = {
        let projection = store.projection();
        let guard = projection.lock().unwrap();
        let conn = guard.connection();
        crate::outline::repos::OutlineRepo::new(&conn)
            .get_entry(situational_id)
            .unwrap()
            .unwrap()
            .parent_id
    };
    assert_eq!(parent_id, Some(tod_id));
    let titles: Vec<_> = store
        .flatten_outline(list_id)
        .unwrap()
        .into_iter()
        .map(|r| r.node.title)
        .collect();
    assert_eq!(
        titles,
        vec!["tod", "Interview", "Situational UI", "Archive"]
    );

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn obligation_crud_and_counts() {
    use crate::outline::{KIND_CONSTRAINT, KIND_REQUIREMENT, ReorderDirection};

    let root = std::env::temp_dir().join(format!("tod-obl-crud-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "obl".into(),
            title: "Obl".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let list_id = store.list_outline_lists().unwrap()[0].id;
    let node_id = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node_id),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Spec node".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id,
            capabilities: vec![Capability::Spec],
        })
        .unwrap();
    store.writer().flush().unwrap();

    let req_a = Uuid::new_v4();
    let req_b = Uuid::new_v4();
    let con = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateObligation {
            obligation_id: Some(req_a),
            node_id,
            kind: KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            body: "Req A".into(),
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateObligation {
            obligation_id: Some(req_b),
            node_id,
            kind: KIND_REQUIREMENT.into(),
            after_id: Some(req_a),
            before: false,
            body: "Req B".into(),
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateObligation {
            obligation_id: Some(con),
            node_id,
            kind: KIND_CONSTRAINT.into(),
            after_id: None,
            before: false,
            body: "Con 1".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();

    let counts = store.obligation_counts_for_list(list_id).unwrap();
    let c = counts.get(&node_id).copied().unwrap();
    assert_eq!(c.requirements, 2);
    assert_eq!(c.constraints, 1);

    store
        .enqueue_outline(OutlineMutation::ReorderObligation {
            obligation_id: req_b,
            direction: ReorderDirection::Up,
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();
    let bodies: Vec<_> = store
        .list_obligations_for_node(node_id)
        .unwrap()
        .into_iter()
        .filter(|o| o.kind == KIND_REQUIREMENT)
        .map(|o| o.body)
        .collect();
    assert_eq!(bodies, vec!["Req B", "Req A"]);

    store
        .enqueue_outline(OutlineMutation::UpdateObligationBody {
            obligation_id: con,
            body: "Con updated".into(),
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::DeleteObligation {
            obligation_id: req_a,
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();

    let counts = store.obligation_counts_for_list(list_id).unwrap();
    let c = counts.get(&node_id).copied().unwrap();
    assert_eq!(c.requirements, 1);
    assert_eq!(c.constraints, 1);
    let cons = store.list_obligations_for_node(node_id).unwrap();
    assert_eq!(
        cons.iter().find(|o| o.id == con).unwrap().body,
        "Con updated"
    );

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn delete_node_and_undo_restore() {
    let root = std::env::temp_dir().join(format!("tod-del-undo-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "delu".into(),
            title: "DelU".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let list_id = store.list_outline_lists().unwrap()[0].id;
    let parent_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(parent_id),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Parent note".into(),
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(child_id),
            list_id,
            parent_id: Some(parent_id),
            anchor_id: None,
            position: CreatePosition::Child,
            title: "Child note".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();

    store
        .enqueue_outline(OutlineMutation::DeleteNode { node_id: parent_id })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();
    assert!(store.flatten_outline(list_id).unwrap().is_empty());
    assert!(!store.command_log().lock().unwrap().entries().is_empty());

    let label = store.undo_last().unwrap().expect("undo label");
    assert!(label.contains("Parent note"));
    store.reload_if_stale().ok();
    let titles: Vec<_> = store
        .flatten_outline(list_id)
        .unwrap()
        .into_iter()
        .map(|r| r.node.title)
        .collect();
    assert!(titles.contains(&"Parent note".to_string()));
    assert!(titles.contains(&"Child note".to_string()));

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn gate_criteria_seed_on_migration() {
    use crate::fleet::schema;
    use crate::outline::GATE_CRITERIA;
    use crate::outline::repos::GateRepo;

    let root = std::env::temp_dir().join(format!("tod-gate-seed-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let db = root.join("tod.db");
    let _store = FleetStore::open(&root).unwrap();
    let conn = schema::open_writer_connection(&db).unwrap();
    let repo = GateRepo::new(&conn);
    let design_planning = repo.list_for_transition("design", "planning").unwrap();
    assert_eq!(design_planning.len(), 11);
    let planning_ready = repo.list_for_transition("planning", "ready").unwrap();
    assert_eq!(planning_ready.len(), 12);
    let verifying_review = repo.list_for_transition("verifying", "review").unwrap();
    assert_eq!(verifying_review.len(), 9);
    assert_eq!(
        design_planning.len() + planning_ready.len() + verifying_review.len(),
        GATE_CRITERIA.len()
    );
    let _ = fs::remove_dir_all(root);
}
