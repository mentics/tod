//! A text field that knows where focus goes when editing ends.
//!
//! Escape in a focused field stops editing and hands focus back to the
//! surface that owns it ([`TextField::leave`]). A surface binds Escape for
//! its text fields with [`key_context::bind_panel_escape`](super::key_context::bind_panel_escape)
//! and calls `leave` on each of its fields first in the handler, so the key
//! never also closes the panel the field sits in.

use gpui::{App, Entity, FocusHandle, Focusable as _, Window};
use gpui_component::input::InputState;

/// An [`InputState`] paired with the focus handle Escape returns to.
#[derive(Clone)]
pub struct TextField {
    pub state: Entity<InputState>,
    return_focus: FocusHandle,
}

impl TextField {
    pub fn new(state: Entity<InputState>, return_focus: FocusHandle) -> Self {
        Self { state, return_focus }
    }

    /// Whether the field itself has keyboard focus.
    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.state.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Focus the field to start editing.
    pub fn enter(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Stop editing: when the field is focused, move focus back out and return
    /// true. Returns false (touching nothing) when it was not being edited.
    pub fn leave(&self, window: &mut Window, cx: &mut App) -> bool {
        if !self.is_focused(window, cx) {
            return false;
        }
        self.return_focus.focus(window, cx);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        AppContext as _, Context, FocusHandle, InteractiveElement as _, IntoElement,
        ParentElement as _, Render, TestAppContext, div,
    };
    use gpui_component::{Root, input::Input};

    struct Host {
        focus: FocusHandle,
        field: TextField,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().track_focus(&self.focus).child(Input::new(&self.field.state))
        }
    }

    #[gpui::test]
    fn leave_returns_focus_only_while_editing(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut field = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let state = cx.new(|cx| InputState::new(window, cx));
            let focus = cx.focus_handle();
            let f = TextField::new(state, focus.clone());
            field = Some((f.clone(), focus.clone()));
            let host = cx.new(|_| Host { focus, field: f });
            Root::new(host, window, cx)
        });
        let (field, focus) = field.unwrap();
        cx.update(|window, cx| {
            window.focus(&focus, cx);
            assert!(!field.leave(window, cx));
            field.enter(window, cx);
            assert!(field.is_focused(window, cx));
            assert!(field.leave(window, cx));
            assert!(!field.is_focused(window, cx));
            assert!(focus.is_focused(window));
        });
    }
}
