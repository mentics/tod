use anyhow::Result;
use rusqlite::Connection;
use tod_store::fleet::repos::interview_session::InterviewSessionRepo;
use tod_store::interview::{
    InterviewRepo, MEMORY_HANDOFF, MEMORY_OPEN, MEMORY_PARKED, PHASE_PLANNING,
    QUESTION_MAKER_EXHAUSTED, STATUS_OPEN, phase_for_session_key,
};
use uuid::Uuid;

use crate::interview::InterviewSessionStatus;
use crate::interview::phase::base_interview_phase;
use crate::process::interview_phase_for_lifecycle;

/// Context stored when the workspace was opened from a task-list lifecycle jump,
/// so **Proceed** can route to the lifecycle transition panel for that task.
#[derive(Debug, Clone)]
pub struct TaskListProceedContext {
    pub task_id: String,
    pub lifecycle: String,
}

/// A phase interview is complete when the question maker has nothing more to
/// ask, nothing is open or waiting to be processed, and no follow-up is
/// pending. Planning additionally requires every parked item consumed.
pub fn interview_complete(conn: &Connection, node_id: Uuid, session_id: Uuid) -> Result<bool> {
    let repo = InterviewRepo::new(conn);
    let Some((state, _)) = repo.question_maker_state(session_id)? else {
        return Ok(false);
    };
    if state != QUESTION_MAKER_EXHAUSTED {
        return Ok(false);
    }
    if !repo.list_questions(node_id, &[STATUS_OPEN])?.is_empty()
        || !repo.unprocessed_answers(node_id)?.is_empty()
        || !repo
            .list_memory(node_id, Some(MEMORY_HANDOFF), Some(MEMORY_OPEN))?
            .is_empty()
    {
        return Ok(false);
    }
    let phase = InterviewSessionRepo::new(conn)
        .get(session_id)?
        .map(|s| phase_for_session_key(&s.phase))
        .unwrap_or_default();
    if phase == PHASE_PLANNING
        && !repo
            .list_memory(node_id, Some(MEMORY_PARKED), Some(MEMORY_OPEN))?
            .is_empty()
    {
        return Ok(false);
    }
    Ok(true)
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
    // Read-only: the running app holds the store lock.
    let Ok(conn) = tod_store::fleet::schema::open_read_connection(&root.join("tod.db")) else {
        return true;
    };
    interview_work_remains_with_conn(&conn, node_id, phase)
}

pub fn interview_work_remains_with_conn(conn: &Connection, node_id: Uuid, phase: &str) -> bool {
    let wanted_base = base_interview_phase(phase);
    let Ok(sessions) = InterviewSessionRepo::new(conn).list_for_node(node_id) else {
        return true;
    };
    let matches: Vec<_> = sessions
        .iter()
        .filter(|s| base_interview_phase(&s.phase) == wanted_base)
        .collect();
    if matches.is_empty() {
        return true;
    }
    if matches
        .iter()
        .any(|s| s.status == InterviewSessionStatus::Complete)
    {
        return false;
    }
    !matches
        .iter()
        .any(|s| interview_complete(conn, node_id, s.id).unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::db::{InterviewSessionStatus, NewInterviewSession, SessionStore};
    use std::fs;
    use tod_store::fleet::FleetStore;
    use tod_store::outline::OutlineMutation;

    fn test_node() -> (std::path::PathBuf, std::sync::Arc<FleetStore>, Uuid) {
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

    #[test]
    fn no_session_means_work_remains() {
        let (_root, fleet, node_id) = test_node();
        assert!(
            fleet
                .read(|conn| Ok(interview_work_remains_with_conn(
                    conn,
                    node_id,
                    "task-requirements-interview"
                )))
                .unwrap()
        );
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
        assert!(
            !fleet
                .read(|conn| Ok(interview_work_remains_with_conn(
                    conn,
                    node_id,
                    "task-requirements-interview"
                )))
                .unwrap()
        );
    }

    #[test]
    fn active_session_is_complete_only_when_exhausted_and_drained() {
        use tod_store::interview::{ACTOR_USER, InterviewCommand};
        let (_root, fleet, node_id) = test_node();
        let store = SessionStore::open(fleet.clone());
        let session = store
            .insert_session_with_metadata(
                NewInterviewSession {
                    node_id,
                    agent_config_id: None,
                    display_name: "T".into(),
                    phase: "task-requirements-interview".into(),
                },
                InterviewSessionStatus::Active,
                None,
            )
            .unwrap();
        let complete = || {
            fleet
                .read(|conn| interview_complete(conn, node_id, session.id))
                .unwrap()
        };
        assert!(!complete());
        fleet
            .interview(
                ACTOR_USER,
                InterviewCommand::SetExhausted {
                    session_id: session.id,
                    reason: Some("done".into()),
                },
            )
            .unwrap();
        assert!(complete());
    }
}
