//! One-off: render the exact first-turn context an implementation session
//! would get for a given node (default: the deepest node in the tree, by
//! ancestor-chain length) and write it to a file for review.
//!
//! Usage:
//!   cargo run -p tod-core --example dump_implement_context -- [data_root] [out_file]
//!
//! Defaults: data_root = ".local/data", out_file = "implement-context-dump.md".
//! Opens the fleet database directly with a read-only SQLite connection
//! (bypassing `FleetStore`'s exclusive process lock entirely), so this is
//! safe to run against a live data root while the real app has it open.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::PathBuf;
use tod_core::agent_context::{ImplementRequest, NodeSelection, build_implement_message};
use tod_core::gate::PlanStepWithLinks;
use tod_core::media::MediaPaths;
use tod_store::fleet::paths::FleetPaths;
use tod_store::fleet::schema::open_read_connection;
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::repos::{NodeRepo, ObligationRepo, PlanStepRepo};
use tod_store::outline::uuid_blob::blob_to_uuid_sql;
use uuid::Uuid;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let data_root = args.next().unwrap_or_else(|| ".local/data".to_string());
    let out_file = args
        .next()
        .unwrap_or_else(|| "implement-context-dump.md".to_string());

    let paths = FleetPaths::new(&data_root)?;
    let conn = open_read_connection(paths.db())
        .with_context(|| format!("open fleet db read-only at {}", paths.db().display()))?;

    let node_id = deepest_node(&conn)?;
    let nodes = NodeRepo::new(&conn);
    let node = nodes
        .get(node_id)?
        .with_context(|| format!("node {node_id} not found"))?;
    let body = nodes
        .get_extra_content(node_id, EXTRA_CONTENT_DETAILS)
        .ok()
        .flatten();
    let obligations = ObligationRepo::new(&conn).list_for_node(node_id)?;

    let plan_repo = PlanStepRepo::new(&conn);
    let plan_steps = plan_repo
        .list_for_node(node_id)?
        .into_iter()
        .map(|step| {
            let depends_on = plan_repo.list_dependencies(step.id).unwrap_or_default();
            let satisfies = plan_repo.list_obligations(step.id).unwrap_or_default();
            PlanStepWithLinks {
                step,
                depends_on,
                satisfies,
            }
        })
        .collect();

    let ancestor_context =
        tod_core::node_context::render_inherited_context(&conn, &nodes, node_id, None)
            .unwrap_or_default();

    let media = MediaPaths::discover()?;
    let message = build_implement_message(
        &media,
        &ImplementRequest {
            data_root: &PathBuf::from(&data_root),
            working_dir: &std::env::current_dir()?,
            node: NodeSelection {
                id: node_id,
                title: node.title.clone(),
                body,
                lifecycle: nodes.get_lifecycle(node_id).ok().flatten(),
                slug: Some(node.slug.clone()),
            },
            plan_steps,
            obligations,
            ancestor_context,
        },
    )?;

    std::fs::write(&out_file, &message).with_context(|| format!("write {out_file}"))?;

    let out_path = std::fs::canonicalize(&out_file).unwrap_or_else(|_| PathBuf::from(&out_file));
    println!("Node: {} ({node_id})", node.title);
    println!("Wrote {} bytes to {}", message.len(), out_path.display());
    Ok(())
}

/// The node with the longest root-to-leaf chain across every outline list.
fn deepest_node(conn: &Connection) -> Result<Uuid> {
    let mut stmt = conn.prepare("SELECT node_id, parent_id FROM outline_entries")?;
    let mut parent_of: std::collections::HashMap<Uuid, Option<Uuid>> =
        std::collections::HashMap::new();
    let rows = stmt.query_map([], |row| {
        let node_id: Vec<u8> = row.get(0)?;
        let parent_id: Option<Vec<u8>> = row.get(1)?;
        Ok((node_id, parent_id))
    })?;
    for row in rows {
        let (node_id, parent_id) = row?;
        let node_id = blob_to_uuid_sql(&node_id)?;
        let parent_id = parent_id.map(|b| blob_to_uuid_sql(&b)).transpose()?;
        parent_of.insert(node_id, parent_id);
    }

    let depth = |mut id: Uuid| -> usize {
        let mut depth = 0;
        let mut seen = std::collections::HashSet::new();
        while let Some(Some(parent)) = parent_of.get(&id) {
            if !seen.insert(id) {
                break; // cycle guard, shouldn't happen
            }
            id = *parent;
            depth += 1;
        }
        depth
    };

    parent_of
        .keys()
        .copied()
        .max_by_key(|id| depth(*id))
        .context("no nodes in outline")
}
