//! Resolves gate criteria for a transition without requiring a live
//! [`tod_store::fleet::FleetStore`] — mirrors the outer/inner shape of
//! `interview::routing::interview_work_remains` (outer resolves paths/settings/
//! connection, inner takes `&Connection` for testability).

use anyhow::Result;
use rusqlite::Connection;
use tod_store::outline::{GateCriterion, GateRepo, NodeGateEvaluation};
use uuid::Uuid;

/// Gate criteria for one forward transition, paired with this node's most
/// recent evaluation of each. Opens its own short-lived read connection —
/// callers that already hold a `FleetStore` should prefer
/// `FleetStore::gate_criteria_for_transition` instead.
pub fn gate_criteria_for(
    node_id: Uuid,
    from_state: &str,
    to_state: &str,
) -> Result<Vec<(GateCriterion, Option<NodeGateEvaluation>)>> {
    let paths = crate::interview::TodPaths::discover()?;
    let settings = crate::interview::TodSettings::load(&paths)?;
    let root = settings.resolve_fleet_storage_root(&paths)?;
    let conn = tod_store::fleet::schema::open_read_connection(&root.join("tod.db"))?;
    gate_criteria_for_with_conn(&conn, node_id, from_state, to_state)
}

pub fn gate_criteria_for_with_conn(
    conn: &Connection,
    node_id: Uuid,
    from_state: &str,
    to_state: &str,
) -> Result<Vec<(GateCriterion, Option<NodeGateEvaluation>)>> {
    GateRepo::new(conn).list_evaluations_for_transition(node_id, from_state, to_state)
}
