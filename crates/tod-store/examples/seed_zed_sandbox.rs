//! Seed `.local/test/zed-verify` with one task + agent config for socket smoke tests.
//!
//! ```bash
//! cargo run -p tod-store --example seed_zed_sandbox
//! ```

use std::path::PathBuf;
use tod_store::fleet::repos::agent_config::NewAgentConfig;
use tod_store::fleet::repos::task::FleetTask;
use tod_store::fleet::{FleetMutation, FleetStore};

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".local/test/zed-verify");
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
    let task_id = uuid::Uuid::new_v4().to_string();
    let config_id = "zed-smoke-cfg".to_string();

    store.enqueue(FleetMutation::InsertTask {
        task: FleetTask {
            id: task_id.clone(),
            title: "Zed smoke".into(),
            slug: "zed-smoke".into(),
            lifecycle: "active".into(),
            repo: Some(workspace.display().to_string()),
            branch: Some("main".into()),
            notes: None,
            tags: vec![],
            linked_issues: vec![],
            linked_prs: vec![],
        },
    })?;
    store.writer().flush()?;

    store.enqueue(FleetMutation::InsertAgent {
        agent: NewAgentConfig {
            id: config_id.clone(),
            node_id: task_id.clone(),
            env_type: "local".into(),
            mode: "agent".into(),
            work_directory: Some(workspace.display().to_string()),
            use_worktree: false,
            platform: "claude".into(),
            model: "auto".into(),
            effort: "auto".into(),
        },
    })?;
    store.writer().flush()?;

    println!("seeded {}", root.display());
    println!("task_id={task_id}");
    println!("config_id={config_id}");
    println!("cwd={}", workspace.display());
    Ok(())
}
