//! Drives a node in the cloud without the app's window, for testing
//! autonomous nodes against a real Blaxel workspace
//! (`doc/cloud-sandboxes/autonomous-nodes-plan.md`, milestone 1).
//!
//! Usage:
//!   cargo run -p tod-core --example cloud_dev -- <data_root> init
//!       creates the database and a list `cloud` to build the test node in
//!       with `tod-cli` (`node create --list cloud ...`).
//!   cargo run -p tod-core --example cloud_dev -- <data_root> run <node>
//!       `cloud_sync::run_in_cloud` for the node (slug or UUID). Set
//!       `TOD_CLOUD_AGENT=mock` to run its supervisor with the mock agent.
//!   cargo run -p tod-core --example cloud_dev -- <data_root> sync
//!       `cloud_sync::sync_now`: send the outbox, pull the feed.
//!
//! Never run it on a data root the app has open.

use anyhow::{Context, Result, anyhow, bail};
use std::path::PathBuf;
use tod_store::fleet::FleetStore;
use tod_store::outline::OutlineMutation;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [root, cmd, rest @ ..] = args.as_slice() else {
        bail!("usage: cloud_dev <data_root> init | run <node> | sync");
    };
    let root = PathBuf::from(root);
    std::fs::create_dir_all(&root)?;
    let fleet = FleetStore::open(&root).map_err(|e| anyhow!("open {}: {e}", root.display()))?;
    match (cmd.as_str(), rest) {
        ("init", []) => {
            fleet
                .enqueue_outline(OutlineMutation::CreateList { slug: "cloud".into(), title: "Cloud".into() })
                .map_err(|e| anyhow!("{e}"))?;
            fleet.writer().flush().map_err(|e| anyhow!("{e}"))?;
            println!("created list `cloud` in {}", root.display());
        }
        ("run", [node]) => {
            let id = resolve(&fleet, node)?;
            let record = tod_core::cloud_sync::run_in_cloud(&fleet, &root, &id, &mut |m| eprintln!("{m}"))?;
            println!("{id} runs in {} (user {})", record.sandbox, record.user);
        }
        ("sync", []) => {
            let report = tod_core::cloud_sync::sync_now(&fleet, &root)?;
            println!("{}", report.summary());
        }
        _ => bail!("usage: cloud_dev <data_root> init | run <node> | sync"),
    }
    let _ = fleet.flush_on_quit();
    Ok(())
}

/// A node's UUID from its slug or UUID.
fn resolve(fleet: &FleetStore, raw: &str) -> Result<String> {
    if uuid::Uuid::parse_str(raw).is_ok() {
        return Ok(raw.to_string());
    }
    fleet
        .read(|conn| {
            Ok(tod_store::outline::repos::NodeRepo::new(conn)
                .get_by_slug(raw)?
                .map(|n| n.id.to_string()))
        })
        .context("look up the node")?
        .ok_or_else(|| anyhow!("no node {raw}"))
}
