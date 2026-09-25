//! The column panels for the unified view. Each module owns one
//! `PanelKind`'s implementation.

pub mod decisions;
pub mod details;
pub mod findings;
pub mod obligations;
pub mod plan;
pub mod settings;
pub mod transcript;

pub use details::DetailsPanel;

use tod_store::fleet::FleetStore;
use uuid::Uuid;

/// The outline node's title, for a panel's header. `FleetStore::get_node`,
/// not `get_task`: the latter reads the old fleet task table, which knows
/// nothing of outline nodes, and so fell back to showing the UUID.
fn node_title(fleet: &FleetStore, node_id: Uuid) -> String {
    fleet
        .get_node(&node_id.to_string())
        .ok()
        .flatten()
        .map(|node| node.title)
        .unwrap_or_else(|| node_id.to_string())
}
