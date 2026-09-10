use uuid::Uuid;

use crate::interview::{
    InterviewSession, InterviewSessionStatus, SessionStore,
    config::{base_interview_phase, parse_interview_config},
    queue::load_queue_dir,
};
use crate::process::interview_phase_for_lifecycle;

/// Context stored when the workspace was opened from a task-list lifecycle jump,
/// so **Proceed** can route to the lifecycle transition panel for that task.
#[derive(Debug, Clone)]
pub struct TaskListProceedContext {
    pub task_id: String,
    pub lifecycle: String,
}

/// True when lifecycle next for **proposed / design / planning** should open the
/// interview rather than the lifecycle transition panel.
pub fn interview_work_remains(node_id: Uuid, lifecycle: &str) -> bool {
    let Some(phase) = interview_phase_for_lifecycle(lifecycle) else {
        return false;
    };
    let Ok(paths) = crate::interview::TodPaths::discover() else {
        return true;
    };
    let Ok(settings) = crate::interview::TodSettings::load(&paths) else {
        return true;
    };
    let Ok(root) = settings.resolve_fleet_storage_root(&paths) else {
        return true;
    };
    let Ok(fleet) = tod_store::fleet::FleetStore::open(root) else {
        return true;
    };
    let fleet = std::sync::Arc::new(fleet);
    let store = SessionStore::open(fleet);
    interview_work_remains_with_store(&store, node_id, phase)
}

pub fn interview_work_remains_with_store(store: &SessionStore, node_id: Uuid, phase: &str) -> bool {
    let wanted_base = base_interview_phase(phase);
    let Ok(sessions) = store.list_for_node(node_id) else {
        return true;
    };

    let matches: Vec<&InterviewSession> = sessions
        .iter()
        .filter(|s| base_interview_phase(&s.phase) == wanted_base)
        .collect();

    if matches.is_empty() {
        return true;
    }
    if matches
        .iter()
        .any(|s| session_open_question_count(store.storage_root(), s) > 0)
    {
        return true;
    }
    if matches.iter().any(|s| {
        s.status == InterviewSessionStatus::Active
            && session_needs_bootstrap(store.storage_root(), s)
    }) {
        return true;
    }
    if matches
        .iter()
        .any(|s| s.status == InterviewSessionStatus::Active)
    {
        return true;
    }
    false
}

fn session_needs_bootstrap(data_root: &std::path::Path, session: &InterviewSession) -> bool {
    !crate::process_bundle::session_has_scaffolding(data_root, session)
}

fn session_open_question_count(data_root: &std::path::Path, session: &InterviewSession) -> usize {
    let scratch = crate::process_bundle::resolve_session_scratchpad(data_root, session);
    let config_path = scratch.join("interview-config.md");
    if !config_path.exists() {
        return 0;
    }
    let Ok(config) = parse_interview_config(&config_path) else {
        return 0;
    };
    load_queue_dir(&config.queue).map(|q| q.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::db::{InterviewSessionStatus, NewInterviewSession, SessionStore};
    use std::fs;
    use tod_store::fleet::FleetStore;
    use tod_store::outline::OutlineMutation;

    fn test_node() -> (PathBuf, std::sync::Arc<FleetStore>, Uuid) {
        let root = std::env::temp_dir().join(format!("tod-route-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let fleet = std::sync::Arc::new(FleetStore::open(&root).unwrap());
        fleet
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "t".into(),
                title: "T".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: None,
                position: tod_store::outline::CreatePosition::Below,
                title: "N".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        let node_id = fleet.flatten_outline(list_id).unwrap()[0].node.id;
        (root, fleet, node_id)
    }

    use std::path::PathBuf;

    #[test]
    fn no_session_means_work_remains() {
        let (_root, fleet, node_id) = test_node();
        let store = SessionStore::open(fleet);
        assert!(interview_work_remains_with_store(
            &store,
            node_id,
            "task-requirements-interview"
        ));
    }

    #[test]
    fn complete_session_means_no_work_remains() {
        let (_root, fleet, node_id) = test_node();
        let store = SessionStore::open(fleet.clone());
        store
            .insert_session_with_metadata(
                NewInterviewSession {
                    node_id,
                    agent_config_id: None,
                    display_name: "T".into(),
                    phase: "task-requirements-interview".into(),
                },
                InterviewSessionStatus::Complete,
                None,
            )
            .unwrap();
        assert!(!interview_work_remains_with_store(
            &store,
            node_id,
            "task-requirements-interview"
        ));
    }
}
