//! GPUI key-context helpers for suppressing parent-surface shortcuts during text input.
//!
//! gpui-component [`Input`](gpui_component::input::Input) registers focus context
//! [`INPUT`]. Parent views bind shortcuts to their own context (for example
//! `TaskList`). Because GPUI matches bindings at ancestor depths too, parent
//! shortcuts still fire unless the binding predicate excludes [`INPUT`].
//!
//! ## Navigation vs edit mode
//!
//! Text fields should participate in keyboard navigation as **focus stops** while
//! disabled; **Enter** enters edit mode (enable + focus the `Input`); **Escape**
//! exits back to the parent navigation focus. While editing, parent arrow/activate
//! shortcuts must not fire. See `.cursor/rules/keyboard-focus-edit-mode.mdc` and
//! `interview/views/workspace.rs` for the reference implementation.

use gpui::{Action, App, KeyBinding};

/// gpui-component text field focus context.
pub const INPUT: &str = "Input";

/// Predicate for app-wide shortcuts that must not fire while a text field is focused.
pub const NOT_INPUT: &str = "!Input";

/// Include or exclude an input (an `InputState` or `TextareaState`) from GPUI tab order.
///
/// Disabled inputs still render with `track_focus` and would otherwise receive Tab
/// focus (showing a cursor without accepting typing). Call while not in edit mode.
pub fn set_input_tab_stop<T: gpui::Focusable>(
    input: &gpui::Entity<T>,
    tab_stop: bool,
    cx: &gpui::App,
) {
    let _ = input.read(cx).focus_handle(cx).tab_stop(tab_stop);
}

/// [`set_input_tab_stop`] for a field held as either kind of input state.
pub fn set_any_input_tab_stop(
    input: &gpui_component::input::AnyInputState,
    tab_stop: bool,
    cx: &gpui::App,
) {
    let _ = input.focus_handle(cx).tab_stop(tab_stop);
}

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

/// Whether keyboard focus is inside a dropdown's menu (a `Select`, or the
/// `List` a combobox opens) below `surface`. Escape and Enter there belong to
/// the menu: a surface handling them instead (an Escape bound with
/// [`including_input`] matches the menu's search box) leaves the menu open
/// but hidden, and its click-outside handler then swallows the next click.
/// A surface's handler should `cx.propagate()` when this is true, so the
/// menu's own binding runs.
pub fn focus_in_menu(window: &gpui::Window, surface: &str) -> bool {
    window
        .context_stack()
        .iter()
        .skip_while(|context| !context.contains(surface))
        .skip(1)
        .any(|context| context.contains("List") || context.contains("Select"))
}

/// Build a surface context predicate for when a text field tagged `tag` (via
/// `.key_context(tag)` on a wrapping element) inside that surface is focused.
/// Use this to scope a key binding (e.g. plain Enter to commit) to one
/// specific text field among several in the same surface.
pub fn including_tag(surface: &str, tag: &str) -> &'static str {
    Box::leak(format!("{surface} > {tag}").into_boxed_str())
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

/// Make Up/Down work in the menu of a gpui-component `Combobox` or `Select`
/// inside `el`.
///
/// The menu's list is a `List` key context, and the app rebinds Up/Down in
/// that context to its own clamped `ListArrowUp`/`ListArrowDown`
/// (`ui::list::keyboard`), which only the app's own lists handle: the menu
/// never moved. Wrap every dropdown's element in this so those two actions
/// become the menu's own `SelectUp`/`SelectDown`.
pub fn forward_menu_arrows<E: gpui::InteractiveElement>(el: E) -> E {
    use crate::ui::list::{ListArrowDown, ListArrowUp};
    el.on_action(|_: &ListArrowUp, window, cx| {
        window.dispatch_action(Box::new(gpui_base::actions::SelectUp), cx);
    })
    .on_action(|_: &ListArrowDown, window, cx| {
        window.dispatch_action(Box::new(gpui_base::actions::SelectDown), cx);
    })
}
