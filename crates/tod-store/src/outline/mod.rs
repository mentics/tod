//! Outline persistence — lists, nodes, capabilities, and tree placement.

pub mod archive;
pub mod workspace;
pub mod ddl;
pub mod gate_criteria_seed;
pub mod import;
pub mod migrate_interview;
pub mod mutations;
pub mod repos;
pub mod resolve;
pub mod slug;
pub mod types;
pub mod uuid_blob;

pub use gate_criteria_seed::{GATE_CRITERIA, seed_gate_criteria};
pub use import::import_doc_process;
pub use mutations::{CreatePosition, OutlineMutation, ReorderDirection};
pub use repos::gate::{
    GateCriterion, GateRepo, NodeGateEvaluation, OUTCOME_FAIL, OUTCOME_PASS, OUTCOME_PENDING,
    OUTCOME_WAIVED, SOURCE_AGENT, SOURCE_DERIVED, SOURCE_HUMAN,
};
pub use repos::obligations::{
    KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation, ObligationCounts, ObligationRepo,
};
pub use resolve::resolve_obligations;
pub use workspace::workspace_cwd_for_node;
pub use slug::{SLUG_MAX_LEN, allocate_unique_slug, derive_node_slug, slugify};
pub use types::{
    Capability, EXTRA_CONTENT_GOAL, EXTRA_CONTENT_TYPES, FlatNodeRow, Node, NodeKind, OutlineEntry,
    OutlineList,
};
pub use uuid_blob::{blob_to_uuid, ms_to_datetime, now_ms, uuid_to_blob};

#[cfg(test)]
mod tests;
