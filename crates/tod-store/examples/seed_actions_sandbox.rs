//! Seed a parent+child outline whose parent has Agent, Files, and Ticket pointing
//! at a throwaway git repo, for live-testing worktrees, shells, chat, and editors.
//!
//! ```bash
//! cargo run -p tod-store --example seed_actions_sandbox -- <data-root> <git-repo>
//! ```

use std::path::PathBuf;
use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::outline::{CreatePosition, OutlineMutation, types::Capability};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage: <data-root> <git-repo>"));
    let repo = PathBuf::from(args.next().expect("usage: <data-root> <git-repo>"));

    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;

    let store = FleetStore::open(&root)?;
    store.enqueue_outline(OutlineMutation::CreateList {
        slug: "actions".into(),
        title: "Actions".into(),
    })?;
    store.writer().flush()?;
    let list_id = store.list_outline_lists()?[0].id;

    store.enqueue_outline(OutlineMutation::CreateNode {
        node_id: None,
        list_id,
        parent_id: None,
        anchor_id: None,
        position: CreatePosition::Below,
        title: "Actions project".into(),
    })?;
    store.writer().flush()?;
    store.reload_if_stale().ok();
    let parent_id = store.flatten_outline(list_id)?[0].node.id;

    store.enqueue_outline(OutlineMutation::EnableCapabilities {
        node_id: parent_id,
        capabilities: vec![Capability::Agent, Capability::Files, Capability::Ticket],
    })?;
    store.enqueue(FleetMutation::UpdateTaskRepo {
        id: parent_id.to_string(),
        repo: Some(repo.display().to_string()),
    })?;
    store.enqueue(FleetMutation::UpdateTaskBranch {
        id: parent_id.to_string(),
        branch: Some("feature-x".into()),
    })?;
    store.enqueue(FleetMutation::UpsertNodeAgent {
        node_id: parent_id.to_string(),
        platform: Some("claude".into()),
        model: Some("fable".into()),
        effort: Some("high".into()),
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

    println!("seeded {}", root.display());
    println!("parent_id={parent_id}");
    println!("child_id={child_id}");
    Ok(())
}
