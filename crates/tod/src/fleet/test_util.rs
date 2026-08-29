//! Shared helpers for fleet persistence verification tests.

use crate::fleet::repos::agent::{AgentRepo, NewAgent};
use crate::fleet::repos::task::{FleetTask, TaskRepo};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Snapshot counts produced by the scale generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaleSnapshot {
    pub task_count: usize,
    pub agent_count: usize,
}

/// Create a unique ephemeral fleet storage root.
pub fn temp_fleet_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("tod-fleet-verify-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    root
}

/// Remove a temp fleet root (best-effort).
pub fn cleanup_fleet_root(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

fn lifecycle_for(index: usize) -> String {
    match index % 3 {
        0 => "proposed",
        1 => "active",
        _ => "done",
    }
    .into()
}

fn deterministic_task(index: usize) -> FleetTask {
    FleetTask {
        id: format!("scale-task-{index:04}"),
        title: format!("Scale Task {index}"),
        slug: format!("scale-task-{index}"),
        lifecycle: lifecycle_for(index),
        repo: Some(format!("github.com/org/repo-{}", index % 20)),
        branch: Some(if index % 2 == 0 {
            "main".into()
        } else {
            format!("feature/{index}")
        }),
        notes: Some(format!("notes for task {index}")),
        tags: vec![format!("tag-{}", index % 5), "scale".into()],
        linked_issues: vec![format!("TOD-{index}")],
        linked_prs: vec![format!("#{index}")],
    }
}

/// Insert ~500 tasks and ~100 agents with deterministic varied fields.
pub fn insert_scale_data(conn: &Connection) -> ScaleSnapshot {
    const TASK_COUNT: usize = 500;
    const AGENT_COUNT: usize = 100;

    let mut task_ids = Vec::with_capacity(TASK_COUNT);
    for index in 0..TASK_COUNT {
        let task = deterministic_task(index);
        task_ids.push(task.id.clone());
        TaskRepo::new(conn).insert(&task).expect("scale task insert");
    }

    for index in 0..AGENT_COUNT {
        let agent = NewAgent {
            id: format!("scale-agent-{index:03}"),
            task_id: task_ids[index % TASK_COUNT].clone(),
            env_type: if index % 2 == 0 {
                "local".into()
            } else {
                "devcontainer".into()
            },
            mode: "agent".into(),
        };
        AgentRepo::new(conn)
            .insert(&agent)
            .expect("scale agent insert");
    }

    ScaleSnapshot {
        task_count: TASK_COUNT,
        agent_count: AGENT_COUNT,
    }
}
