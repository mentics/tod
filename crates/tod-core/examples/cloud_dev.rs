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
//!   cargo run -p tod-core --example cloud_dev -- <data_root> node <title> <repo> <branch> <step>...
//!       a node in `cloud`, `active`, with Lifecycle, Agent, and Files on `<repo>` (an HTTPS URL
//!       or a local checkout) and `<branch>`, and one plan step per
//!       `<step>`; prints its UUID. The mock agent reads directives in the
//!       steps (`wait 3m: …`, `write <path>: <text>`).
//!   cargo run -p tod-core --example cloud_dev -- <data_root> answer <decision> <option>
//!       answers a pending decision (its full UUID, from `tod-cli --json
//!       decisions list`) with its 1-based option, as the task panel does;
//!       `sync` then sends it to the node.
//!   cargo run -p tod-core --example cloud_dev -- <data_root> state <node> <state>
//!       sets the node's lifecycle state, bypassing every gate (to run a
//!       node's later steps again); `run` or `sync` sends it.
//!   cargo run -p tod-core --example cloud_dev -- <data_root> stop <node>
//!       takes the node out of the cloud and deletes its sandbox (and with
//!       it the sandbox's schedules), as the app's "Stop running in the
//!       cloud" does; syncs.
//!
//! Never run it on a data root the app has open.

use anyhow::{Context, Result, anyhow, bail};
use std::path::PathBuf;
use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::outline::{Capability, CreatePosition, OutlineMutation};

const USAGE: &str =
    "usage: cloud_dev <data_root> init | run <node> | sync | node <title> <repo> <branch> <step>... | answer <decision> <option> | state <node> <state> | stop <node>";

fn create_node(fleet: &FleetStore, title: &str, repo: &str, branch: &str, steps: &[String]) -> Result<uuid::Uuid> {
    fn e(err: impl std::fmt::Display) -> anyhow::Error {
        anyhow!("{err}")
    }
    let list_id = fleet
        .list_outline_lists()
        .map_err(e)?
        .into_iter()
        .find(|l| l.slug == "cloud")
        .context("no list `cloud`: run `init` first")?
        .id;
    let node = uuid::Uuid::new_v4();
    fleet
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: title.into(),
        })
        .map_err(e)?;
    fleet
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: node,
            // Agent too: a Claude run's gate before `implementing` needs it.
            capabilities: vec![Capability::Spec, Capability::Lifecycle, Capability::Agent, Capability::Files],
        })
        .map_err(e)?;
    fleet.writer().flush().map_err(e)?;
    fleet
        .enqueue(FleetMutation::UpdateTaskRepo { id: node.to_string(), repo: Some(repo.into()) })
        .map_err(e)?;
    fleet
        .enqueue(FleetMutation::UpdateTaskBranch { id: node.to_string(), branch: Some(branch.into()) })
        .map_err(e)?;
    let mut after = None;
    for body in steps {
        let step = uuid::Uuid::new_v4();
        fleet
            .enqueue_outline(OutlineMutation::CreatePlanStep {
                step_id: Some(step),
                node_id: node,
                after_id: after,
                before: false,
                body: body.clone(),
            })
            .map_err(e)?;
        after = Some(step);
    }
    fleet.writer().flush().map_err(e)?;
    tod_core::lifecycle::set_lifecycle(fleet, node, "active")?;
    fleet.writer().flush().map_err(e)?;
    Ok(node)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [root, cmd, rest @ ..] = args.as_slice() else {
        bail!("{USAGE}");
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
        ("node", [title, repo, branch, steps @ ..]) if !steps.is_empty() => {
            let id = create_node(&fleet, title, repo, branch, steps)?;
            println!("{id}");
        }
        ("answer", [decision, option]) => {
            let decision_id = uuid::Uuid::parse_str(decision).context("the decision's full UUID")?;
            let option: i64 = option.parse().context("a 1-based option number")?;
            if option < 1 {
                bail!("options are numbered from 1");
            }
            fleet
                .interview(
                    tod_store::interview::ACTOR_USER,
                    tod_store::interview::InterviewCommand::AnswerDecision {
                        decision_id,
                        option: Some(option),
                        text: None,
                    },
                )
                .map_err(|e| anyhow!("{e}"))?;
            println!("answered {decision_id} with option {option}");
        }
        ("state", [node, state]) => {
            let id = resolve(&fleet, node)?;
            tod_core::lifecycle::set_lifecycle(&fleet, uuid::Uuid::parse_str(&id)?, state)?;
            fleet.writer().flush().map_err(|e| anyhow!("{e}"))?;
            println!("{id} is now {state}");
        }
        ("stop", [node]) => {
            let id = resolve(&fleet, node)?;
            println!("{}", tod_core::cloud_sync::lost::stop_running_in_cloud(&fleet, &id, true)?);
        }
        _ => bail!("{USAGE}"),
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
