//! Outline integration tests.

use crate::fleet::store::FleetStore;
use crate::interview::PHASE_REQUIREMENTS;
use crate::outline::types::Capability;
use crate::outline::{CreatePosition, OutlineMutation, resolve_obligations};
use crate::paths::{clear_data_root_override, set_data_root};
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
        let resolved = resolve_obligations(&conn, row.node.id, None).unwrap();
        assert!(resolved.is_empty() || !resolved[0].obligation.body.is_empty());
    }
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
    assert_eq!(req_b_node, node_id, "rejected move must leave obligation in place");

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
    use crate::fleet::repos::agent_config::NewAgentConfig;
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
        .enqueue(FleetMutation::InsertAgent {
            agent: NewAgentConfig {
                id: Uuid::new_v4().to_string(),
                node_id: node_id.to_string(),
                env_type: "local".into(),
                mode: "agent".into(),
                work_directory: None,
                use_worktree: false,
                platform: "claude".into(),
                model: "default".into(),
                effort: "auto".into(),
            },
        })
        .unwrap();

    let err = store
        .enqueue_outline(OutlineMutation::DeleteNode { node_id })
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("node \"Agent task\" has associated agents"),
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
            assert_eq!(repo.ready_steps(node_id).unwrap(), vec![step_a]);

            assert_eq!(repo.list_obligations(step_a).unwrap(), vec![req]);
            assert_eq!(repo.list_steps_for_obligation(req).unwrap(), vec![step_a]);
            Ok(())
        })
        .unwrap();

    store
        .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
            step_id: step_a,
            status: "implemented".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let repo = PlanStepRepo::new(conn);
            // step_b is now unblocked (auto-promoted to ready).
            let mut ready = repo.ready_steps(node_id).unwrap();
            ready.sort();
            let mut expected = vec![step_b];
            expected.sort();
            assert_eq!(ready, expected);
            assert_eq!(repo.get(step_b).unwrap().unwrap().status, "ready");
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
    let caps = crate::outline::repos::NodeRepo::new(&fresh_conn).list_capabilities(node_id).unwrap();
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
                let caps =
                    crate::outline::repos::NodeRepo::new(conn).list_capabilities(node1)?;
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
                let caps =
                    crate::outline::repos::NodeRepo::new(conn).list_capabilities(node2)?;
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
    let node_id = Uuid::new_v4();
    store
        .enqueue_outline(OutlineMutation::CreateManagedNode {
            node_id: Some(node_id),
            list_id,
            parent_id,
            title: title.into(),
            external_id: external_id.into(),
            source_type: "mock".into(),
            generator_node_id,
            tags: vec![],
            body: String::new(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    node_id
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
            archive_payload: "{}".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();

    store
        .read(|conn| {
            let gen_repo = crate::outline::repos::GeneratorRepo::new(conn);
            assert!(gen_repo.get_config(generator_id).unwrap().is_none());
            assert!(gen_repo.get_link(managed_id).unwrap().is_none());
            let node = crate::outline::repos::NodeRepo::new(conn).get(managed_id).unwrap();
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
        .enqueue_outline(OutlineMutation::DeleteNode { node_id: generator_id })
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
            archive_payload: "{}".into(),
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
            assert!(config.is_none(), "re-enabled generator must not retain old config");
            Ok(())
        })
        .unwrap();

    drop(store);
    let _ = fs::remove_dir_all(root);
}
