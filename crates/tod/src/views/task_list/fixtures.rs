use chrono::Utc;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::fleet::FleetStore;
use crate::fleet::repos::agent::FleetAgent;
use crate::process::ProcessTask;

use super::model::{AgentInfo, ShellInfo, TaskItem};

/// Load tasks from `doc/process/…` scan, merged with fleet agent rows when matched.
pub fn load_tasks_from_store(store: &FleetStore, repo_root: &Path) -> Vec<TaskItem> {
    let fleet_tasks = store.list_tasks().unwrap_or_default();
    let fleet_by_id: HashMap<String, &crate::fleet::repos::task::FleetTask> =
        fleet_tasks.iter().map(|t| (t.id.clone(), t)).collect();
    let fleet_by_slug: HashMap<String, &crate::fleet::repos::task::FleetTask> =
        fleet_tasks.iter().map(|t| (t.slug.clone(), t)).collect();

    scan_process_tasks(repo_root)
        .into_iter()
        .map(|mut item| {
            if let Some(fleet) = fleet_for_item(&item, &fleet_by_id, &fleet_by_slug) {
                if item.tags.is_empty() {
                    item.tags = fleet.tags.clone();
                }
                if let Ok(agents) = store.list_agents_for_task(&fleet.id) {
                    item.agents = agents.iter().map(agent_to_info).collect();
                }
            }
            item
        })
        .collect()
}

fn fleet_for_item<'a>(
    item: &TaskItem,
    by_id: &HashMap<String, &'a crate::fleet::repos::task::FleetTask>,
    by_slug: &HashMap<String, &'a crate::fleet::repos::task::FleetTask>,
) -> Option<&'a crate::fleet::repos::task::FleetTask> {
    by_id
        .get(&item.id)
        .copied()
        .or_else(|| by_slug.get(&item.task_slug()).copied())
}

fn scan_process_tasks(repo_root: &Path) -> Vec<TaskItem> {
    crate::process::scan_process_tasks(repo_root)
        .into_iter()
        .map(process_task_to_item)
        .collect()
}

fn process_task_to_item(task: ProcessTask) -> TaskItem {
    let id = if task.project_slug.is_empty() {
        task.task_slug.clone()
    } else {
        format!("{}-{}", task.project_slug, task.task_slug)
    };
    TaskItem {
        id,
        ticket_id: None,
        title: task.title,
        lifecycle: task.lifecycle,
        entity_path: task.entity_path,
        tags: Vec::new(),
        agents: Vec::new(),
        shells: Vec::new(),
        interaction_timestamp: Utc::now(),
    }
}

fn agent_to_info(agent: &FleetAgent) -> AgentInfo {
    AgentInfo {
        id: agent.id.clone(),
        label: format!("{} {}", agent.id, agent.env_type),
        status: runtime_status_label(&agent.runtime_status),
    }
}

fn runtime_status_label(status: &str) -> String {
    match status {
        "starting" => "Starting".into(),
        "processing" => "Processing".into(),
        "waiting" => "Waiting".into(),
        "blocked" => "Blocked".into(),
        "not_running" => "Not running".into(),
        other => other.to_string(),
    }
}

impl TaskItem {
    pub fn task_slug(&self) -> String {
        self.entity_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&self.id)
            .to_string()
    }
}

/// Synthetic rows for scale checks (not shown in the live app).
pub fn large_fixture_set(base_count: usize) -> Vec<TaskItem> {
    (0..base_count)
        .map(|i| TaskItem {
            id: format!("scale-{i}"),
            ticket_id: Some(format!("TOD-{}", 1000 + i)),
            title: format!("Scale test task {i}"),
            lifecycle: "ready".into(),
            entity_path: PathBuf::from(format!("test/scale-{i}")),
            tags: vec!["scale".into()],
            agents: Vec::<AgentInfo>::new(),
            shells: Vec::<ShellInfo>::new(),
            interaction_timestamp: Utc::now(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::FleetStore;
    use std::fs;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    #[test]
    fn load_tasks_from_workspace_process_tree() {
        let root = repo_root();
        let task_list_dir = root.join("doc/process/projects/tod/tasks/task-list");
        if !task_list_dir.is_dir() {
            return;
        }
        let store_root =
            std::env::temp_dir().join(format!("tod-task-load-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&store_root).unwrap();
        let store = FleetStore::open(&store_root).unwrap();
        let tasks = load_tasks_from_store(&store, &root);
        assert!(
            tasks.iter().any(|t| t.task_slug() == "task-list"),
            "expected workspace task-list row; got {:?}",
            tasks.iter().map(|t| (&t.id, &t.title)).collect::<Vec<_>>()
        );
        assert!(
            !tasks
                .iter()
                .any(|t| t.title == "Add fleet persistence layer"),
            "hand-authored fixture titles must not appear"
        );
        drop(store);
        let _ = fs::remove_dir_all(store_root);
    }
}
