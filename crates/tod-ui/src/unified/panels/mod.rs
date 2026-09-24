//! Real column panels for the unified view, replacing [`super::panel::PlaceholderPanel`]
//! one panel kind at a time (W5, W7, W9). Each module owns one `PanelKind`'s
//! implementation.

pub mod decisions;
pub mod details;
pub mod findings;
pub mod obligations;
pub mod plan;
pub mod settings;
pub mod transcript;

pub use details::DetailsPanel;
