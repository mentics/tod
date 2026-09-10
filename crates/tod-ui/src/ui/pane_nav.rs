//! Cross-panel keyboard navigation for multi-column views.
//!
//! Convention: in any view laid out as multiple columns, Left/Right move the
//! focused panel. Where a panel already gives Left/Right its own meaning — the
//! task tree collapses/expands and selects the parent with them — that panel
//! binds Ctrl+Left / Ctrl+Right instead. The Ctrl chords are registered on every
//! multi-column surface, so the same keystroke crosses panels everywhere.

use crate::ui::key_context;
use gpui::{App, KeyBinding, actions};

actions!(pane_nav, [PaneFocusLeft, PaneFocusRight]);

/// Bind Ctrl+Left / Ctrl+Right on `surface`.
///
/// Use for panels whose plain arrow keys already act on their own content.
pub fn bind_modified_pane_nav(cx: &mut App, surface: &str) {
    let context = Some(key_context::excluding_input(surface));
    cx.bind_keys([
        KeyBinding::new("ctrl-left", PaneFocusLeft, context),
        KeyBinding::new("ctrl-right", PaneFocusRight, context),
    ]);
}

/// Bind plain Left/Right *and* the Ctrl variants on `surface`.
///
/// Use for panels where the plain arrows are otherwise unused.
pub fn bind_pane_nav(cx: &mut App, surface: &str) {
    bind_modified_pane_nav(cx, surface);
    let context = Some(key_context::excluding_input(surface));
    cx.bind_keys([
        KeyBinding::new("left", PaneFocusLeft, context),
        KeyBinding::new("right", PaneFocusRight, context),
    ]);
}
