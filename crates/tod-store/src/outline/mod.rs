//! Outline persistence — lists, nodes, capabilities, and tree placement.

pub mod archive;
pub mod ddl;
pub mod file_refs;
pub mod gate_criteria_seed;
pub mod migrate_interview;
pub mod mutations;
pub mod references;
pub mod repos;
pub mod resolve;
pub mod row_archive;
pub mod slug;
pub mod types;
pub mod uuid_blob;

pub use file_refs::{check_no_file_references, referenced_files};
pub use gate_criteria_seed::{GATE_CRITERIA, seed_gate_criteria};
pub use mutations::{CreatePosition, OutlineMutation, ReorderDirection};
pub use references::{BrokenReference, broken_references, check_references, referenced_slugs};
pub use repos::gate::{
    ACTIVE_VERIFYING_PLAN_IMPLEMENTED_SLUG, BUILDABLE_CRITERION_SLUG, DESIGN_CONSTRAINTS_CRITERION_SLUG, GateCriterion, GateRepo, NodeGateEvaluation, OUTCOME_FAIL,
    OUTCOME_PASS, OUTCOME_PENDING, OUTCOME_WAIVED, PLANNING_READY_REQUIREMENTS_TRACEABLE_SLUG,
    READY_ACTIVE_ACTION_CONFIG_SLUG,
    REVIEW_APPROVED_FINDINGS_ANSWERED_SLUG, REVIEW_APPROVED_REVIEW_DONE_SLUG, SOURCE_AGENT,
    SOURCE_DERIVED, SOURCE_HUMAN, VERIFYING_REVIEW_OBLIGATIONS_VERIFIED_SLUG,
    VERIFYING_REVIEW_PLAN_VERIFIED_SLUG, APPROVED_MERGED_PR_MERGED_SLUG, DERIVED_CRITERION_SLUGS,
    DESIGN_PLANNING_PHASE_CERTIFIED_SLUG, LEARN_DONE_LEARN_RECORDED_SLUG,
    MERGED_RELEASED_PHASE_CERTIFIED_SLUG, PLANNING_READY_PHASE_CERTIFIED_SLUG,
    PROPOSED_DESIGN_HAS_REQUIREMENTS_SLUG, PROPOSED_DESIGN_PHASE_CERTIFIED_SLUG,
    PR_APPROVED_MERGEABLE_SLUG, RELEASED_LEARN_PHASE_CERTIFIED_SLUG, phase_certified_slug,
};
pub use repos::obligations::{
    KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation, ObligationCounts, ObligationRepo,
};
pub use repos::NodeSummary;
pub use repos::plan_steps::{PLAN_STEP_STATUSES, PlanStep, PlanStepRepo};
pub use resolve::{ancestor_chain, phase_visible, resolve_obligations};
pub use slug::{SLUG_MAX_LEN, allocate_unique_slug, derive_node_slug, slugify};
pub use types::{
    Capability, EXTRA_CONTENT_DETAILS, EXTRA_CONTENT_SUMMARY,
    EXTRA_CONTENT_TYPES, FlatNodeRow, Node, OutlineEntry, OutlineList,
};
pub use uuid_blob::{blob_to_uuid, ms_to_datetime, now_ms, uuid_to_blob};

#[cfg(test)]
mod tests;
