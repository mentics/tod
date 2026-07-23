//! Single-line text fields backed by [`tui_textarea`].

use ratatui::style::{Modifier, Style};
use tui_textarea::{CursorMove, TextArea};

/// Create a single-line editor, optionally seeded with existing text.
pub fn single_line(initial: impl AsRef<str>) -> TextArea<'static> {
    let initial = initial.as_ref();
    let mut ta = if initial.is_empty() {
        TextArea::default()
    } else {
        TextArea::from([initial.to_string()])
    };
    // Single-line fields: no underline on the whole line.
    ta.set_cursor_line_style(Style::default());
    ta.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
    if !initial.is_empty() {
        ta.move_cursor(CursorMove::End);
    }
    ta
}

/// Like [`single_line`], but masks characters (e.g. API keys).
pub fn single_line_masked(initial: impl AsRef<str>) -> TextArea<'static> {
    let mut ta = single_line(initial);
    ta.set_mask_char('*');
    ta
}

/// Current contents as one string (first line; inputs are kept single-line).
pub fn value(ta: &TextArea<'_>) -> String {
    ta.lines().first().cloned().unwrap_or_default()
}
