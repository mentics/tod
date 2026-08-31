//! Shared blur/Enter commit helpers for text inputs.

use gpui::{App, Entity};
use gpui_component::input::InputEvent;
use gpui_component::input::InputState;

/// Returns true when the event should trigger a commit.
pub fn is_commit_event(event: &InputEvent) -> bool {
    matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. })
}

/// Read trimmed text from an input entity.
pub fn input_text(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).text().to_string()
}

/// Reset input to baseline without persisting.
pub fn discard_to_baseline(
    input: &Entity<InputState>,
    baseline: &str,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let baseline = baseline.to_string();
    input.update(cx, |state, cx| {
        state.set_value(&baseline, window, cx);
    });
}
