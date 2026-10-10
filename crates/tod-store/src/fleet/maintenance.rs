//! Writes that are not mutations on the writer's queue (the journey queue's
//! bookkeeping), as data, so a client of the daemon can ask the daemon to make
//! them: only the process that owns the store writes it.

use crate::fleet::store::FleetStore;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Maintenance {
    /// Delete every `journey_changes` row through `through_id`.
    PruneJourneyChanges { through_id: i64 },
    /// Queue a built bundle for submission; answers with its `SubmissionEntry`.
    QueueJourneySubmission {
        bundle_id: Uuid,
        node_id: Option<Uuid>,
        seq: i64,
        reason: String,
    },
    SetJourneySubmissionStatus { bundle_id: Uuid, status: String },
}

impl Maintenance {
    /// Make the write on `store`, which owns the database.
    pub fn apply(self, store: &FleetStore) -> Result<serde_json::Value> {
        match self {
            Self::PruneJourneyChanges { through_id } => {
                store.prune_journey_changes_through(through_id)?;
                Ok(serde_json::Value::Null)
            }
            Self::QueueJourneySubmission { bundle_id, node_id, seq, reason } => {
                let entry = store.queue_journey_submission(bundle_id, node_id, seq, &reason)?;
                Ok(serde_json::to_value(entry)?)
            }
            Self::SetJourneySubmissionStatus { bundle_id, status } => {
                store.set_journey_submission_status(bundle_id, &status)?;
                Ok(serde_json::Value::Null)
            }
        }
    }
}
