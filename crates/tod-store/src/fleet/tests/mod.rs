//! Phase 7 integration verification for fleet persistence.

use crate::fleet::launch::FleetLaunchError;
use crate::fleet::lock::FleetLockError;
use crate::fleet::reconnect_identity::ReconnectIdentity;
use crate::fleet::repos::agent_run::AgentRunRepo;
use crate::fleet::repos::notification::NotificationRepo;
use crate::fleet::repos::shell::ShellRepo;
use crate::fleet::repos::task::{FleetTask, NoteItem, TaskRepo};
use crate::fleet::schema;
use crate::fleet::store::FleetStore;
use crate::fleet::test_util::{cleanup_fleet_root, insert_scale_data, temp_fleet_root};
use crate::fleet::writer::{FleetMutation, FleetWriter};
use crate::outline::OutlineMutation;
use crate::outline::types::Capability;
use rusqlite::OptionalExtension;
use std::thread;
use std::time::Duration;

fn short_settle() {
    thread::sleep(Duration::from_millis(50));
}

fn read_task_title(db_path: &std::path::Path, id: &str) -> Option<String> {
    let conn = schema::open_read_connection(db_path).unwrap();
    let blob = uuid::Uuid::parse_str(id)
        .ok()
        .map(|u| u.as_bytes().to_vec())?;
    conn.query_row("SELECT title FROM nodes WHERE id = ?1", [blob], |row| {
        row.get(0)
    })
    .optional()
    .unwrap()
}

fn list_notifications(
    store: &FleetStore,
) -> Vec<crate::fleet::repos::notification::FleetNotification> {
    let projection = store.projection();
    let guard = projection.lock().unwrap();
    let conn = guard.connection();
    NotificationRepo::new(&conn).list_open().unwrap()
}

fn row_exists(conn: &rusqlite::Connection, table: &str, id: &str) -> bool {
    let count: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE id = ?1"),
            [id],
            |row| row.get(0),
        )
        .unwrap();
    count > 0
}

#[test]
fn external_edit_reloads_fleet_store_projection() {
    let root = temp_fleet_root();
    let store = FleetStore::open(&root).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    store
        .enqueue(FleetMutation::InsertTask {
            task: FleetTask::new(&id, "Baseline", "baseline"),
        })
        .unwrap();
    store.writer().flush().unwrap();
    assert_eq!(store.list_tasks().unwrap().len(), 1);

    let db_path = store.paths().db().to_path_buf();
    {
        let conn = schema::open_writer_connection(&db_path).unwrap();
        let node_id = uuid::Uuid::new_v4();
        let blob = node_id.as_bytes().to_vec();
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO nodes (id, slug, title, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            rusqlite::params![blob, "external", "External", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO node_capabilities (node_id, capability, enabled_at) VALUES (?1, 'agent', ?2)",
            rusqlite::params![blob, now],
        )
        .unwrap();
    }

    assert_eq!(store.projection().lock().unwrap().metadata().task_count, 1);
    assert!(store.reload_if_stale().unwrap());
    assert_eq!(store.list_tasks().unwrap().len(), 2);

    drop(store);
    cleanup_fleet_root(&root);
}

#[test]
fn debounced_mutations_lost_when_writer_abandoned() {
    let root = temp_fleet_root();
    let db_path = root.join("tod.db");
    schema::open_writer_connection(&db_path).unwrap();

    let writer = FleetWriter::open_with_debounce(
        &db_path,
        Duration::from_secs(3600),
        crate::fleet::command_log::CommandLog::shared(),
    )
    .unwrap();
    let task_id = uuid::Uuid::new_v4().to_string();
    writer
        .enqueue(FleetMutation::InsertTask {
            task: FleetTask::new(&task_id, "Baseline", "baseline"),
        })
        .unwrap();
    writer.flush().unwrap();
    assert_eq!(
        read_task_title(&db_path, &task_id).as_deref(),
        Some("Baseline")
    );

    writer
        .enqueue(FleetMutation::UpdateTaskTitle {
            id: task_id.clone(),
            title: "Lost edit".into(),
        })
        .unwrap();
    writer.abandon_without_flush();

    assert_eq!(
        read_task_title(&db_path, &task_id).as_deref(),
        Some("Baseline")
    );

    let reopened = FleetWriter::open(&db_path).unwrap();
    reopened.flush().unwrap();
    assert_eq!(
        read_task_title(&db_path, &task_id).as_deref(),
        Some("Baseline")
    );
    reopened.shutdown().unwrap();
    cleanup_fleet_root(&root);
}

#[test]
fn scale_generator_inserts_tasks_and_runs() {
    let root = temp_fleet_root();
    let db_path = root.join("tod.db");
    let conn = schema::open_writer_connection(&db_path).unwrap();
    let snapshot = insert_scale_data(&conn);
    drop(conn);

    let store = FleetStore::open(&root).unwrap();
    let meta = store.projection().lock().unwrap().metadata().clone();
    assert_eq!(meta.task_count, snapshot.task_count);
    assert_eq!(meta.run_count, snapshot.run_count);

    let tasks = store.list_tasks().unwrap();
    assert_eq!(tasks.len(), snapshot.task_count);
    assert!(tasks.iter().any(|t| t.slug == "scale-task-0"));
    assert!(tasks.iter().any(|t| t.slug == "scale-task-499"));

    let projection = store.projection();
    let guard = projection.lock().unwrap();
    let run_total: usize = guard
        .connection()
        .query_row("SELECT COUNT(*) FROM agent_runs", [], |row| {
            row.get::<_, i64>(0).map(|n| n as usize)
        })
        .unwrap();
    assert_eq!(run_total, snapshot.run_count);

    drop(store);
    cleanup_fleet_root(&root);
}

#[test]
fn second_fleet_store_open_rejected_while_lock_held() {
    let root = temp_fleet_root();
    let store1 = FleetStore::open(&root).unwrap();

    let err = match FleetStore::open(&root) {
        Err(err) => err,
        Ok(_) => panic!("expected second FleetStore::open to fail while lock held"),
    };
    match err {
        FleetLaunchError::Other(inner) => {
            assert!(
                inner.downcast_ref::<FleetLockError>().is_some()
                    || inner.to_string().contains("lock")
            );
        }
        other => panic!("expected lock error, got {other:?}"),
    }

    drop(store1);
    FleetStore::open(&root).unwrap();
    cleanup_fleet_root(&root);
}

#[test]
fn immediate_mutation_categories_persist_without_debounce_wait() {
    let root = temp_fleet_root();
    let db_path = root.join("tod.db");
    schema::open_writer_connection(&db_path).unwrap();

    let writer = FleetWriter::open_with_debounce(
        &db_path,
        Duration::from_secs(60),
        crate::fleet::command_log::CommandLog::shared(),
    )
    .unwrap();

    let task_id = uuid::Uuid::new_v4().to_string();
    let run_id = format!("{task_id}-run-1");
    let notification_id = uuid::Uuid::new_v4().to_string();
    let blocked_notification_id = uuid::Uuid::new_v4().to_string();
    let shell_id = uuid::Uuid::new_v4().to_string();
    let identity = ReconnectIdentity {
        pid: std::process::id(),
        birth_token: 42,
    };
    let get_run = |id: &str| {
        let conn = schema::open_read_connection(&db_path).unwrap();
        AgentRunRepo::new(&conn).get(id).unwrap()
    };

    writer
        .enqueue(FleetMutation::InsertTask {
            task: FleetTask::new(&task_id, "Immediate suite", "immediate-suite"),
        })
        .unwrap();
    writer.flush().unwrap();

    writer
        .enqueue(FleetMutation::CreateAgentRun {
            node_id: task_id.clone(),
            run_kind: None,
            session_name: None,
            launch: None,
        })
        .unwrap();
    short_settle();
    assert!(get_run(&run_id).is_some());

    writer
        .enqueue(FleetMutation::UpdateAgentRunRuntimeStatus {
            run_id: run_id.clone(),
            runtime_status: "processing".into(),
        })
        .unwrap();
    short_settle();
    assert_eq!(get_run(&run_id).unwrap().runtime_status, "processing");

    writer
        .enqueue(FleetMutation::UpdateAgentRunReconnect {
            run_id: run_id.clone(),
            identity,
        })
        .unwrap();
    short_settle();
    assert_eq!(get_run(&run_id).unwrap().reconnect, Some(identity));

    writer
        .enqueue(FleetMutation::CreateNotification {
            id: notification_id.clone(),
            message: "open".into(),
            related_task_id: Some(task_id.clone()),
            related_run_ids: vec![run_id.clone()],
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert!(
            NotificationRepo::new(&conn)
                .get(&notification_id)
                .unwrap()
                .is_some()
        );
    }

    writer
        .enqueue(FleetMutation::CreateBlockedNotification {
            id: blocked_notification_id.clone(),
            message: "blocked".into(),
            related_task_id: Some(task_id.clone()),
            run_id: run_id.clone(),
        })
        .unwrap();
    short_settle();
    assert_eq!(get_run(&run_id).unwrap().runtime_status, "blocked");

    writer
        .enqueue(FleetMutation::CreateShellSession {
            id: shell_id.clone(),
            node_id: task_id.clone(),
            reconnect: Some(identity),
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert!(
            ShellRepo::new(&conn)
                .list_for_node(&task_id)
                .unwrap()
                .iter()
                .any(|session| session.id == shell_id)
        );
    }

    writer
        .enqueue(FleetMutation::ClearShellReconnect {
            id: shell_id.clone(),
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        let session = ShellRepo::new(&conn)
            .list_for_node(&task_id)
            .unwrap()
            .into_iter()
            .find(|session| session.id == shell_id)
            .expect("shell session exists");
        assert!(session.reconnect.is_none());
    }

    writer
        .enqueue(FleetMutation::ResolveNotification {
            id: notification_id.clone(),
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert!(!row_exists(&conn, "notifications", &notification_id));
    }

    writer
        .enqueue(FleetMutation::DismissShellSession {
            id: shell_id.clone(),
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert!(!row_exists(&conn, "shell_sessions", &shell_id));
    }

    writer
        .enqueue(FleetMutation::ClearAgentRunReconnect {
            run_id: run_id.clone(),
        })
        .unwrap();
    writer
        .enqueue(FleetMutation::DeleteAgentRun {
            run_id: run_id.clone(),
        })
        .unwrap();
    short_settle();
    assert!(get_run(&run_id).is_none());

    writer
        .enqueue(FleetMutation::DeleteTask {
            id: task_id.clone(),
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert!(TaskRepo::new(&conn).get(&task_id).unwrap().is_none());
    }

    writer.shutdown().unwrap();
    cleanup_fleet_root(&root);
}

#[test]
fn task_round_trip_survives_store_close_and_reopen() {
    let root = temp_fleet_root();
    let task = FleetTask {
        id: uuid::Uuid::new_v4().to_string(),
        title: "Quit-sim task".into(),
        slug: "quit-sim-task".into(),
        lifecycle: "active".into(),
        repo: Some("github.com/org/tod".into()),
        branch: Some("main".into()),
        notes: vec![NoteItem::new("persist me")],
        tags: vec!["ui".into(), "persistence".into()],
        linked_issues: vec!["TOD-99".into()],
        linked_prs: vec!["#7".into()],
    };

    {
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue(FleetMutation::InsertTask { task: task.clone() })
            .unwrap();
        store.writer().flush().unwrap();
    }

    let store = FleetStore::open(&root).unwrap();
    let loaded = store
        .list_tasks()
        .unwrap()
        .into_iter()
        .find(|t| t.id == task.id)
        .expect("task restored");
    assert_eq!(loaded, task);

    drop(store);
    cleanup_fleet_root(&root);
}

#[test]
fn notification_round_trip_and_resolve_absent_after_reopen() {
    let root = temp_fleet_root();
    let task_id = uuid::Uuid::new_v4().to_string();
    let run_id = format!("{task_id}-run-1");
    let notification_id = uuid::Uuid::new_v4().to_string();

    {
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue(FleetMutation::InsertTask {
                task: FleetTask::new(&task_id, "Notify", "notify"),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store
            .enqueue(FleetMutation::CreateAgentRun {
                node_id: task_id.clone(),
                run_kind: None,
                session_name: None,
                launch: None,
            })
            .unwrap();
        store
            .enqueue(FleetMutation::CreateNotification {
                id: notification_id.clone(),
                message: "needs review".into(),
                related_task_id: None,
                related_run_ids: vec![run_id.clone()],
            })
            .unwrap();
        store.writer().wait_for_idle().unwrap();
    }

    {
        let store = FleetStore::open(&root).unwrap();
        let open = list_notifications(&store);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, notification_id);
        assert_eq!(open[0].message, "needs review");
        assert_eq!(open[0].related_run_ids, vec![run_id.clone()]);

        store
            .enqueue(FleetMutation::ResolveNotification {
                id: notification_id.clone(),
            })
            .unwrap();
        store.writer().wait_for_idle().unwrap();
    }

    {
        let store = FleetStore::open(&root).unwrap();
        assert!(list_notifications(&store).is_empty());
    }

    cleanup_fleet_root(&root);
}

#[test]
fn capability_disable_blockers_reflect_running_work() {
    let root = temp_fleet_root();
    let store = FleetStore::open(&root).unwrap();
    let task_id = uuid::Uuid::new_v4().to_string();
    let node_uuid = uuid::Uuid::parse_str(&task_id).unwrap();

    store
        .enqueue(FleetMutation::InsertTask {
            task: FleetTask::new(&task_id, "Blocker check", "blocker-check"),
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: node_uuid,
            capabilities: vec![Capability::Files],
        })
        .unwrap();
    store.writer().flush().unwrap();

    // Nothing running — nothing to block on.
    for cap in [Capability::Agent, Capability::Files] {
        assert!(store.capability_disable_blocker(&task_id, cap).unwrap().is_none());
    }

    // A live run blocks disabling Agent, not Files.
    let run_id = format!("{task_id}-run-1");
    store
        .enqueue(FleetMutation::CreateAgentRun {
            node_id: task_id.clone(),
            run_kind: None,
            session_name: None,
            launch: None,
        })
        .unwrap();
    store.writer().flush().unwrap();
    let reason = store
        .capability_disable_blocker(&task_id, Capability::Agent)
        .unwrap()
        .expect("live run should block disabling Agent");
    assert!(reason.contains("running"));
    assert!(
        store
            .capability_disable_blocker(&task_id, Capability::Files)
            .unwrap()
            .is_none()
    );
    store
        .enqueue(FleetMutation::EndAgentRun {
            run_id: run_id.clone(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    assert!(
        store
            .capability_disable_blocker(&task_id, Capability::Agent)
            .unwrap()
            .is_none()
    );

    // An open shell blocks disabling Files.
    let shell_id = uuid::Uuid::new_v4().to_string();
    store
        .enqueue(FleetMutation::CreateShellSession {
            id: shell_id.clone(),
            node_id: task_id.clone(),
            reconnect: None,
        })
        .unwrap();
    store.writer().flush().unwrap();
    let reason = store
        .capability_disable_blocker(&task_id, Capability::Files)
        .unwrap()
        .expect("open shell should block disabling Files");
    assert!(reason.contains("shell"));
    store
        .enqueue(FleetMutation::DismissShellSession { id: shell_id })
        .unwrap();

    // So does a set-up worktree.
    store
        .enqueue(FleetMutation::UpdateNodeWorktree {
            node_id: task_id.clone(),
            worktree_path: Some("/wt/blocker".into()),
            worktree_lease_id: None,
            worktree_lease_holder: None,
        })
        .unwrap();
    store.writer().flush().unwrap();
    let reason = store
        .capability_disable_blocker(&task_id, Capability::Files)
        .unwrap()
        .expect("set-up worktree should block disabling Files");
    assert!(reason.contains("worktree"));

    store
        .enqueue(FleetMutation::UpdateNodeWorktree {
            node_id: task_id.clone(),
            worktree_path: None,
            worktree_lease_id: None,
            worktree_lease_holder: None,
        })
        .unwrap();
    store.writer().flush().unwrap();
    assert!(
        store
            .capability_disable_blocker(&task_id, Capability::Files)
            .unwrap()
            .is_none()
    );

    drop(store);
    cleanup_fleet_root(&root);
}

#[test]
fn worktree_release_blocker_reflects_running_shells_and_agents() {
    let root = temp_fleet_root();
    let store = FleetStore::open(&root).unwrap();
    let task_id = uuid::Uuid::new_v4().to_string();
    let node_uuid = uuid::Uuid::parse_str(&task_id).unwrap();

    store
        .enqueue(FleetMutation::InsertTask {
            task: FleetTask::new(&task_id, "Release check", "release-check"),
        })
        .unwrap();
    store
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: node_uuid,
            capabilities: vec![Capability::Files],
        })
        .unwrap();
    store.writer().flush().unwrap();
    assert!(store.worktree_release_blocker(&task_id).unwrap().is_none());

    // A shell that's no longer running doesn't block; a running one does.
    store
        .enqueue(FleetMutation::CreateShellSession {
            id: uuid::Uuid::new_v4().to_string(),
            node_id: task_id.clone(),
            reconnect: None,
        })
        .unwrap();
    store.writer().flush().unwrap();
    assert!(store.worktree_release_blocker(&task_id).unwrap().is_none());

    let live_shell = uuid::Uuid::new_v4().to_string();
    store
        .enqueue(FleetMutation::CreateShellSession {
            id: live_shell.clone(),
            node_id: task_id.clone(),
            reconnect: crate::fleet::reconnect_identity::record(std::process::id()),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let reason = store
        .worktree_release_blocker(&task_id)
        .unwrap()
        .expect("running shell should block release");
    assert!(reason.contains("1 shell"));
    store
        .enqueue(FleetMutation::DismissShellSession { id: live_shell })
        .unwrap();
    store.writer().flush().unwrap();
    assert!(store.worktree_release_blocker(&task_id).unwrap().is_none());

    // An agent at work blocks until it ends.
    let run_id = format!("{task_id}-run-1");
    store
        .enqueue(FleetMutation::CreateAgentRun {
            node_id: task_id.clone(),
            run_kind: None,
            session_name: None,
            launch: None,
        })
        .unwrap();
    store
        .enqueue(FleetMutation::UpdateAgentRunRuntimeStatus {
            run_id: run_id.clone(),
            runtime_status: "processing".into(),
        })
        .unwrap();
    store.writer().flush().unwrap();
    let reason = store
        .worktree_release_blocker(&task_id)
        .unwrap()
        .expect("working agent should block release");
    assert!(reason.contains("1 agent"));
    store
        .enqueue(FleetMutation::EndAgentRun { run_id })
        .unwrap();
    store.writer().flush().unwrap();
    assert!(store.worktree_release_blocker(&task_id).unwrap().is_none());

    drop(store);
    cleanup_fleet_root(&root);
}
