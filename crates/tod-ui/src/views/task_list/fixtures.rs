use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

use tod_store::fleet::FleetStore;
use tod_store::outline::types::{Capability, FlatNodeRow};

use super::model::{ShellInfo, TaskItem};

/// Load tree rows from the outline store for `list_id` (or empty when none).
pub fn load_tasks_from_store(store: &FleetStore, list_id: Option<Uuid>) -> Vec<TaskItem> {
    let Some(list_id) = list_id else {
        return Vec::new();
    };
    let rows = store.flatten_outline(list_id).unwrap_or_default();
    let counts = store.obligation_counts_for_list(list_id).unwrap_or_default();
    // Everything below is loaded once for the whole list. Doing any of it per
    // row costs a prepared statement and a projection-mutex acquisition each,
    // which is what made a few hundred rows take hundreds of milliseconds on
    // the UI thread.
    let live_run_counts = store.live_run_counts().unwrap_or_default();
    let mut shells_by_node = store.shells_by_node().unwrap_or_default();
    let inherits = InheritedCapabilities::from_rows(&rows);

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
            // Agent and Files inherit from the nearest ancestor that has them.
            let has_agent = inherits.has(row.node.id, Capability::Agent);
            let has_files = inherits.has(row.node.id, Capability::Files);
            let live_run_count = live_run_counts.get(&node_id).copied().unwrap_or(0);
            let shells = shells_by_node
                .remove(&node_id)
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
                has_children: row.has_children,
                in_flight_activity: None,
                managed: row.managed,
                external_id: row.external_id,
                source_type: row.source_type,
                managed_count: row.managed_count,
                generator_status: row.generator_status,
                generator_error: row.generator_error,
            }
        })
        .collect()
}

/// Whether Agent / Files is enabled on a node or any ancestor, resolved once
/// for a whole flattened list instead of one ancestor walk per row per
/// capability.
///
/// The flattened rows are enough on their own: a row is only visible when
/// every one of its ancestors is expanded, so each row's ancestors are also
/// in the set. Walking `parent_id` in memory is therefore exactly what the
/// per-node store lookup used to compute.
struct InheritedCapabilities {
    /// Node → (agent, files), each true when the node or an ancestor has it.
    resolved: HashMap<Uuid, (bool, bool)>,
}

impl InheritedCapabilities {
    fn from_rows(rows: &[FlatNodeRow]) -> Self {
        let mut resolved: HashMap<Uuid, (bool, bool)> = HashMap::with_capacity(rows.len());
        // Rows arrive in tree order, so a parent is always resolved before
        // its children and each row is a single lookup.
        for row in rows {
            let (parent_agent, parent_files) = row
                .parent_id
                .and_then(|parent| resolved.get(&parent).copied())
                .unwrap_or((false, false));
            resolved.insert(
                row.node.id,
                (
                    parent_agent || row.capabilities.contains(&Capability::Agent),
                    parent_files || row.capabilities.contains(&Capability::Files),
                ),
            );
        }
        Self { resolved }
    }

    fn has(&self, node_id: Uuid, cap: Capability) -> bool {
        let Some((agent, files)) = self.resolved.get(&node_id).copied() else {
            return false;
        };
        match cap {
            Capability::Agent => agent,
            Capability::Files => files,
            _ => false,
        }
    }
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
            has_children: false,
            in_flight_activity: None,
            managed: false,
            external_id: None,
            source_type: None,
            managed_count: None,
            generator_status: None,
            generator_error: None,
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
