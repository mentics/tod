//! GPUI key-context helpers for suppressing parent-surface shortcuts during text input.
//!
//! gpui-component [`Input`](gpui_component::input::Input) registers focus context
//! [`INPUT`]. Parent views bind shortcuts to their own context (for example
//! `TaskList`). Because GPUI matches bindings at ancestor depths too, parent
//! shortcuts still fire unless the binding predicate excludes [`INPUT`].

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
