//! Seed `.local/test/terminal-agent-ui-verify` with one outline task and **no** agent config.
//!
//! ```bash
//! cargo run -p tod-store --example seed_terminal_agent_ui_sandbox
//! ```

use std::path::PathBuf;
use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::outline::{CreatePosition, OutlineMutation, types::Capability};

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".local/test/terminal-agent-ui-verify");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let workspace = {
        let text = workspace.display().to_string();
        let cleaned = text.strip_prefix(r#"\\?\"#).unwrap_or(&text);
        PathBuf::from(cleaned)
    };

    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(root.join("shots"))?;

    let store = FleetStore::open(&root)?;
    store.enqueue_outline(OutlineMutation::CreateList {
        slug: "terminal-ui".into(),
        title: "Terminal UI".into(),
    })?;
    store.writer().flush()?;
    let list_id = store.list_outline_lists()?[0].id;

    store.enqueue_outline(OutlineMutation::CreateNode {
        node_id: None,
        list_id,
        parent_id: None,
        anchor_id: None,
        position: CreatePosition::Below,
        title: "Terminal UI task".into(),
    })?;
    store.writer().flush()?;
    store.reload_if_stale().ok();
    let task_id = store.flatten_outline(list_id)?[0].node.id;

    store.enqueue_outline(OutlineMutation::EnableCapabilities {
        node_id: task_id,
        capabilities: vec![Capability::Agent, Capability::Lifecycle],
    })?;
    store.enqueue(FleetMutation::UpdateTaskRepo {
        id: task_id.to_string(),
        repo: Some(workspace.display().to_string()),
    })?;
    store.enqueue(FleetMutation::UpdateTaskBranch {
        id: task_id.to_string(),
        branch: Some("main".into()),
    })?;
    store.writer().flush()?;
    store.reload_if_stale().ok();

    println!("seeded {}", root.display());
    println!("task_id={task_id}");
    println!("cwd={}", workspace.display());

    std::fs::write(
        root.join("ids.txt"),
        format!(
            "task_id={task_id}
cwd={}
",
            workspace.display()
        ),
    )?;
    Ok(())
}
