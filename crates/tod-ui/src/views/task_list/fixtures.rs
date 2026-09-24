use std::path::PathBuf;
use uuid::Uuid;

use tod_store::fleet::FleetStore;
use tod_store::outline::types::Capability;

use super::model::{ShellInfo, TaskItem};

/// Load tree rows from the outline store for `list_id` (or empty when none).
pub fn load_tasks_from_store(store: &FleetStore, list_id: Option<Uuid>) -> Vec<TaskItem> {
    let Some(list_id) = list_id else {
        return Vec::new();
    };
    let rows = store.flatten_outline(list_id).unwrap_or_default();
    // One list-scoped query per column instead of four per row: agent runtime
    // commits reload this list often, and each per-row call takes the
    // projection mutex.
    let counts = store
        .obligation_counts_for_list(list_id)
        .unwrap_or_default();
    let agent_sources = store
        .capability_sources_for_list(list_id, Capability::Agent)
        .unwrap_or_default();
    let files_sources = store
        .capability_sources_for_list(list_id, Capability::Files)
        .unwrap_or_default();
    let live_run_counts = store.live_run_counts_for_list(list_id).unwrap_or_default();
    // Every node's pending incoming-change count, in one query.
    let incoming = store.incoming_counts().unwrap_or_default();
    let mut shells_by_node = store.shells_for_list(list_id).unwrap_or_default();
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
            // Agent and Files inherit from the nearest ancestor that has them;
            // the maps already carry that resolution.
            let has_agent = agent_sources.contains_key(&row.node.id);
            let has_files = files_sources.contains_key(&row.node.id);
            let live_run_count = live_run_counts.get(&row.node.id).copied().unwrap_or(0);
            let shells = shells_by_node
                .remove(&row.node.id)
                .unwrap_or_default()
                .into_iter()
                .map(|shell| ShellInfo {
                    label: format!("shell {}", shell.label_number),
                    id: shell.id,
                })
                .collect();
            TaskItem {
                id: node_id,
                ticket_id: row.ticket_id,
                title: row.node.title,
                lifecycle,
                entity_path: node_scratchpad_path(&row.node.id.to_string()),
                tags: row.tags,
                has_actions: has_agent || has_files,
                has_files,
                live_run_count,
                shells,
                interaction_timestamp: row.node.updated_at,
                tree_ordinal: row.tree_ordinal,
                parent_id: row.parent_id.map(|id| id.to_string()),
                depth: row.depth,
                collapsed: row.collapsed,
                is_work_node: is_work,
                has_spec,
                has_agent,
                requirement_count: counts.requirements,
                constraint_count: counts.constraints,
                incoming_count: incoming.get(&row.node.id).copied().unwrap_or(0),
                has_children: row.has_children,
                    managed: row.managed,
                external_id: row.external_id,
                source_type: row.source_type,
                managed_count: row.managed_count,
                generator_status: row.generator_status,
                generator_error: row.generator_error,
                accept_ready: row.accept_ready,
                // Filled in by `TaskListView::set_attention`, not the store load.
                needs_you_count: 0,
                waiting_since: None,
                status_override: None,
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
            has_actions: false,
            has_files: false,
            live_run_count: 0,
            shells: Vec::new(),
            interaction_timestamp: Utc::now(),
            tree_ordinal: i,
            parent_id: None,
            depth: 0,
            collapsed: false,
            is_work_node: true,
            has_spec: false,
            has_agent: false,
            requirement_count: 0,
            constraint_count: 0,
            incoming_count: 0,
            has_children: false,
            managed: false,
            external_id: None,
            source_type: None,
            managed_count: None,
            generator_status: None,
            generator_error: None,
            accept_ready: false,
            needs_you_count: 0,
            waiting_since: None,
            status_override: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tod_store::fleet::FleetMutation;
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

    /// The list-scoped loader must still show a child the Agent and Files its
    /// parent owns, and the child's own shells and live runs.
    #[test]
    fn inherits_agent_and_files_and_reports_shells_and_runs() {
        let root = std::env::temp_dir().join(format!("tod-fixtures-inherit-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "inherit".into(),
                title: "Inherit".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        let list_id = store.list_outline_lists().unwrap()[0].id;
        let parent = Uuid::new_v4();
        let child = Uuid::new_v4();
        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(parent),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Parent".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(child),
                list_id,
                parent_id: Some(parent),
                anchor_id: Some(parent),
                position: CreatePosition::Child,
                title: "Child".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: parent,
                capabilities: vec![Capability::Agent, Capability::Files],
            })
            .unwrap();
        store
            .enqueue(FleetMutation::CreateShellSession {
                id: "shell-1".into(),
                node_id: child.to_string(),
                reconnect: None,
            })
            .unwrap();
        store
            .enqueue(FleetMutation::CreateAgentRun {
                node_id: child.to_string(),
                run_kind: None,
                session_name: None,
                launch: None,
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();

        let items = load_tasks_from_store(&store, Some(list_id));
        let by_title = |title: &str| {
            items
                .iter()
                .find(|item| item.title == title)
                .expect("row")
                .clone()
        };
        let parent_row = by_title("Parent");
        assert!(parent_row.has_agent && parent_row.has_files);
        assert_eq!(parent_row.live_run_count, 0);
        assert!(parent_row.shells.is_empty());

        let child_row = by_title("Child");
        assert!(child_row.has_agent, "child inherits the parent's Agent");
        assert!(child_row.has_files, "child inherits the parent's Files");
        assert!(child_row.has_actions);
        assert_eq!(child_row.live_run_count, 1);
        assert_eq!(
            child_row
                .shells
                .iter()
                .map(|shell| shell.label.as_str())
                .collect::<Vec<_>>(),
            vec!["shell 1"]
        );
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
