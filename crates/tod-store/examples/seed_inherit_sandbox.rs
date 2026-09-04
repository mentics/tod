//! Seed parent+child outline with agent config only on the parent.
//!
//! ```bash
//! cargo run -p tod-store --example seed_inherit_sandbox
//! ```

use std::path::PathBuf;
use tod_store::fleet::repos::agent_config::NewAgentConfig;
use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::outline::{CreatePosition, OutlineMutation, types::Capability};

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".local/test/inherit-verify");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let workspace = {
        let text = workspace.display().to_string();
        PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
    };

    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;

    let store = FleetStore::open(&root)?;
    store
        .enqueue_outline(OutlineMutation::CreateList {
            slug: "inherit".into(),
            title: "Inherit".into(),
        })?;
    store.writer().flush()?;
    let list_id = store.list_outline_lists()?[0].id;

    store.enqueue_outline(OutlineMutation::CreateNode {
        node_id: None,
        list_id,
        parent_id: None,
        anchor_id: None,
        position: CreatePosition::Below,
        title: "Parent project".into(),
    })?;
    store.writer().flush()?;
    store.reload_if_stale().ok();
    let parent_id = store.flatten_outline(list_id)?[0].node.id;

    store.enqueue_outline(OutlineMutation::EnableCapabilities {
        node_id: parent_id,
        capabilities: vec![Capability::Agent, Capability::Lifecycle],
    })?;
    store.enqueue(FleetMutation::UpdateTaskRepo {
        id: parent_id.to_string(),
        repo: Some(workspace.display().to_string()),
    })?;
    store.enqueue(FleetMutation::UpdateTaskBranch {
        id: parent_id.to_string(),
        branch: Some("main".into()),
    })?;
    store.writer().flush()?;

    store.enqueue_outline(OutlineMutation::CreateNode {
        node_id: None,
        list_id,
        parent_id: Some(parent_id),
        anchor_id: Some(parent_id),
        position: CreatePosition::Child,
        title: "Child task".into(),
    })?;
    store.writer().flush()?;
    store.reload_if_stale().ok();
    let child_id = store
        .flatten_outline(list_id)?
        .into_iter()
        .find(|r| r.node.title == "Child task")
        .expect("child")
        .node
        .id;

    let config_id = "inherit-parent-cfg".to_string();
    store.enqueue(FleetMutation::InsertAgent {
        agent: NewAgentConfig {
            id: config_id.clone(),
            node_id: parent_id.to_string(),
            env_type: "local".into(),
            mode: "shell".into(),
            work_directory: Some(workspace.display().to_string()),
            use_worktree: false,
            platform: "claude".into(),
            model: "default".into(),
            effort: "auto".into(),
        },
    })?;
    store.writer().flush()?;
    store.reload_if_stale().ok();

    let resolved = store.resolve_agents_for_node(&child_id.to_string())?;
    assert!(resolved.inherited, "child should inherit parent config");
    assert_eq!(resolved.configs.len(), 1);
    assert_eq!(resolved.configs[0].id, config_id);

    println!("seeded {}", root.display());
    println!("parent_id={parent_id}");
    println!("child_id={child_id}");
    println!("config_id={config_id}");
    println!("cwd={}", workspace.display());

    std::fs::write(
        root.join("ids.txt"),
        format!(
            "parent_id={parent_id}\nchild_id={child_id}\nconfig_id={config_id}\ncwd={}\n",
            workspace.display()
        ),
    )?;
    Ok(())
}
