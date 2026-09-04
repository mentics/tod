use std::path::PathBuf;
use uuid::Uuid;

use tod_store::fleet::{FleetStore, ResolvedAgentConfigs};
use tod_store::outline::types::Capability;

use super::model::{AgentInfo, ShellInfo, TaskItem};

/// Load tree rows from the outline store for `list_id` (or empty when none).
pub fn load_tasks_from_store(store: &FleetStore, list_id: Option<Uuid>) -> Vec<TaskItem> {
    let Some(list_id) = list_id else {
        return Vec::new();
    };
    let rows = store.flatten_outline(list_id).unwrap_or_default();
    let counts = store
        .obligation_counts_for_list(list_id)
        .unwrap_or_default();
    rows.into_iter()
        .map(|row| {
            let is_work = !row.capabilities.is_empty();
            let has_spec = row.capabilities.contains(&Capability::Spec);
            let has_lifecycle = row.capabilities.contains(&Capability::Lifecycle);
            let counts = counts.get(&row.node.id).copied().unwrap_or_default();
            // Only Lifecycle capability owns a lifecycle chip. Do not invent "proposed"
            // when Agent/Spec alone are enabled.
            let lifecycle = if has_lifecycle {
                row.lifecycle.unwrap_or_else(|| "proposed".into())
            } else {
                String::new()
            };
            let node_id = row.node.id.to_string();
            let resolved =
                store
                    .resolve_agents_for_node(&node_id)
                    .unwrap_or_else(|_| ResolvedAgentConfigs {
                        queried_node_id: node_id.clone(),
                        source_node_id: node_id.clone(),
                        inherited: false,
                        configs: Vec::new(),
                    });
            let agents: Vec<AgentInfo> = resolved
                .configs
                .iter()
                .map(|a| {
                    let mut status = a.runtime_status.clone();
                    if let Ok(runs) = store.list_terminal_agent_runs_for_config(&a.id) {
                        let terminal_live = runs.iter().any(|run| {
                            run.reconnect.is_some()
                                && run.ended_at.is_none()
                                && run.runtime_status != "not_running"
                        });
                        if terminal_live
                            && matches!(
                                status.as_str(),
                                "not_running" | "waiting" | "starting" | "processing"
                            )
                        {
                            status = "processing".into();
                        }
                    }
                    AgentInfo {
                        id: a.id.clone(),
                        label: format!(
                            "{} {}",
                            env_chip_label(&a.env_type),
                            mode_chip_label(&a.mode)
                        ),
                        status,
                        inherited: resolved.inherited,
                    }
                })
                .collect();
            let mut shells = Vec::new();
            for config in &resolved.configs {
                if let Ok(sessions) = store.list_shells_for_config(&config.id) {
                    for shell in sessions {
                        let label = if resolved.configs.len() > 1 {
                            format!("{} · {}", config.id, shell.id)
                        } else {
                            shell.id.clone()
                        };
                        shells.push(ShellInfo {
                            id: shell.id,
                            label,
                        });
                    }
                }
            }
            TaskItem {
                id: node_id,
                ticket_id: row.ticket_id,
                title: row.node.title,
                lifecycle,
                entity_path: node_scratchpad_path(&row.node.id.to_string()),
                tags: row.tags,
                agents,
                shells,
                interaction_timestamp: row.node.updated_at,
                tree_ordinal: row.tree_ordinal,
                parent_id: row.parent_id.map(|id| id.to_string()),
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
        _other => mode.to_string(),
    }
}

/// Generate a large in-memory fixture set for list performance tests.
#[cfg(test)]
pub fn large_fixture_set(base_count: usize) -> Vec<TaskItem> {
    use chrono::Utc;
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
            parent_id: None,
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
    use std::fs;
    use tod_store::outline::types::Capability;
    use tod_store::outline::{CreatePosition, OutlineMutation};

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

    #[test]
    fn agent_capability_alone_does_not_invent_proposed_lifecycle() {
        let root = std::env::temp_dir().join(format!("tod-fixtures-agent-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "agent-only".into(),
                title: "Agent only".into(),
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
                title: "Agent node".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        let node_id = store.flatten_outline(list_id).unwrap()[0].node.id;
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id,
                capabilities: vec![Capability::Agent],
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();

        let items = load_tasks_from_store(&store, Some(list_id));
        assert_eq!(items.len(), 1);
        assert!(items[0].lifecycle.is_empty());
        drop(store);
        let _ = fs::remove_dir_all(root);
    }
}
