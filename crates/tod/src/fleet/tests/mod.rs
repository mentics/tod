//! Phase 7 integration verification for fleet persistence.

use crate::fleet::launch::FleetLaunchError;
use crate::fleet::lock::FleetLockError;
use crate::fleet::reconnect_identity::ReconnectIdentity;
use crate::fleet::repos::agent::{AgentRepo, NewAgent};
use crate::fleet::repos::notification::NotificationRepo;
use crate::fleet::repos::shell::ShellRepo;
use crate::fleet::repos::task::{FleetTask, TaskRepo};
use crate::fleet::schema;
use crate::fleet::store::FleetStore;
use crate::fleet::test_util::{cleanup_fleet_root, insert_scale_data, temp_fleet_root};
use crate::fleet::writer::{FleetMutation, FleetWriter};
use rusqlite::OptionalExtension;
use std::thread;
use std::time::Duration;

fn short_settle() {
    thread::sleep(Duration::from_millis(50));
}

fn read_task_title(db_path: &std::path::Path, id: &str) -> Option<String> {
    let conn = schema::open_read_connection(db_path).unwrap();
    conn.query_row(
        "SELECT title FROM tasks WHERE id = ?1",
        [id],
        |row| row.get(0),
    )
    .optional()
    .unwrap()
}

fn list_notifications(store: &FleetStore) -> Vec<crate::fleet::repos::notification::FleetNotification> {
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
        conn.execute(
            "INSERT INTO tasks (id, title, slug, lifecycle) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                "External",
                "external",
                "proposed"
            ],
        )
        .unwrap();
    }

    assert_eq!(
        store
            .projection()
            .lock()
            .unwrap()
            .metadata()
            .task_count,
        1
    );
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

    let writer =
        FleetWriter::open_with_debounce(&db_path, Duration::from_secs(3600)).unwrap();
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
fn scale_generator_inserts_tasks_and_agents() {
    let root = temp_fleet_root();
    let db_path = root.join("tod.db");
    let conn = schema::open_writer_connection(&db_path).unwrap();
    let snapshot = insert_scale_data(&conn);
    drop(conn);

    let store = FleetStore::open(&root).unwrap();
    let meta = store
        .projection()
        .lock()
        .unwrap()
        .metadata()
        .clone();
    assert_eq!(meta.task_count, snapshot.task_count);
    assert_eq!(meta.agent_count, snapshot.agent_count);

    let tasks = store.list_tasks().unwrap();
    assert_eq!(tasks.len(), snapshot.task_count);
    assert!(tasks.iter().any(|t| t.id == "scale-task-0000"));
    assert!(tasks.iter().any(|t| t.id == "scale-task-0499"));

    let projection = store.projection();
    let guard = projection.lock().unwrap();
    let agent_total: usize = guard
        .connection()
        .query_row("SELECT COUNT(*) FROM agents", [], |row| {
            row.get::<_, i64>(0).map(|n| n as usize)
        })
        .unwrap();
    assert_eq!(agent_total, snapshot.agent_count);

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

    let writer =
        FleetWriter::open_with_debounce(&db_path, Duration::from_secs(60)).unwrap();

    let task_id = uuid::Uuid::new_v4().to_string();
    let agent_id = uuid::Uuid::new_v4().to_string();
    let prompt_id = uuid::Uuid::new_v4().to_string();
    let response_id = uuid::Uuid::new_v4().to_string();
    let notification_id = uuid::Uuid::new_v4().to_string();
    let blocked_notification_id = uuid::Uuid::new_v4().to_string();
    let shell_id = uuid::Uuid::new_v4().to_string();
    let identity = ReconnectIdentity {
        pid: std::process::id(),
        birth_token: 42,
    };

    writer
        .enqueue(FleetMutation::InsertTask {
            task: FleetTask::new(&task_id, "Immediate suite", "immediate-suite"),
        })
        .unwrap();
    writer.flush().unwrap();

    writer
        .enqueue(FleetMutation::InsertAgent {
            agent: NewAgent {
                id: agent_id.clone(),
                task_id: task_id.clone(),
                env_type: "local".into(),
                mode: "agent".into(),
            },
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert!(AgentRepo::new(&conn).get(&agent_id).unwrap().is_some());
    }

    writer
        .enqueue(FleetMutation::UpdateAgentRuntimeStatus {
            id: agent_id.clone(),
            runtime_status: "waiting".into(),
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert_eq!(
            AgentRepo::new(&conn)
                .get(&agent_id)
                .unwrap()
                .unwrap()
                .runtime_status,
            "waiting"
        );
    }

    writer
        .enqueue(FleetMutation::UpdateAgentReconnect {
            id: agent_id.clone(),
            identity,
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert_eq!(
            AgentRepo::new(&conn)
                .get(&agent_id)
                .unwrap()
                .unwrap()
                .reconnect,
            Some(identity)
        );
    }

    writer
        .enqueue(FleetMutation::SendPrompt {
            id: prompt_id.clone(),
            agent_id: agent_id.clone(),
            content: "hello".into(),
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert!(row_exists(&conn, "transcript_turns", &prompt_id));
    }

    writer
        .enqueue(FleetMutation::CompleteResponse {
            response_id: response_id.clone(),
            agent_id: agent_id.clone(),
            content: "world".into(),
            prompt_id: prompt_id.clone(),
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert!(row_exists(&conn, "transcript_turns", &response_id));
    }

    writer
        .enqueue(FleetMutation::CreateNotification {
            id: notification_id.clone(),
            message: "open".into(),
            related_task_id: Some(task_id.clone()),
            related_agent_ids: vec![agent_id.clone()],
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert!(NotificationRepo::new(&conn)
            .get(&notification_id)
            .unwrap()
            .is_some());
    }

    writer
        .enqueue(FleetMutation::CreateBlockedNotification {
            id: blocked_notification_id.clone(),
            message: "blocked".into(),
            related_task_id: Some(task_id.clone()),
            agent_id: agent_id.clone(),
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert_eq!(
            AgentRepo::new(&conn)
                .get(&agent_id)
                .unwrap()
                .unwrap()
                .runtime_status,
            "blocked"
        );
    }

    writer
        .enqueue(FleetMutation::CreateShellSession {
            id: shell_id.clone(),
            agent_id: agent_id.clone(),
            reconnect: Some(identity),
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert!(ShellRepo::new(&conn)
            .list_for_agent(&agent_id)
            .unwrap()
            .iter()
            .any(|session| session.id == shell_id));
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
            .list_for_agent(&agent_id)
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
        .enqueue(FleetMutation::ClearAgentReconnect {
            id: agent_id.clone(),
        })
        .unwrap();
    writer
        .enqueue(FleetMutation::MarkAgentPromptsInterrupted {
            agent_id: agent_id.clone(),
        })
        .unwrap();
    writer
        .enqueue(FleetMutation::DeleteAgent {
            id: agent_id.clone(),
        })
        .unwrap();
    short_settle();
    {
        let conn = schema::open_read_connection(&db_path).unwrap();
        assert!(AgentRepo::new(&conn).get(&agent_id).unwrap().is_none());
    }

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
        notes: Some("persist me".into()),
        tags: vec!["ui".into(), "persistence".into()],
        linked_issues: vec!["TOD-99".into()],
        linked_prs: vec!["#7".into()],
    };

    {
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue(FleetMutation::InsertTask {
                task: task.clone(),
            })
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
    let agent_id = uuid::Uuid::new_v4().to_string();
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
            .enqueue(FleetMutation::InsertAgent {
                agent: NewAgent {
                    id: agent_id.clone(),
                    task_id,
                    env_type: "local".into(),
                    mode: "agent".into(),
                },
            })
            .unwrap();
        store
            .enqueue(FleetMutation::CreateNotification {
                id: notification_id.clone(),
                message: "needs review".into(),
                related_task_id: None,
                related_agent_ids: vec![agent_id.clone()],
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
        assert_eq!(open[0].related_agent_ids, vec![agent_id.clone()]);

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
