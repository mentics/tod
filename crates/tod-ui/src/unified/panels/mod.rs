//! Real column panels, replacing `PlaceholderPanel` one kind at a time
//! (W5, W7, W9). Each module owns one `PanelKind`'s implementation.

pub mod details;

pub use details::DetailsPanel;
