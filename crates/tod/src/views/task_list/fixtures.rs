use chrono::Utc;
use std::path::PathBuf;
use uuid::Uuid;

use tod_store::fleet::FleetStore;
use tod_store::outline::types::Capability;

use super::model::{AgentInfo, TaskItem};

/// Load tree rows from the outline store for `list_id` (or empty when none).
pub fn load_tasks_from_store(
    store: &FleetStore,
    list_id: Option<Uuid>,
) -> Vec<TaskItem> {
    let Some(list_id) = list_id else {
        return Vec::new();
    };
    let rows = store.flatten_outline(list_id).unwrap_or_default();
    let counts = store.obligation_counts_for_list(list_id).unwrap_or_default();
    rows.into_iter()
        .map(|row| {
            let is_work = !row.capabilities.is_empty();
            let has_spec = row.capabilities.contains(&Capability::Spec);
            let counts = counts.get(&row.node.id).copied().unwrap_or_default();
            let lifecycle = if is_work {
                row.lifecycle.unwrap_or_else(|| "proposed".into())
            } else {
                String::new()
            };
            let agents = if row.capabilities.contains(&Capability::Agent) {
                store
                    .list_agents_for_task(&row.node.id.to_string())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|a| AgentInfo {
                        id: a.id,
                        label: format!("{} {}", env_chip_label(&a.env_type), mode_chip_label(&a.mode)),
                        status: a.runtime_status,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            TaskItem {
                id: row.node.id.to_string(),
                ticket_id: row.ticket_id,
                title: row.node.title,
                lifecycle,
                entity_path: node_scratchpad_path(&row.node.id.to_string()),
                tags: row.tags,
                agents,
                shells: Vec::new(),
                interaction_timestamp: row.node.updated_at,
                tree_ordinal: row.tree_ordinal,
                depth: row.depth,
                collapsed: row.collapsed,
                is_work_node: is_work,
                has_spec,
                requirement_count: counts.requirements,
                constraint_count: counts.constraints,
                has_children: row.has_children,
            }
        })
        .collect()
}

fn node_scratchpad_path(node_id: &str) -> PathBuf {
    PathBuf::from(".local")
        .join("agent")
        .join("nodes")
        .join(node_id)
}

fn env_chip_label(env_type: &str) -> String {
    match env_type {
        "local" => "host".into(),
        "devcontainer" => "dc".into(),
        "micro_vm" => "vm".into(),
        other => other.to_string(),
    }
}

fn mode_chip_label(mode: &str) -> String {
    match mode {
        "agent" => "auto".into(),
        "shell" => "interactive".into(),
        "interview" => "interview".into(),
        other => mode.to_string(),
    }
}

/// Generate a large in-memory fixture set for list performance tests.
pub fn large_fixture_set(base_count: usize) -> Vec<TaskItem> {
    (0..base_count)
        .map(|i| TaskItem {
            id: format!("scale-{i}"),
            ticket_id: None,
            title: format!("Scale task {i}"),
            lifecycle: "active".into(),
            entity_path: PathBuf::from(format!("test/scale-{i}")),
            tags: vec![],
            agents: Vec::new(),
            shells: Vec::new(),
            interaction_timestamp: Utc::now(),
            tree_ordinal: i,
            depth: 0,
            collapsed: false,
            is_work_node: true,
            has_spec: false,
            requirement_count: 0,
            constraint_count: 0,
            has_children: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::outline::{CreatePosition, OutlineMutation};
    use std::fs;

    #[test]
    fn loads_from_outline_tree() {
        let root = std::env::temp_dir().join(format!("tod-fixtures-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "test".into(),
                title: "Test".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        let lists = store.list_outline_lists().unwrap();
        let list_id = lists[0].id;
        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Root task".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();

        let items = load_tasks_from_store(&store, Some(list_id));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Root task");
        let projection = store.projection();
        let proj = projection.lock().expect("projection");
        let conn = proj.connection();
        let node = tod_store::outline::repos::NodeRepo::new(&conn)
            .get(uuid::Uuid::parse_str(&items[0].id).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(node.slug, "root-task");
        drop(store);
        let _ = fs::remove_dir_all(root);
    }
}
