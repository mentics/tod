//! What the node is waiting on (design: "The supervisor and waiting").
//!
//! TODO(W7): read the node's open waits from `tod_store::waits` in the local
//! copy (after a pull), and schedule the next wake of a timed wait through
//! `tod_core::scheduler`. Until then nothing is ever waited on.

use anyhow::Result;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

/// Whether `node` is waiting, and on what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitStatus {
    /// Nothing open, or everything satisfied: work.
    Clear,
    /// Sleep: `reason` for the log.
    Waiting { reason: String },
}

pub fn check(_fleet: &FleetStore, _node: Uuid) -> Result<WaitStatus> {
    Ok(WaitStatus::Clear)
}

/// Schedules the wake for whatever `node` waits on that has a time.
pub fn schedule_wake(_fleet: &FleetStore, _node: Uuid) -> Result<()> {
    Ok(())
}
