//! Schedule the next supervisor wake when agent quota is exhausted.

use anyhow::Result;
use tod_store::fleet::FleetStore;
use tod_store::interview::InterviewCommand;
use tod_store::waits::NewWait;
use uuid::Uuid;

pub use tod_core::conversation::quota::{UsageLimit, detect};

/// Records an `until` wait on `node` for the reset.
pub fn record(fleet: &FleetStore, node: Uuid, limit: &UsageLimit) -> Result<()> {
    fleet
        .interview(
            crate::waits::ACTOR,
            InterviewCommand::RecordWait { node_id: node, wait: NewWait::until(limit.reset_at_ms) },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}
