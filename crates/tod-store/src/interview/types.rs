//! Shared actor, phase, and obligation-history types.

use serde::Deserialize;
use uuid::Uuid;

/// Environment variable carrying the actor used to attribute agent writes.
pub const ACTOR_ENV: &str = "TOD_INTERVIEW_ACTOR";
/// Actor for writes made by the person using the app.
pub const ACTOR_USER: &str = "user";
/// The prefix of the actor a shepherd (`tod-cli` run with `TOD_ACTOR=shepherd:<id>`) acts as.
pub const SHEPHERD_ACTOR_PREFIX: &str = "shepherd:";
/// Actor for agent writes without a more specific identity.
pub const ACTOR_AGENT: &str = "agent";

pub const AUTHOR_USER: &str = "user";

pub const PHASE_REQUIREMENTS: &str = "requirements";
pub const PHASE_DESIGN: &str = "design";
pub const PHASE_PLANNING: &str = "planning";
pub const PHASES: [&str; 3] = [PHASE_REQUIREMENTS, PHASE_DESIGN, PHASE_PLANNING];

/// Sentinel phase for obligations that predate phase-tagging (migrated rows,
/// imported docs). Never a valid target for creating a new obligation, but a
/// valid target for an explicit phase change.
pub const PHASE_UNKNOWN: &str = "unknown";
/// Valid phase values for an obligation, including the `unknown` sentinel.
/// `planning` is deliberately excluded: obligations are a requirements/design
/// artifact, while planning produces structured plan steps
/// (`outline::PlanStepRepo`) instead. Historical obligations tagged `planning`
/// from before this split still read back fine — this only gates creation.
pub const OBLIGATION_PHASES: [&str; 3] = [PHASE_UNKNOWN, PHASE_REQUIREMENTS, PHASE_DESIGN];

/// How long a change-log row holding an obligation's prior row (a delete or
/// an edit) survives trimming.
pub const OBLIGATION_SNAPSHOT_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// An obligation row as it was before a delete or an edit, kept in
/// `interview_changes.prior` by the obligation triggers.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ObligationPrior {
    pub kind: String,
    pub ordinal: i64,
    pub section: Option<String>,
    pub body: String,
    pub phase: String,
    pub visual_design_path: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A change-log row that kept an obligation's prior row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObligationSnapshot {
    pub rev: i64,
    pub node_id: Uuid,
    pub obligation_id: Uuid,
    /// `delete` or `update`.
    pub op: String,
    pub actor: String,
    pub at: i64,
    pub prior: ObligationPrior,
}
