//! GPUI key-context helpers for suppressing parent-surface shortcuts during text input.
//!
//! gpui-component [`Input`](gpui_component::input::Input) registers focus context
//! [`INPUT`]. Parent views bind shortcuts to their own context (for example
//! `TaskList`). Because GPUI matches bindings at ancestor depths too, parent
//! shortcuts still fire unless the binding predicate excludes [`INPUT`].

use gpui::{Action, App, KeyBinding};

/// gpui-component text field focus context.
pub const INPUT: &str = "Input";

/// Predicate for app-wide shortcuts that must not fire while a text field is focused.
pub const NOT_INPUT: &str = "!Input";

/// Build a surface context predicate that excludes text input focus.
///
/// Leaks the formatted string; intended for one-time keymap registration at startup.
pub fn excluding_input(surface: &str) -> &'static str {
    Box::leak(format!("{surface} && !{INPUT}").into_boxed_str())
}

/// Build a surface context predicate for when a text field inside that surface is focused.
///
/// Uses the descendant operator because focus moves to a child `Input` node whose
/// context stack frame only contains [`INPUT`], not the parent surface identifier.
pub fn including_input(surface: &str) -> &'static str {
    Box::leak(format!("{surface} > {INPUT}").into_boxed_str())
}

/// Bind Escape to close a side panel when the panel or any of its text fields are focused.
pub fn bind_panel_escape<A>(cx: &mut App, action: A, surface: &str)
where
    A: Action + Clone,
{
    cx.bind_keys([
        KeyBinding::new("escape", action.clone(), Some(excluding_input(surface))),
        KeyBinding::new("escape", action, Some(including_input(surface))),
    ]);
}
