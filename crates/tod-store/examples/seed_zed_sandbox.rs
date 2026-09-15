//! Seed `.local/test/zed-verify` with one task with Files enabled for socket smoke tests.
//!
//! ```bash
//! cargo run -p tod-store --example seed_zed_sandbox
//! ```

use std::path::PathBuf;
use tod_store::fleet::repos::task::FleetTask;
use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::outline::{OutlineMutation, types::Capability};

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
    let node_id = uuid::Uuid::new_v4();
    let task_id = node_id.to_string();

    store.enqueue(FleetMutation::InsertTask {
        task: FleetTask {
            id: task_id.clone(),
            title: "Zed smoke".into(),
            slug: "zed-smoke".into(),
            lifecycle: "active".into(),
            repo: Some(workspace.display().to_string()),
            branch: Some("main".into()),
            notes: Vec::new(),
            tags: vec![],
            linked_issues: vec![],
            linked_prs: vec![],
        },
    })?;
    store.writer().flush()?;

    store.enqueue_outline(OutlineMutation::EnableCapabilities {
        node_id,
        capabilities: vec![Capability::Files],
    })?;
    store.writer().flush()?;

    println!("seeded {}", root.display());
    println!("task_id={task_id}");
    println!("cwd={}", workspace.display());
    Ok(())
}
