//! Outline integration tests.

use crate::fleet::store::FleetStore;
use crate::interview::PHASE_REQUIREMENTS;
use crate::outline::types::Capability;
use crate::outline::repos::{NodeRepo, OutlineRepo};
use crate::outline::{CreatePosition, OutlineMutation};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

#[test]
fn create_node_requires_title_and_slugs_it() {
    let root = std::env::temp_dir().join(format!("tod-create-title-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "titles".into(),
            title: "Titles".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let list_id = store.list_outline_lists().unwrap()[0].id;

    let create = |title: &str| {
        let id = Uuid::new_v4();
        let queued = store.enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(id),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: title.into(),
        });
        let flushed = queued.and_then(|_| store.writer().flush());
        (id, flushed)
    };

    let (blank, result) = create("   ");
    assert!(result.is_err(), "a blank title must not create a node");
    store.reload_if_stale().ok();
    assert!(store.get_node(&blank.to_string()).unwrap().is_none());

    let (named, result) = create("Fix login bug");
    result.unwrap();
    store.reload_if_stale().ok();
    let node = store.get_node(&named.to_string()).unwrap().unwrap();
    assert_eq!(node.slug, "fix-login-bug");

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn obligation_text_cannot_reference_files() {
    use crate::outline::KIND_CONSTRAINT;

    let root = std::env::temp_dir().join(format!("tod-obl-files-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "files".into(),
            title: "Files".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let list_id = store.list_outline_lists().unwrap()[0].id;
    let node_id = Uuid::new_v4();
    for mutation in [
        OutlineMutation::CreateNode {
            node_id: Some(node_id),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Spec node".into(),
        },
        OutlineMutation::EnableCapabilities {
            node_id,
            capabilities: vec![Capability::Spec],
        },
    ] {
        store.enqueue_outline(mutation).unwrap();
        store.writer().flush().unwrap();
    }

    let create = |body: &str| {
        let id = Uuid::new_v4();
        let result = store
            .enqueue_outline(OutlineMutation::CreateObligation {
                obligation_id: Some(id),
                node_id,
                kind: KIND_CONSTRAINT.into(),
                after_id: None,
                before: false,
                section: None,
                body: body.into(),
                phase: crate::interview::PHASE_DESIGN.into(),
            })
            .and_then(|_| store.writer().flush());
        (id, result)
    };

    let (_, linked) = create(
        "Row control activation — Follow [`doc/process/shared/constraints/row-control-activation-constraints.md`](../../shared/constraints/row-control-activation-constraints.md).",
    );
    assert!(linked.is_err(), "a file link must not become an obligation");

    let (kept, result) = create("Row controls activate on Enter and on click.");
    result.unwrap();
    let edited = store
        .enqueue_outline(OutlineMutation::UpdateObligationBody {
            obligation_id: kept,
            body: "Row controls activate as described in doc/row-controls.md.".into(),
        })
        .and_then(|_| store.writer().flush());
    assert!(edited.is_err(), "an edit must not add a file reference");

    store.reload_if_stale().ok();
    let bodies: Vec<String> = store
        .read(|conn| {
            let mut stmt = conn.prepare("SELECT body FROM node_obligations")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .unwrap();
    assert_eq!(bodies, vec!["Row controls activate on Enter and on click.".to_string()]);

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn resolve_obligations_for_node_includes_ancestor_and_design_phase() {
    use crate::interview::PHASE_DESIGN;
    use crate::outline::KIND_REQUIREMENT;

    let root = std::env::temp_dir().join(format!("tod-resolve-node-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "resolve".into(),
            title: "Resolve".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let list_id = store.list_outline_lists().unwrap()[0].id;

    let parent_id = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(parent_id),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Parent".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: parent_id,
            capabilities: vec![Capability::Spec],
        })
        .unwrap();
    store.writer().flush().unwrap();

    let child_id = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(child_id),
            list_id,
            parent_id: Some(parent_id),
            anchor_id: None,
            position: CreatePosition::Child,
            title: "Child".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: child_id,
            capabilities: vec![Capability::Spec],
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .enqueue_outline(OutlineMutation::CreateObligation {
            obligation_id: Some(Uuid::new_v4()),
            node_id: parent_id,
            kind: KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            section: None,
            body: "Ancestor requirement.".into(),
            phase: PHASE_REQUIREMENTS.into(),
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateObligation {
            obligation_id: Some(Uuid::new_v4()),
            node_id: child_id,
            kind: KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            section: None,
            body: "Child design decision.".into(),
            phase: PHASE_DESIGN.into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();

    // The local-only accessor sees just the child's own obligation — this is
    // what a gate check must NOT rely on for traceability.
    let local = store.list_obligations_for_node(child_id).unwrap();
    assert_eq!(local.len(), 1);
    assert_eq!(local[0].body, "Child design decision.");

    // The resolved accessor a gate check should use sees both, regardless of
    // phase — the ancestor's requirements-phase obligation and the node's
    // own design-phase obligation.
    let resolved = store.resolve_obligations_for_node(child_id).unwrap();
    let bodies: Vec<&str> = resolved.iter().map(|o| o.body.as_str()).collect();
    assert!(bodies.contains(&"Ancestor requirement."), "{bodies:?}");
    assert!(bodies.contains(&"Child design decision."), "{bodies:?}");

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
            section: None,
            body: "Req A".into(),
            phase: PHASE_REQUIREMENTS.into(),
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateObligation {
            obligation_id: Some(req_b),
            node_id,
            kind: KIND_REQUIREMENT.into(),
            after_id: Some(req_a),
            before: false,
            section: None,
            body: "Req B".into(),
            phase: PHASE_REQUIREMENTS.into(),
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateObligation {
            obligation_id: Some(con),
            node_id,
            kind: KIND_CONSTRAINT.into(),
            after_id: None,
            before: false,
            section: None,
            body: "Con 1".into(),
            phase: PHASE_REQUIREMENTS.into(),
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

    let other_spec_node = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(other_spec_node),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Other spec node".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: other_spec_node,
            capabilities: vec![Capability::Spec],
        })
        .unwrap();
    store.writer().flush().unwrap();

    let non_spec_node = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(non_spec_node),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Plain node".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();

    assert!(
        store
            .enqueue_outline(OutlineMutation::MoveObligation {
                obligation_id: req_b,
                target_node_id: non_spec_node,
            })
            .and_then(|_| store.writer().flush())
            .is_err()
    );
    store.reload_if_stale().ok();
    let req_b_node = store
        .list_obligations_for_node(node_id)
        .unwrap()
        .into_iter()
        .find(|o| o.id == req_b)
        .unwrap()
        .node_id;
    assert_eq!(
        req_b_node, node_id,
        "rejected move must leave obligation in place"
    );

    store
        .enqueue_outline(OutlineMutation::MoveObligation {
            obligation_id: req_b,
            target_node_id: other_spec_node,
        })
        .unwrap();
    store.writer().flush().unwrap();
    store.reload_if_stale().ok();

    let source_bodies: Vec<_> = store
        .list_obligations_for_node(node_id)
        .unwrap()
        .into_iter()
        .filter(|o| o.kind == KIND_REQUIREMENT)
        .map(|o| o.body)
        .collect();
    assert!(!source_bodies.contains(&"Req B".to_string()));
    let target_bodies: Vec<_> = store
        .list_obligations_for_node(other_spec_node)
        .unwrap()
        .into_iter()
        .map(|o| o.body)
        .collect();
    assert_eq!(target_bodies, vec!["Req B"]);

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn delete_node_blocked_when_agents_present() {
    use crate::fleet::writer::FleetMutation;

    let root = std::env::temp_dir().join(format!("tod-del-blocked-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "blocked".into(),
            title: "Blocked".into(),
        })
        .unwrap();
    let list_id = store.list_outline_lists().unwrap()[0].id;
    let node_id = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node_id),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Agent task".into(),
        })
        .unwrap();
    store.reload_if_stale().ok();

    store
        .enqueue(FleetMutation::CreateShellSession {
            id: Uuid::new_v4().to_string(),
            node_id: node_id.to_string(),
            reconnect: None,
        })
        .unwrap();

    let err = store
        .enqueue_outline(OutlineMutation::DeleteNode { node_id })
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("node \"Agent task\" has running agents or open shells"),
        "unexpected error: {err}"
    );
    store.reload_if_stale().ok();
    assert_eq!(store.flatten_outline(list_id).unwrap().len(), 1);

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
    let slugs = |from: &str, to: &str| -> Vec<String> {
        repo.list_for_transition(from, to)
            .unwrap()
            .into_iter()
            .map(|c| c.slug)
            .collect()
    };
    use crate::outline::repos::gate::*;
    // Every gate is the app's own check; the agent-judged rows are retired.
    assert_eq!(
        slugs("proposed", "design"),
        [PROPOSED_DESIGN_HAS_REQUIREMENTS_SLUG, PROPOSED_DESIGN_PHASE_CERTIFIED_SLUG]
    );
    assert_eq!(slugs("design", "planning"), [DESIGN_PLANNING_PHASE_CERTIFIED_SLUG]);
    assert_eq!(
        slugs("planning", "ready"),
        [PLANNING_READY_REQUIREMENTS_TRACEABLE_SLUG, PLANNING_READY_PHASE_CERTIFIED_SLUG]
    );
    assert_eq!(slugs("ready", "active"), [READY_ACTIVE_ACTION_CONFIG_SLUG]);
    assert_eq!(slugs("active", "verifying"), [ACTIVE_VERIFYING_PLAN_IMPLEMENTED_SLUG]);
    assert_eq!(
        slugs("verifying", "review"),
        [VERIFYING_REVIEW_OBLIGATIONS_VERIFIED_SLUG, VERIFYING_REVIEW_PLAN_VERIFIED_SLUG]
    );
    assert_eq!(
        slugs("review", "pr"),
        [REVIEW_APPROVED_REVIEW_DONE_SLUG, REVIEW_APPROVED_FINDINGS_ANSWERED_SLUG]
    );
    assert_eq!(
        slugs("pr", "approved"),
        [
            PR_APPROVED_MERGEABLE_SLUG,
            PR_APPROVED_UP_TO_DATE_SLUG,
            PR_APPROVED_THREADS_RESOLVED_SLUG,
            PR_APPROVED_REVIEW_CURRENT_SLUG,
            PR_APPROVED_REVIEW_SCORE_SLUG
        ]
    );
    assert_eq!(slugs("approved", "merged"), [APPROVED_MERGED_PR_MERGED_SLUG]);
    assert_eq!(
        slugs("merged", "released"),
        [MERGED_RELEASED_PHASE_CERTIFIED_SLUG, MERGED_RELEASED_PLAN_VERIFIED_SLUG]
    );
    assert_eq!(
        slugs("released", "learn"),
        [RELEASED_LEARN_PHASE_CERTIFIED_SLUG, RELEASED_LEARN_PLAN_VERIFIED_SLUG]
    );
    assert_eq!(slugs("learn", "done"), [LEARN_DONE_LEARN_RECORDED_SLUG]);

    // Exactly the derived criteria are active, and each is seeded.
    let mut active: Vec<String> = conn
        .prepare("SELECT slug FROM gate_criteria WHERE active = 1")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    active.sort();
    let mut derived: Vec<String> = DERIVED_CRITERION_SLUGS.iter().map(|s| s.to_string()).collect();
    derived.sort();
    assert_eq!(active, derived);
    for slug in DERIVED_CRITERION_SLUGS {
        assert!(GATE_CRITERIA.iter().any(|c| c.slug == *slug), "{slug} is not seeded");
    }
    assert!(!repo.get_by_slug(BUILDABLE_CRITERION_SLUG).unwrap().unwrap().active);

    // Reseeding (every open) leaves it so.
    crate::outline::seed_gate_criteria(&conn).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM gate_criteria WHERE active = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count as usize, DERIVED_CRITERION_SLUGS.len());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn plan_step_dependency_graph_and_obligation_links() {
    use crate::outline::repos::PlanStepRepo;

    let root = std::env::temp_dir().join(format!("tod-plan-step-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "plan".into(),
            title: "Plan".into(),
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

    let req = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateObligation {
            obligation_id: Some(req),
            node_id,
            kind: crate::outline::KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            section: None,
            body: "Req".into(),
            phase: PHASE_REQUIREMENTS.into(),
        })
        .unwrap();

    let step_a = Uuid::new_v4();
    let step_b = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreatePlanStep {
            step_id: Some(step_a),
            node_id,
            after_id: None,
            before: false,
            body: "Step A".into(),
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::CreatePlanStep {
            step_id: Some(step_b),
            node_id,
            after_id: Some(step_a),
            before: false,
            body: "Step B".into(),
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::AddPlanStepDependency {
            step_id: step_b,
            depends_on_step_id: step_a,
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::LinkPlanStepObligation {
            step_id: step_a,
            obligation_id: req,
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let repo = PlanStepRepo::new(conn);

            // A cycle is rejected.
            assert!(repo.add_dependency(step_a, step_b).is_err());

            // step_b is blocked on step_a until step_a is implemented.
            assert_eq!(repo.ready_steps(node_id, None).unwrap(), vec![step_a]);

            assert_eq!(repo.list_obligations(step_a).unwrap(), vec![req]);
            assert_eq!(repo.list_steps_for_obligation(req).unwrap(), vec![step_a]);
            Ok(())
        })
        .unwrap();

    // The same step cannot be added twice, nor can an edit make one a copy
    // of another; whitespace differences don't disguise a duplicate.
    let dup = store.enqueue_outline(OutlineMutation::CreatePlanStep {
        step_id: None,
        node_id,
        after_id: None,
        before: false,
        body: "  Step   A\n".into(),
    });
    assert!(dup.is_err() || store.writer().flush().is_err());
    let rename = store.enqueue_outline(OutlineMutation::UpdatePlanStepBody {
        step_id: step_b,
        body: "Step A".into(),
    });
    assert!(rename.is_err() || store.writer().flush().is_err());
    store
        .enqueue_outline(OutlineMutation::UpdatePlanStepBody {
            step_id: step_a,
            body: "Step A".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .read(|conn| {
            assert_eq!(PlanStepRepo::new(conn).list_ids_for_node(node_id).unwrap().len(), 2);
            Ok(())
        })
        .unwrap();

    store
        .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
            step_id: step_a,
            status: "implemented".into(),
            note: None,
            reason: None,
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let repo = PlanStepRepo::new(conn);
            // step_b is now unblocked (auto-promoted to ready).
            let mut ready = repo.ready_steps(node_id, None).unwrap();
            ready.sort();
            let mut expected = vec![step_b];
            expected.sort();
            assert_eq!(ready, expected);
            assert_eq!(repo.get(step_b).unwrap().unwrap().status, "ready");
            Ok(())
        })
        .unwrap();

    // Verification fails step_a twice, with implementation in between: each
    // note is kept, oldest first, and the step's own note is the latest.
    let set = |status: &str, note: Option<&str>| {
        store
            .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                step_id: step_a,
                status: status.into(),
                note: note.map(str::to_string),
                reason: None,
            })
            .unwrap();
        store.writer().flush().unwrap();
    };
    set("failed", Some("Empty input panics"));
    set("implemented", None);
    set("failed", Some("Still panics on whitespace"));
    // Setting the same note again, as reversing a change would, adds nothing.
    set("failed", Some("Empty input panics"));
    store
        .read(|conn| {
            let repo = PlanStepRepo::new(conn);
            let step = repo.get(step_a).unwrap().unwrap();
            assert_eq!(step.status, "failed");
            assert_eq!(step.note.as_deref(), Some("Empty input panics"));
            let notes: Vec<(String, String)> = repo
                .list_notes(step_a)
                .unwrap()
                .into_iter()
                .map(|n| (n.status, n.body))
                .collect();
            assert_eq!(
                notes,
                vec![
                    ("failed".into(), "Empty input panics".into()),
                    ("failed".into(), "Still panics on whitespace".into()),
                ]
            );
            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

// ── Generator capability tests ──────────────────────────────────────────

fn setup_store_with_list() -> (FleetStore, PathBuf, Uuid) {
    let root = std::env::temp_dir().join(format!("tod-gen-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "gen-test".into(),
            title: "Gen Test".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let list_id = store.list_outline_lists().unwrap()[0].id;
    (store, root, list_id)
}

fn create_node_in(store: &FleetStore, list_id: Uuid, parent_id: Option<Uuid>, title: &str) -> Uuid {
    let id = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(id),
            list_id,
            parent_id,
            anchor_id: parent_id,
            position: if parent_id.is_some() {
                CreatePosition::Child
            } else {
                CreatePosition::Below
            },
            title: title.into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    id
}

#[test]
fn generator_capability_on_empty_node_succeeds() {
    let (store, root, list_id) = setup_store_with_list();
    let node_id = create_node_in(&store, list_id, None, "Generator Node");

    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id,
            capabilities: vec![Capability::Generator],
        })
        .unwrap();
    let flush_result = store.writer().flush();
    assert!(
        flush_result.is_ok(),
        "flush should succeed for Generator on empty node: {:?}",
        flush_result.err()
    );
    store.projection().lock().unwrap().reload().unwrap();

    // Direct check: open a fresh connection and query capabilities
    let db_path = root.join("tod.db");
    let fresh_conn = crate::fleet::schema::open_read_connection(&db_path).unwrap();
    let caps = crate::outline::repos::NodeRepo::new(&fresh_conn)
        .list_capabilities(node_id)
        .unwrap();
    assert!(
        caps.contains(&Capability::Generator),
        "expected Generator in capabilities, got: {:?}",
        caps
    );

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn generator_capability_on_node_with_children_fails() {
    let (store, root, list_id) = setup_store_with_list();
    let parent_id = create_node_in(&store, list_id, None, "Parent");
    let _child_id = create_node_in(&store, list_id, Some(parent_id), "Child");

    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: parent_id,
            capabilities: vec![Capability::Generator],
        })
        .unwrap();
    let result = store.writer().flush();
    // The flush may succeed (if mutations are individually swallowed) or fail.
    // Either way, the capability should not be enabled.
    if result.is_ok() {
        store
            .read(|conn| {
                let caps =
                    crate::outline::repos::NodeRepo::new(conn).list_capabilities(parent_id)?;
                assert!(
                    !caps.contains(&Capability::Generator),
                    "Generator should not be enabled on a node with children"
                );
                Ok(())
            })
            .unwrap();
    }
    // If flush failed, the error message should be about children.

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn generator_and_lifecycle_are_mutually_exclusive() {
    let (store, root, list_id) = setup_store_with_list();

    // Test 1: Enable Lifecycle first, then Generator should fail.
    let node1 = create_node_in(&store, list_id, None, "Lifecycle-first");
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: node1,
            capabilities: vec![Capability::Lifecycle],
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: node1,
            capabilities: vec![Capability::Generator],
        })
        .unwrap();
    let result = store.writer().flush();
    if result.is_ok() {
        store
            .read(|conn| {
                let caps = crate::outline::repos::NodeRepo::new(conn).list_capabilities(node1)?;
                assert!(
                    !caps.contains(&Capability::Generator),
                    "Generator should not coexist with Lifecycle"
                );
                Ok(())
            })
            .unwrap();
    }

    // Test 2: Enable Generator first, then Lifecycle should fail.
    let node2 = create_node_in(&store, list_id, None, "Generator-first");
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: node2,
            capabilities: vec![Capability::Generator],
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: node2,
            capabilities: vec![Capability::Lifecycle],
        })
        .unwrap();
    let result = store.writer().flush();
    if result.is_ok() {
        store
            .read(|conn| {
                let caps = crate::outline::repos::NodeRepo::new(conn).list_capabilities(node2)?;
                assert!(
                    !caps.contains(&Capability::Lifecycle),
                    "Lifecycle should not coexist with Generator"
                );
                Ok(())
            })
            .unwrap();
    }

    drop(store);
    let _ = fs::remove_dir_all(root);
}

// ── Generator lifecycle cleanup ──────────────────────────────────────────

fn create_managed_node_for_test(
    store: &FleetStore,
    list_id: Uuid,
    parent_id: Uuid,
    generator_node_id: Uuid,
    external_id: &str,
    title: &str,
) -> Uuid {
    create_managed_node_of_type(
        store,
        list_id,
        parent_id,
        generator_node_id,
        external_id,
        title,
        "mock",
    )
}

fn create_managed_node_of_type(
    store: &FleetStore,
    list_id: Uuid,
    parent_id: Uuid,
    generator_node_id: Uuid,
    external_id: &str,
    title: &str,
    source_type: &str,
) -> Uuid {
    ensure_generator(store, generator_node_id, source_type);
    let node_id = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateManagedNode {
            node_id: Some(node_id),
            list_id,
            parent_id,
            title: title.into(),
            external_id: external_id.into(),
            tags: vec![],
            body: String::new(),
            metadata: None,
        })
        .unwrap();
    store.writer().flush().unwrap();
    node_id
}

/// Make `node_id` a generator of `source_type`, unless it is one already:
/// a managed node's source type is its generator's.
fn ensure_generator(store: &FleetStore, node_id: Uuid, source_type: &str) {
    let configured = store
        .read(move |conn| Ok(crate::outline::repos::GeneratorRepo::new(conn).get_config(node_id)?.is_some()))
        .unwrap();
    if configured {
        return;
    }
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id,
            capabilities: vec![Capability::Generator],
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::SetGeneratorConfig {
            node_id,
            data_source_type: source_type.into(),
            config_json: "{}".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
}

/// Give a plain node ticket `ticket` through its Ticket capability.
fn set_ticket(store: &FleetStore, node_id: Uuid, ticket: &str) {
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id,
            capabilities: vec![Capability::Ticket],
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::SetNodeTicket {
            node_id,
            ticket: Some(ticket.into()),
            linked_prs: vec![],
        })
        .unwrap();
    store.writer().flush().unwrap();
}

fn paste_copy_of(store: &FleetStore, list_id: Uuid, source: Uuid, parent: Uuid) -> Uuid {
    store
        .enqueue_outline(OutlineMutation::PasteManagedNodeCopy {
            source_node_id: source,
            list_id,
            parent_id: Some(parent),
            ordinal: 0,
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .read(|conn| {
            Ok(OutlineRepo::new(conn)
                .list_for_list(list_id)?
                .into_iter()
                .find(|e| e.parent_id == Some(parent))
                .unwrap()
                .node_id)
        })
        .unwrap()
}

#[test]
fn pasted_linear_copy_carries_its_id_as_a_ticket_not_in_its_title() {
    let (store, root, list_id) = setup_store_with_list();
    let generator_id = create_node_in(&store, list_id, None, "Generator");
    let mock_generator = create_node_in(&store, list_id, None, "Mock generator");
    let outside = create_node_in(&store, list_id, None, "Outside");
    let linear = create_managed_node_of_type(
        &store,
        list_id,
        generator_id,
        generator_id,
        "ENG-42",
        "Fix it",
        "linear",
    );
    let other = create_managed_node_for_test(
        &store,
        list_id,
        mock_generator,
        mock_generator,
        "EXT-1",
        "Other",
    );

    let linear_copy = paste_copy_of(&store, list_id, linear, outside);
    store
        .read(|conn| {
            let nodes = NodeRepo::new(conn);
            assert_eq!(nodes.get(linear_copy)?.unwrap().title, "Fix it");
            assert!(nodes.list_capabilities(linear_copy)?.contains(&Capability::Ticket));
            assert_eq!(nodes.get_ticket_id(linear_copy)?.as_deref(), Some("ENG-42"));
            Ok(())
        })
        .unwrap();

    let other_copy = paste_copy_of(&store, list_id, other, outside);
    store
        .read(|conn| {
            let nodes = NodeRepo::new(conn);
            assert_eq!(nodes.get(other_copy)?.unwrap().title, "EXT-1: Other");
            assert!(!nodes.list_capabilities(other_copy)?.contains(&Capability::Ticket));
            // Still that ticket, so refreshes reach it.
            assert_eq!(nodes.get_ticket_id(other_copy)?.as_deref(), Some("EXT-1"));
            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn disabling_generator_deletes_managed_children_and_config() {
    let (store, root, list_id) = setup_store_with_list();
    let generator_id = create_node_in(&store, list_id, None, "Generator");
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: generator_id,
            capabilities: vec![Capability::Generator],
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::SetGeneratorConfig {
            node_id: generator_id,
            data_source_type: "mock".into(),
            config_json: "{}".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let managed_id =
        create_managed_node_for_test(&store, list_id, generator_id, generator_id, "EXT-1", "Item");

    store
        .enqueue_outline(OutlineMutation::DisableCapability {
            node_id: generator_id,
            capability: Capability::Generator,
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let gen_repo = crate::outline::repos::GeneratorRepo::new(conn);
            assert!(gen_repo.get_config(generator_id).unwrap().is_none());
            assert!(gen_repo.get_link(managed_id).unwrap().is_none());
            let node = crate::outline::repos::NodeRepo::new(conn)
                .get(managed_id)
                .unwrap();
            assert!(node.is_none(), "managed node should be deleted on disable");
            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn deleting_generator_node_cascades_managed_cleanup() {
    let (store, root, list_id) = setup_store_with_list();
    let generator_id = create_node_in(&store, list_id, None, "Generator");
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: generator_id,
            capabilities: vec![Capability::Generator],
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::SetGeneratorConfig {
            node_id: generator_id,
            data_source_type: "mock".into(),
            config_json: "{}".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let managed_id =
        create_managed_node_for_test(&store, list_id, generator_id, generator_id, "EXT-1", "Item");

    store
        .enqueue_outline(OutlineMutation::DeleteNode {
            node_id: generator_id,
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let node_repo = crate::outline::repos::NodeRepo::new(conn);
            assert!(node_repo.get(generator_id).unwrap().is_none());
            assert!(node_repo.get(managed_id).unwrap().is_none());
            let gen_repo = crate::outline::repos::GeneratorRepo::new(conn);
            assert!(gen_repo.get_config(generator_id).unwrap().is_none());
            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn re_enabling_generator_after_disable_starts_with_no_config() {
    let (store, root, list_id) = setup_store_with_list();
    let generator_id = create_node_in(&store, list_id, None, "Generator");
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: generator_id,
            capabilities: vec![Capability::Generator],
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::SetGeneratorConfig {
            node_id: generator_id,
            data_source_type: "mock".into(),
            config_json: r#"{"query":"old"}"#.into(),
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .enqueue_outline(OutlineMutation::DisableCapability {
            node_id: generator_id,
            capability: Capability::Generator,
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: generator_id,
            capabilities: vec![Capability::Generator],
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let config = crate::outline::repos::GeneratorRepo::new(conn)
                .get_config(generator_id)
                .unwrap();
            assert!(
                config.is_none(),
                "re-enabled generator must not retain old config"
            );
            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn flatten_visible_reports_managed_and_generator_status() {
    let (store, root, list_id) = setup_store_with_list();
    let generator_id = create_node_in(&store, list_id, None, "Generator");
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: generator_id,
            capabilities: vec![Capability::Generator],
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::SetGeneratorConfig {
            node_id: generator_id,
            data_source_type: "mock".into(),
            config_json: "{}".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let managed_id =
        create_managed_node_for_test(&store, list_id, generator_id, generator_id, "EXT-1", "Item");
    store
        .enqueue_outline(OutlineMutation::SetRefreshStatus {
            node_id: generator_id,
            status: "error".into(),
            error: Some("boom".into()),
        })
        .unwrap();
    store.writer().flush().unwrap();

    let rows = store.flatten_outline(list_id).unwrap();
    let generator_row = rows.iter().find(|r| r.node.id == generator_id).unwrap();
    assert!(!generator_row.managed);
    assert_eq!(generator_row.managed_count, Some(1));
    assert_eq!(generator_row.generator_status.as_deref(), Some("error"));
    assert_eq!(generator_row.generator_error.as_deref(), Some("boom"));

    let managed_row = rows.iter().find(|r| r.node.id == managed_id).unwrap();
    assert!(managed_row.managed);
    assert_eq!(managed_row.external_id.as_deref(), Some("EXT-1"));
    assert_eq!(managed_row.managed_count, None);

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn editing_title_on_linked_node_marks_it_dirty() {
    let (store, root, list_id) = setup_store_with_list();
    let outside_parent = create_node_in(&store, list_id, None, "Outside");
    let node_id = create_node_in(&store, list_id, Some(outside_parent), "Original title");
    set_ticket(&store, node_id, "EXT-9");

    store
        .enqueue_outline(OutlineMutation::UpdateNodeTitle {
            node_id,
            title: "Edited title".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let fields = crate::outline::repos::GeneratorRepo::new(conn).user_modified_fields(node_id)?;
            assert_eq!(fields, vec!["title".to_string()]);
            Ok(())
        })
        .unwrap();

    // Editing again does not duplicate the entry.
    store
        .enqueue_outline(OutlineMutation::UpdateNodeTitle {
            node_id,
            title: "Edited again".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .read(|conn| {
            let fields = crate::outline::repos::GeneratorRepo::new(conn).user_modified_fields(node_id)?;
            assert_eq!(fields, vec!["title".to_string()]);
            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn editing_body_on_linked_node_marks_it_dirty_independently_of_title() {
    let (store, root, list_id) = setup_store_with_list();
    let node_id = create_node_in(&store, list_id, None, "Node");
    set_ticket(&store, node_id, "EXT-10");

    store
        .enqueue_outline(OutlineMutation::SetExtraContent {
            node_id,
            content_type: crate::outline::types::EXTRA_CONTENT_DETAILS.into(),
            body: "user-written body".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let fields = crate::outline::repos::GeneratorRepo::new(conn).user_modified_fields(node_id)?;
            assert_eq!(fields, vec!["body".to_string()]);
            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn copying_out_a_managed_node_greys_out_the_original_and_clears_on_delete() {
    let (store, root, list_id) = setup_store_with_list();
    let (generator_id, managed_id, _child_id) = setup_generator_with_managed_tree(&store, list_id);
    let outside_parent = create_node_in(&store, list_id, None, "Outside");

    assert!(!tree_row(&store, list_id, managed_id).has_copies);
    let managed_count = tree_row(&store, list_id, generator_id).managed_count;

    store
        .enqueue_outline(OutlineMutation::PasteManagedNodeCopy {
            source_node_id: managed_id,
            list_id,
            parent_id: Some(outside_parent),
            ordinal: 0,
        })
        .unwrap();
    store.writer().flush().unwrap();

    let copy_id = store
        .read(|conn| {
            let outline = crate::outline::repos::OutlineRepo::new(conn);
            let copy_id = outline
                .list_for_list(list_id)
                .unwrap()
                .into_iter()
                .find(|e| e.parent_id == Some(outside_parent))
                .unwrap()
                .node_id;
            Ok(copy_id)
        })
        .unwrap();
    let original = tree_row(&store, list_id, managed_id);
    assert!(
        original.has_copies && !original.linked_copy,
        "original should show as copied once a copy exists"
    );
    let copy = tree_row(&store, list_id, copy_id);
    assert!(copy.linked_copy && !copy.has_copies);
    assert_eq!(
        tree_row(&store, list_id, generator_id).managed_count,
        managed_count,
        "a copied-out node is not counted as one the generator manages"
    );

    store
        .enqueue_outline(OutlineMutation::DeleteNode { node_id: copy_id })
        .unwrap();
    store.writer().flush().unwrap();

    assert!(
        !tree_row(&store, list_id, managed_id).has_copies,
        "copied state should clear immediately once the last copy is deleted"
    );

    drop(store);
    let _ = fs::remove_dir_all(root);
}

/// A node is a ticket by having it, however it was made: one given the
/// ticket by hand counts as accepted, and is a linked copy that refreshes
/// reach, just as a pasted copy is.
#[test]
fn a_node_given_a_generated_ticket_by_hand_is_accepted() {
    let (store, root, list_id) = setup_store_with_list();
    let generator = create_node_in(&store, list_id, None, "Generator");
    let managed = create_managed_node_for_test(&store, list_id, generator, generator, "ENG-7", "Item");
    let by_hand = create_node_in(&store, list_id, None, "By hand");
    assert!(!tree_row(&store, list_id, managed).has_copies);

    set_ticket(&store, by_hand, "ENG-7");

    assert!(tree_row(&store, list_id, managed).has_copies);
    let row = tree_row(&store, list_id, by_hand);
    assert!(row.linked_copy && !row.managed);
    assert_eq!(row.ticket_id.as_deref(), Some("ENG-7"));
    store
        .read(|conn| {
            let generators = crate::outline::repos::GeneratorRepo::new(conn);
            assert!(generators.is_accepted("ENG-7")?);
            let holders: Vec<_> = generators.holders_of("ENG-7")?.into_iter().map(|h| h.node_id).collect();
            assert_eq!(holders, vec![by_hand]);
            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn two_generators_sharing_an_external_id_both_grey_out_when_one_is_copied() {
    let (store, root, list_id) = setup_store_with_list();
    let gen_a = create_node_in(&store, list_id, None, "Generator A");
    let gen_b = create_node_in(&store, list_id, None, "Generator B");
    let managed_a =
        create_managed_node_for_test(&store, list_id, gen_a, gen_a, "SHARED-1", "Item A");
    let managed_b =
        create_managed_node_for_test(&store, list_id, gen_b, gen_b, "SHARED-1", "Item B");
    let outside_parent = create_node_in(&store, list_id, None, "Outside");

    store
        .enqueue_outline(OutlineMutation::PasteManagedNodeCopy {
            source_node_id: managed_a,
            list_id,
            parent_id: Some(outside_parent),
            ordinal: 0,
        })
        .unwrap();
    store.writer().flush().unwrap();

    assert!(tree_row(&store, list_id, managed_a).has_copies);
    assert!(
        tree_row(&store, list_id, managed_b).has_copies,
        "sibling generator sharing the external id also shows as copied"
    );
    let copy_id = store
        .read(|conn| {
            let outline = crate::outline::repos::OutlineRepo::new(conn);
            let copy_id = outline
                .list_for_list(list_id)
                .unwrap()
                .into_iter()
                .find(|e| e.parent_id == Some(outside_parent))
                .unwrap()
                .node_id;
            Ok(copy_id)
        })
        .unwrap();

    store
        .enqueue_outline(OutlineMutation::DeleteNode { node_id: copy_id })
        .unwrap();
    store.writer().flush().unwrap();

    assert!(!tree_row(&store, list_id, managed_a).has_copies);
    assert!(!tree_row(&store, list_id, managed_b).has_copies);

    drop(store);
    let _ = fs::remove_dir_all(root);
}

/// The flattened tree row for `node_id`, which must be visible.
fn tree_row(store: &FleetStore, list_id: Uuid, node_id: Uuid) -> crate::outline::FlatNodeRow {
    store
        .flatten_outline(list_id)
        .unwrap()
        .into_iter()
        .find(|r| r.node.id == node_id)
        .expect("row visible in the flattened tree")
}

#[test]
fn deleting_generator_keeps_copies_their_titles_and_tickets() {
    let (store, root, list_id) = setup_store_with_list();
    let (generator_id, managed_id, _child_id) = setup_generator_with_managed_tree(&store, list_id);
    let outside_parent = create_node_in(&store, list_id, None, "Outside");

    store
        .enqueue_outline(OutlineMutation::PasteManagedNodeCopy {
            source_node_id: managed_id,
            list_id,
            parent_id: Some(outside_parent),
            ordinal: 0,
        })
        .unwrap();
    store.writer().flush().unwrap();

    let copy_id = store
        .read(|conn| {
            let outline = crate::outline::repos::OutlineRepo::new(conn);
            Ok(outline
                .list_for_list(list_id)
                .unwrap()
                .into_iter()
                .find(|e| e.parent_id == Some(outside_parent))
                .unwrap()
                .node_id)
        })
        .unwrap();

    store
        .enqueue_outline(OutlineMutation::DeleteNode {
            node_id: generator_id,
        })
        .unwrap();
    store.writer().flush().unwrap();

    assert!(
        !tree_row(&store, list_id, copy_id).linked_copy,
        "no generator has the ticket any more"
    );
    store
        .read(|conn| {
            let gen_repo = crate::outline::repos::GeneratorRepo::new(conn);
            let node_repo = crate::outline::repos::NodeRepo::new(conn);
            // Nothing tied it to that generator: it is still the ticket,
            // and any generator that returns it reaches it.
            assert_eq!(node_repo.get_ticket_id(copy_id)?.as_deref(), Some("EXT-1"));
            let holders = gen_repo.holders_of("EXT-1")?;
            assert!(holders.iter().any(|h| h.node_id == copy_id));
            let copy_node = node_repo.get(copy_id).unwrap().unwrap();
            assert_eq!(
                copy_node.title, "EXT-1: Fix the bug",
                "copy retains its title"
            );
            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn generator_state_survives_store_restart() {
    let (store, root, list_id) = setup_store_with_list();
    let (generator_id, managed_id, child_id) = setup_generator_with_managed_tree(&store, list_id);
    let outside_parent = create_node_in(&store, list_id, None, "Outside");
    store
        .enqueue_outline(OutlineMutation::PasteManagedNodeCopy {
            source_node_id: managed_id,
            list_id,
            parent_id: Some(outside_parent),
            ordinal: 0,
        })
        .unwrap();
    store.writer().flush().unwrap();
    let copy_id = store
        .read(|conn| {
            let outline = crate::outline::repos::OutlineRepo::new(conn);
            Ok(outline
                .list_for_list(list_id)
                .unwrap()
                .into_iter()
                .find(|e| e.parent_id == Some(outside_parent))
                .unwrap()
                .node_id)
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::SetRefreshStatus {
            node_id: generator_id,
            status: "error".into(),
            error: Some("network unreachable".into()),
        })
        .unwrap();
    store.writer().flush().unwrap();

    // Simulate an app restart: drop the store and reopen against the same data root.
    drop(store);
    let store = FleetStore::open(&root).unwrap();

    store
        .read(|conn| {
            let gen_repo = crate::outline::repos::GeneratorRepo::new(conn);
            let node_repo = crate::outline::repos::NodeRepo::new(conn);

            let config = gen_repo.get_config(generator_id).unwrap().unwrap();
            assert_eq!(config.data_source_type, "mock");
            assert_eq!(config.last_refresh_status.as_deref(), Some("error"));
            assert_eq!(
                config.last_refresh_error.as_deref(),
                Some("network unreachable")
            );

            assert!(
                gen_repo.is_managed(managed_id).unwrap(),
                "managed node persists across restart"
            );
            assert!(gen_repo.is_managed(child_id).unwrap());
            assert!(node_repo.get(managed_id).unwrap().is_some());

            assert!(
                gen_repo.holders_of("EXT-1")?.iter().any(|h| h.node_id == copy_id),
                "copied-out node is still the ticket"
            );
            assert!(!gen_repo.is_managed(copy_id).unwrap());

            Ok(())
        })
        .unwrap();
    assert!(
        tree_row(&store, list_id, managed_id).has_copies,
        "copied state must be recomputed correctly on startup from persisted links"
    );

    drop(store);
    let _ = fs::remove_dir_all(root);
}

fn setup_generator_with_managed_tree(store: &FleetStore, list_id: Uuid) -> (Uuid, Uuid, Uuid) {
    let generator_id = create_node_in(store, list_id, None, "Generator");
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: generator_id,
            capabilities: vec![Capability::Generator],
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::SetGeneratorConfig {
            node_id: generator_id,
            data_source_type: "mock".into(),
            config_json: "{}".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();

    let parent_id = create_managed_node_for_test(
        store,
        list_id,
        generator_id,
        generator_id,
        "EXT-1",
        "Fix the bug",
    );
    let child_id = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateManagedNode {
            node_id: Some(child_id),
            list_id,
            parent_id,
            title: "Sub item".into(),
            external_id: "EXT-2".into(),
            tags: vec!["urgent".into()],
            body: "child body".into(),
            metadata: None,
        })
        .unwrap();
    store.writer().flush().unwrap();

    (generator_id, parent_id, child_id)
}

#[test]
fn paste_managed_node_copy_deep_copies_and_converts_to_normal() {
    let (store, root, list_id) = setup_store_with_list();
    let (generator_id, managed_id, managed_child_id) =
        setup_generator_with_managed_tree(&store, list_id);
    let outside_parent = create_node_in(&store, list_id, None, "Outside");

    store
        .enqueue_outline(OutlineMutation::PasteManagedNodeCopy {
            source_node_id: managed_id,
            list_id,
            parent_id: Some(outside_parent),
            ordinal: 0,
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let node_repo = crate::outline::repos::NodeRepo::new(conn);
            let gen_repo = crate::outline::repos::GeneratorRepo::new(conn);
            let outline = crate::outline::repos::OutlineRepo::new(conn);

            let all_entries = outline.list_for_list(list_id).unwrap();
            let entries: Vec<_> = all_entries
                .iter()
                .filter(|e| e.parent_id == Some(outside_parent))
                .collect();
            assert_eq!(
                entries.len(),
                1,
                "copy should be placed under the target parent"
            );
            let copy_id = entries[0].node_id;
            assert_ne!(
                copy_id, managed_id,
                "copy must be a new node, not the original"
            );

            let copy_node = node_repo.get(copy_id).unwrap().unwrap();
            assert_eq!(copy_node.title, "EXT-1: Fix the bug");
            assert!(
                !gen_repo.is_managed(copy_id).unwrap(),
                "copy must be a normal editable node"
            );
            // Linked by being the same ticket; the original's generator is
            // found from where the original sits.
            assert_eq!(node_repo.get_ticket_id(copy_id)?.as_deref(), Some("EXT-1"));
            assert!(gen_repo.user_modified_fields(copy_id)?.is_empty());
            assert!(gen_repo.get_link(copy_id)?.is_none(), "a copy is not managed");
            assert_eq!(gen_repo.get_link(managed_id)?.unwrap().generator_node_id, generator_id);

            let original = node_repo.get(managed_id).unwrap().unwrap();
            assert!(
                gen_repo.is_managed(managed_id).unwrap(),
                "original stays managed"
            );
            let _ = original;

            let copy_children: Vec<_> = all_entries
                .iter()
                .filter(|e| e.parent_id == Some(copy_id))
                .collect();
            assert_eq!(
                copy_children.len(),
                1,
                "managed descendants must be deep-copied"
            );
            let copy_child_id = copy_children[0].node_id;
            assert_ne!(copy_child_id, managed_child_id);
            assert!(!gen_repo.is_managed(copy_child_id).unwrap());
            let child_copy = node_repo.get(copy_child_id).unwrap().unwrap();
            assert_eq!(child_copy.title, "EXT-2: Sub item");
            assert_eq!(
                node_repo.get_tags(copy_child_id).unwrap(),
                vec!["urgent".to_string()]
            );
            assert_eq!(
                node_repo
                    .get_extra_content(copy_child_id, crate::outline::types::EXTRA_CONTENT_DETAILS)
                    .unwrap()
                    .as_deref(),
                Some("child body")
            );

            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn accept_generated_ticket_is_a_noop_without_a_configured_destination() {
    let (store, root, list_id) = setup_store_with_list();
    let (_generator_id, managed_id, _managed_child_id) =
        setup_generator_with_managed_tree(&store, list_id);

    let new_node_id = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::AcceptGeneratedTicket {
            source_node_id: managed_id,
            new_node_id,
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let node_repo = crate::outline::repos::NodeRepo::new(conn);
            assert!(
                node_repo.get(new_node_id).unwrap().is_none(),
                "no destination configured — accept must not create anything"
            );
            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn accept_generated_ticket_copies_to_destination_and_enables_capabilities() {
    let (store, root, list_id) = setup_store_with_list();
    let (generator_id, managed_id, managed_child_id) =
        setup_generator_with_managed_tree(&store, list_id);
    let destination = create_node_in(&store, list_id, None, "Accepted work");

    store
        .enqueue_outline(OutlineMutation::SetGeneratorAcceptConfig {
            node_id: generator_id,
            destination_node_id: Some(destination),
            capabilities: vec![Capability::Tags, Capability::Spec],
        })
        .unwrap();
    store.writer().flush().unwrap();

    let new_node_id = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::AcceptGeneratedTicket {
            source_node_id: managed_id,
            new_node_id,
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let node_repo = crate::outline::repos::NodeRepo::new(conn);
            let gen_repo = crate::outline::repos::GeneratorRepo::new(conn);
            let outline = crate::outline::repos::OutlineRepo::new(conn);

            let new_node = node_repo.get(new_node_id).unwrap().unwrap();
            assert_eq!(new_node.title, "EXT-1: Fix the bug");
            assert!(!gen_repo.is_managed(new_node_id).unwrap());

            let entries = outline.list_for_list(list_id).unwrap();
            let entry = entries
                .iter()
                .find(|e| e.node_id == new_node_id)
                .expect("new node must be placed in the outline");
            assert_eq!(entry.parent_id, Some(destination));

            let caps = node_repo.list_capabilities(new_node_id).unwrap();
            assert!(caps.contains(&Capability::Tags));
            assert!(caps.contains(&Capability::Spec));

            // The generator's managed original stays untouched.
            assert!(gen_repo.is_managed(managed_id).unwrap());

            let child_copy = entries
                .iter()
                .find(|e| e.parent_id == Some(new_node_id))
                .expect("managed descendants must be deep-copied");
            assert_ne!(child_copy.node_id, managed_child_id);

            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn paste_managed_node_copy_blocked_inside_generator_subtree() {
    let (store, root, list_id) = setup_store_with_list();
    let (_generator_id, managed_id, _managed_child_id) =
        setup_generator_with_managed_tree(&store, list_id);

    let result = store.enqueue_outline(OutlineMutation::PasteManagedNodeCopy {
        source_node_id: managed_id,
        list_id,
        parent_id: Some(managed_id),
        ordinal: 0,
    });
    if result.is_ok() {
        assert!(
            store.writer().flush().is_err(),
            "paste inside a generator subtree must be rejected"
        );
    }

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn restoring_a_disabled_generator_brings_back_its_managed_tree() {
    let (store, root, list_id) = setup_store_with_list();
    let (generator_id, parent_id, child_id) = setup_generator_with_managed_tree(&store, list_id);

    store
        .enqueue_outline(OutlineMutation::DisableCapability {
            node_id: generator_id,
            capability: Capability::Generator,
        })
        .unwrap();
    store.writer().flush().unwrap();
    let archive_id: Uuid = store
        .read(|conn| {
            assert!(crate::outline::repos::NodeRepo::new(conn).get(child_id)?.is_none());
            let id: Vec<u8> = conn.query_row(
                "SELECT id FROM capability_archives WHERE node_id = ?1",
                [generator_id.as_bytes().to_vec()],
                |row| row.get(0),
            )?;
            Ok(Uuid::from_slice(&id)?)
        })
        .unwrap();

    store
        .enqueue_outline(OutlineMutation::RestoreCapability {
            node_id: generator_id,
            capability: Capability::Generator,
            archive_id,
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let nodes = crate::outline::repos::NodeRepo::new(conn);
            let generators = crate::outline::repos::GeneratorRepo::new(conn);
            assert!(nodes.list_capabilities(generator_id)?.contains(&Capability::Generator));
            assert_eq!(generators.get_config(generator_id)?.unwrap().data_source_type, "mock");
            assert!(nodes.get(child_id)?.is_some() && nodes.get(parent_id)?.is_some());
            assert_eq!(generators.get_link(child_id)?.unwrap().external_id, "EXT-2");
            let archives: i64 =
                conn.query_row("SELECT COUNT(*) FROM capability_archives", [], |row| row.get(0))?;
            assert_eq!(archives, 0);
            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn plan_step_phases_order_dependencies_and_readiness() {
    use crate::outline::repos::PlanStepRepo;
    use crate::outline::repos::plan_steps::{PHASE_ACTIVE, PHASE_RELEASED, due_by};

    assert!(due_by("active", "verifying"));
    assert!(due_by("released", "released"));
    assert!(!due_by("released", "merged"));

    let root = std::env::temp_dir().join(format!("tod-plan-phase-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let store = FleetStore::open(&root).unwrap();
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "plan".into(),
            title: "Plan".into(),
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
            title: "Phased node".into(),
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
    let req = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateObligation {
            obligation_id: Some(req),
            node_id,
            kind: crate::outline::KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            section: None,
            body: "Backfilled".into(),
            phase: PHASE_REQUIREMENTS.into(),
        })
        .unwrap();
    let build = Uuid::new_v4();
    let backfill = Uuid::new_v4();
    for (id, body) in [(build, "Build the backfill"), (backfill, "Run the backfill")] {
        store
            .enqueue_outline(OutlineMutation::CreatePlanStep {
                step_id: Some(id),
                node_id,
                after_id: None,
                before: false,
                body: body.into(),
            })
            .unwrap();
    }
    store.writer().flush().unwrap();
    store
        .enqueue_outline(OutlineMutation::SetPlanStepPhase {
            step_id: backfill,
            phase: PHASE_RELEASED.into(),
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::AddPlanStepDependency {
            step_id: backfill,
            depends_on_step_id: build,
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::LinkPlanStepObligation {
            step_id: backfill,
            obligation_id: req,
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::LinkPlanStepObligation {
            step_id: build,
            obligation_id: req,
        })
        .unwrap();
    store.writer().flush().unwrap();

    // An earlier phase may not wait on a later one, either way round.
    let reverse = store.enqueue_outline(OutlineMutation::AddPlanStepDependency {
        step_id: build,
        depends_on_step_id: backfill,
    });
    assert!(reverse.is_err() || store.writer().flush().is_err());
    let set_phase = |step_id, phase: &str| {
        store
            .enqueue_outline(OutlineMutation::SetPlanStepPhase {
                step_id,
                phase: phase.into(),
            })
            .and_then(|_| store.writer().flush())
    };
    set_phase(backfill, "merged").unwrap();
    assert!(set_phase(build, PHASE_RELEASED).is_err(), "a merged step would wait on it");
    set_phase(build, "merged").unwrap();
    set_phase(build, PHASE_ACTIVE).unwrap();
    set_phase(backfill, PHASE_RELEASED).unwrap();
    let bogus = store.enqueue_outline(OutlineMutation::SetPlanStepPhase {
        step_id: build,
        phase: "review".into(),
    });
    assert!(bogus.is_err() || store.writer().flush().is_err());

    store
        .read(|conn| {
            let repo = PlanStepRepo::new(conn);
            let phase = |id| repo.get(id).unwrap().unwrap().phase;
            assert_eq!(phase(build), PHASE_ACTIVE);
            assert_eq!(phase(backfill), PHASE_RELEASED);
            // The obligation is delivered by the latest of its steps.
            assert_eq!(repo.obligation_phases(node_id).unwrap()[&req], PHASE_RELEASED);
            Ok(())
        })
        .unwrap();

    store
        .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
            step_id: build,
            status: "implemented".into(),
            note: None,
            reason: None,
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .read(|conn| {
            let repo = PlanStepRepo::new(conn);
            let ids = |state: Option<&str>| -> Vec<Uuid> {
                repo.ready_steps(node_id, state).unwrap()
            };
            assert!(ids(Some("active")).is_empty(), "the backfill waits for release");
            assert_eq!(ids(Some("released")), vec![backfill]);
            assert_eq!(ids(None), vec![backfill]);
            Ok(())
        })
        .unwrap();

    // Verifying the obligation, then taking the released step, does not
    // reopen it: that phase's action is not a reimplementation.
    store
        .writer()
        .execute_interview(
            "test",
            crate::interview::InterviewCommand::RecordObligationVerdict {
                node_id,
                obligation_id: req,
                conversation_id: None,
                status: "verified".into(),
                evidence: "Checked.".into(),
            },
        )
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
            step_id: backfill,
            status: "implemented".into(),
            note: None,
            reason: None,
        })
        .unwrap();
    store.writer().flush().unwrap();
    store
        .read(|conn| {
            let standings = crate::verification::VerdictRepo::new(conn).standings(node_id)?;
            assert!(standings[0].is_verified());
            assert_eq!(standings[0].phase, PHASE_RELEASED);
            Ok(())
        })
        .unwrap();
}
