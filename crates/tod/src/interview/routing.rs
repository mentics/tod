use std::path::{Path, PathBuf};

use crate::interview::{
    InterviewSession, InterviewSessionStatus, SessionStore, TodPaths,
    config::{base_interview_phase, parse_interview_config, path_for_storage, paths_match},
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
pub fn interview_work_remains(entity_path: &Path, lifecycle: &str) -> bool {
    let Some(phase) = interview_phase_for_lifecycle(lifecycle) else {
        return false;
    };
    let Ok(paths) = TodPaths::discover() else {
        return true;
    };
    let Ok(store) = SessionStore::open(&paths) else {
        return true;
    };
    let Ok(sessions) = store.list_sessions() else {
        return true;
    };

    let entity_path = entity_path
        .canonicalize()
        .unwrap_or_else(|_| entity_path.to_path_buf());
    let storage_key = path_for_storage(&entity_path);
    let wanted_base = base_interview_phase(phase);

    let matches: Vec<&InterviewSession> = sessions
        .iter()
        .filter(|s| session_matches_entity_phase(s, &storage_key, &entity_path, wanted_base))
        .collect();

    if matches.is_empty() {
        return true;
    }
    if matches.iter().any(|s| session_open_question_count(s) > 0) {
        return true;
    }
    if matches
        .iter()
        .any(|s| s.status == InterviewSessionStatus::Active && session_needs_bootstrap(s))
    {
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

fn entity_paths_equal(stored: &str, storage_key: &str, entity_path: &Path) -> bool {
    let stored_path = PathBuf::from(stored);
    stored.eq_ignore_ascii_case(storage_key) || paths_match(&stored_path, entity_path)
}

fn session_matches_entity_phase(
    session: &InterviewSession,
    storage_key: &str,
    entity_path: &Path,
    wanted_base: &str,
) -> bool {
    let Some(stored) = session.entity_path.as_deref() else {
        return false;
    };
    if !entity_paths_equal(stored, storage_key, entity_path) {
        return false;
    }
    let session_base = session
        .phase
        .as_deref()
        .map(base_interview_phase)
        .unwrap_or("");
    wanted_base.is_empty() || session_base == wanted_base
}

fn session_needs_bootstrap(session: &InterviewSession) -> bool {
    !session
        .config_path
        .as_ref()
        .is_some_and(|p| Path::new(p).exists())
}

fn session_open_question_count(session: &InterviewSession) -> usize {
    let Some(cfg_path) = session
        .config_path
        .as_ref()
        .map(Path::new)
        .filter(|p| p.exists())
    else {
        return 0;
    };
    let Ok(config) = parse_interview_config(cfg_path) else {
        return 0;
    };
    load_queue_dir(&config.queue).map(|q| q.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::{
        NewInterviewSession, config::path_for_storage, paths::clear_data_root_override,
        set_data_root,
    };
    use std::fs;

    fn temp_repo(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "tod-interview-routing-{}-{}",
            label,
            uuid::Uuid::new_v4()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        set_data_root(root.clone());
        root
    }

    #[test]
    fn no_session_means_work_remains() {
        let root = temp_repo("no-session");
        let entity = root
            .join("doc")
            .join("process")
            .join("projects")
            .join("p")
            .join("tasks")
            .join("t");
        fs::create_dir_all(&entity).unwrap();
        assert!(interview_work_remains(&entity, "proposed"));
        let _ = fs::remove_dir_all(&root);
        clear_data_root_override();
    }

    #[test]
    fn complete_session_means_no_work_remains() {
        let root = temp_repo("complete");
        let entity = root
            .join("doc")
            .join("process")
            .join("projects")
            .join("p")
            .join("tasks")
            .join("t");
        fs::create_dir_all(&entity).unwrap();
        let paths = TodPaths::from_repo_root(root.clone());
        let store = SessionStore::open(&paths).unwrap();
        let session = store
            .insert_session_with_metadata(
                NewInterviewSession {
                    display_name: "T — Task requirements".into(),
                    entity_path: path_for_storage(&entity),
                    phase: "task-requirements-interview".into(),
                },
                InterviewSessionStatus::Complete,
            )
            .unwrap();
        assert_eq!(session.status, InterviewSessionStatus::Complete);
        assert!(!interview_work_remains(&entity, "proposed"));
        let _ = fs::remove_dir_all(&root);
        clear_data_root_override();
    }
}
