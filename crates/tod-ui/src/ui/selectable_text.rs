//! Selectable read-only text for user/data display (errors, transcripts, query results, etc.).
//!
//! Use for any non-input surface showing data the user may need to copy while troubleshooting.
//! Static chrome (button labels, section headings) may stay plain `div` text.

use gpui::{App, ElementId, SharedString, StyleRefinement, Styled, Window, px, rems};
use gpui_component::ActiveTheme;
use gpui_component::text::{TextView, TextViewStyle};

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Wrap each line in its own paragraph.
///
/// `<br>` is parsed into a node that renders as a zero-height element, so it
/// drops the line break instead of making one — every newline in the source
/// would collapse and the text would run together. One paragraph per line is
/// the only structure the renderer honours. Blank lines carry a non-breaking
/// space so they keep their height.
fn plain_text_html(text: &str) -> SharedString {
    let mut body = String::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            body.push_str("<p>&nbsp;</p>");
        } else {
            body.push_str("<p>");
            body.push_str(&escape_html(line));
            body.push_str("</p>");
        }
    }
    SharedString::from(body)
}

/// Plain data text the user can drag-select and copy (Ctrl/Cmd+C).
pub fn selectable_text(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> TextView {
    let text = text.into();
    TextView::html(id, plain_text_html(&text), window, cx)
        .selectable(true)
        .style(TextViewStyle::default().paragraph_gap(rems(0.)))
}

/// Markdown text the user can drag-select and copy (Ctrl/Cmd+C).
pub fn selectable_markdown(
    id: impl Into<ElementId>,
    markdown: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> TextView {
    // `paragraph_gap` has no effect on a non-scrollable `TextView` in
    // gpui-component 0.5.1: `render_root` marks the root `is_last`, and
    // `Node::Root` hands that same flag to every child, so each block skips its
    // bottom padding. Blocks are separated by their own styling (heading size,
    // list markers, code-block background) until that is fixed upstream.
    TextView::markdown(id, markdown, window, cx)
        .style(
            TextViewStyle::default()
                .paragraph_gap(rems(0.5))
                .heading_font_size(|level, rem_size| match level {
                    1 => rem_size * 1.3,
                    2 => rem_size * 1.2,
                    3 => rem_size * 1.1,
                    4 => rem_size * 1.,
                    _ => rem_size * 0.95,
                })
                .code_block(
                    StyleRefinement::default()
                        .bg(cx.theme().muted)
                        .p_2()
                        .rounded_md()
                        .text_size(px(12.)),
                ),
        )
        .selectable(true)
}
