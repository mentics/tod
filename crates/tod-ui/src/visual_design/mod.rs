//! Visual design browser: shows an obligation's mockup in a separate browser
//! window docked beside tod. Design: `doc/ui/visual-design-browser.md`.
//!
//! Placement decision (item B1): this module lives in `tod-ui`, not
//! `tod-core`. Nothing here needs to be shared with `tod-cli`, and no
//! dependency has forced a move. The pure parts (`placement`, server, browser)
//! hold no GPUI types, so they can move to `tod-core` later if that changes.

pub mod browser;
pub mod chrome;
pub mod feedback;
pub mod launcher;
pub mod mover;
pub mod placement;
pub mod server;
