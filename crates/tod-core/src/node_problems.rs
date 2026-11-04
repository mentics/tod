//! Reporting a background failure on its node, durably
//! (`tod_store::node_problems`), so the node's task panel shows it and
//! `crate::next_actor` can count it. The replacement for
//! `session_log::report_problem` wherever the failure happens in the daemon
//! (or anywhere no conversation driver drains the in-memory list).
//!
//! Every report is also logged (`tracing::warn!`), as before; the log is no
//! longer the only trace.

use anyhow::{Context, Result};
use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::node_problems::{NodeProblem, NodeProblemRepo};
use uuid::Uuid;

/// Records `problem` on its node (updating the row of the same kind) and logs it.
pub fn record(fleet: &FleetStore, problem: NodeProblem) -> Result<()> {
    tracing::warn!("{}", problem.message);
    fleet
        .enqueue(FleetMutation::RecordNodeProblem { problem })
        .map_err(|e| anyhow::anyhow!("could not record the problem on the node: {e}"))?;
    fleet.writer().flush().context("could not record the problem on the node")?;
    Ok(())
}

/// A problem that does not retry, of `kind` on `node_id` (a node id string).
pub fn report(fleet: &FleetStore, node_id: &str, kind: &str, message: String) -> Result<()> {
    let node = Uuid::parse_str(node_id).with_context(|| format!("node id {node_id:?}"))?;
    record(fleet, NodeProblem::new(node, kind, message, tod_store::outline::uuid_blob::now_ms()))
}

/// The job of `kind` succeeded for `node_id`: drops its problem, when it has
/// one (nothing is written otherwise).
pub fn clear(fleet: &FleetStore, node_id: &str, kind: &str) -> Result<()> {
    let node = Uuid::parse_str(node_id).with_context(|| format!("node id {node_id:?}"))?;
    let present = fleet
        .read(|c| NodeProblemRepo::new(c).list_for_nodes(&[node]))?
        .iter()
        .any(|p| p.kind == kind);
    if !present {
        return Ok(());
    }
    fleet
        .enqueue(FleetMutation::ClearNodeProblem { node, kind: kind.to_string() })
        .map_err(|e| anyhow::anyhow!("could not clear the problem on the node: {e}"))?;
    fleet.writer().flush().context("could not clear the problem on the node")?;
    Ok(())
}

/// The problems of `node_id` (every kind), oldest first.
pub fn for_node(fleet: &FleetStore, node_id: Uuid) -> Result<Vec<NodeProblem>> {
    fleet.read(|c| NodeProblemRepo::new(c).list_for_nodes(&[node_id]))
}
