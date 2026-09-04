//! Nearest-ancestor agent config resolution (reuse, no materialize).

use crate::fleet::repos::agent_config::{AgentConfigRepo, AgentConfigRepoError, AgentConfigRow};
use crate::outline::ancestor_chain;
use rusqlite::Connection;
use uuid::Uuid;

/// Agent configs resolved for a node via nearest-ancestor inheritance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentConfigs {
    /// Node that was queried.
    pub queried_node_id: String,
    /// Node that owns the returned configs (may equal queried).
    pub source_node_id: String,
    /// True when configs come from an ancestor, not the queried node.
    pub inherited: bool,
    pub configs: Vec<AgentConfigRow>,
}

/// Walk leaf → root and return the first non-empty `list_for_node` set.
///
/// Does not merge ancestors. Empty `configs` means nothing in the chain.
pub fn resolve_agent_configs_for_node(
    conn: &Connection,
    node_id: &str,
) -> Result<ResolvedAgentConfigs, AgentConfigRepoError> {
    let queried = node_id.to_string();
    let Ok(uuid) = Uuid::parse_str(node_id) else {
        let configs = AgentConfigRepo::new(conn).list_for_node(node_id)?;
        return Ok(ResolvedAgentConfigs {
            queried_node_id: queried.clone(),
            source_node_id: queried,
            inherited: false,
            configs,
        });
    };

    let chain = ancestor_chain(conn, uuid).map_err(|err| {
        AgentConfigRepoError::Other(anyhow::anyhow!("ancestor walk failed: {err:#}"))
    })?;

    let repo = AgentConfigRepo::new(conn);
    // ancestor_chain is root → leaf; nearest-wins walks leaf → root.
    for ancestor in chain.into_iter().rev() {
        let ancestor_str = ancestor.to_string();
        let configs = repo.list_for_node(&ancestor_str)?;
        if !configs.is_empty() {
            let inherited = ancestor_str != queried;
            return Ok(ResolvedAgentConfigs {
                queried_node_id: queried,
                source_node_id: ancestor_str,
                inherited,
                configs,
            });
        }
    }

    Ok(ResolvedAgentConfigs {
        queried_node_id: queried.clone(),
        source_node_id: queried,
        inherited: false,
        configs: Vec::new(),
    })
}

/// First interview-mode agent config on this node or a nearer ancestor.
pub fn resolve_interview_config_for_node(
    conn: &Connection,
    node_id: &str,
) -> Result<Option<AgentConfigRow>, AgentConfigRepoError> {
    let Ok(uuid) = Uuid::parse_str(node_id) else {
        return AgentConfigRepo::new(conn).find_interview_for_node(node_id);
    };

    let chain = ancestor_chain(conn, uuid).map_err(|err| {
        AgentConfigRepoError::Other(anyhow::anyhow!("ancestor walk failed: {err:#}"))
    })?;

    let repo = AgentConfigRepo::new(conn);
    for ancestor in chain.into_iter().rev() {
        if let Some(row) = repo.find_interview_for_node(&ancestor.to_string())? {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use crate::fleet::repos::agent_config::NewAgentConfig;
    use crate::fleet::store::FleetStore;
    use crate::fleet::writer::FleetMutation;
    use crate::outline::{CreatePosition, OutlineMutation};
    use std::fs;
    use uuid::Uuid;

    fn temp_root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("tod-resolve-agent-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn insert_config(store: &FleetStore, id: &str, node_id: &str, mode: &str) {
        store
            .enqueue(FleetMutation::InsertAgent {
                agent: NewAgentConfig {
                    id: id.into(),
                    node_id: node_id.into(),
                    env_type: "local".into(),
                    mode: mode.into(),
                    work_directory: None,
                    use_worktree: false,
                    platform: "claude".into(),
                    model: "default".into(),
                    effort: "auto".into(),
                },
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
    }

    fn setup_tree() -> (FleetStore, std::path::PathBuf, String, String, String) {
        let root = temp_root();
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "t".into(),
                title: "T".into(),
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
                title: "Grandparent".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        let gp = store.flatten_outline(list_id).unwrap()[0].node.id;

        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: Some(gp),
                anchor_id: Some(gp),
                position: CreatePosition::Child,
                title: "Parent".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        let parent = store
            .flatten_outline(list_id)
            .unwrap()
            .into_iter()
            .find(|r| r.node.title == "Parent")
            .unwrap()
            .node
            .id;

        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: Some(parent),
                anchor_id: Some(parent),
                position: CreatePosition::Child,
                title: "Child".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        let child = store
            .flatten_outline(list_id)
            .unwrap()
            .into_iter()
            .find(|r| r.node.title == "Child")
            .unwrap()
            .node
            .id;

        (
            store,
            root,
            gp.to_string(),
            parent.to_string(),
            child.to_string(),
        )
    }

    #[test]
    fn child_inherits_parent_configs() {
        let (store, root, _gp, parent, child) = setup_tree();
        insert_config(&store, "cfg-parent", &parent, "shell");

        let resolved = store.resolve_agents_for_node(&child).unwrap();
        assert!(resolved.inherited);
        assert_eq!(resolved.source_node_id, parent);
        assert_eq!(resolved.configs.len(), 1);
        assert_eq!(resolved.configs[0].id, "cfg-parent");

        drop(store);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn child_local_overrides_parent() {
        let (store, root, _gp, parent, child) = setup_tree();
        insert_config(&store, "cfg-parent", &parent, "shell");
        insert_config(&store, "cfg-child", &child, "agent");

        let resolved = store.resolve_agents_for_node(&child).unwrap();
        assert!(!resolved.inherited);
        assert_eq!(resolved.source_node_id, child);
        assert_eq!(resolved.configs.len(), 1);
        assert_eq!(resolved.configs[0].id, "cfg-child");

        drop(store);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn skips_empty_parent_uses_grandparent() {
        let (store, root, gp, _parent, child) = setup_tree();
        insert_config(&store, "cfg-gp", &gp, "shell");

        let resolved = store.resolve_agents_for_node(&child).unwrap();
        assert!(resolved.inherited);
        assert_eq!(resolved.source_node_id, gp);
        assert_eq!(resolved.configs[0].id, "cfg-gp");

        drop(store);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn empty_chain_returns_empty() {
        let (store, root, _gp, _parent, child) = setup_tree();
        let resolved = store.resolve_agents_for_node(&child).unwrap();
        assert!(!resolved.inherited);
        assert!(resolved.configs.is_empty());
        assert_eq!(resolved.source_node_id, child);

        drop(store);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interview_resolve_reuses_parent() {
        let (store, root, _gp, parent, child) = setup_tree();
        insert_config(&store, "interview-parent", &parent, "interview");

        let found = store
            .resolve_interview_agent_for_node(&child)
            .unwrap()
            .expect("interview config");
        assert_eq!(found.id, "interview-parent");

        drop(store);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interview_resolve_none_when_empty() {
        let (store, root, _gp, _parent, child) = setup_tree();
        assert!(store
            .resolve_interview_agent_for_node(&child)
            .unwrap()
            .is_none());
        drop(store);
        let _ = fs::remove_dir_all(root);
    }
}
