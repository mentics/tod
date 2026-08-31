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

fn plain_text_html(text: &str) -> SharedString {
    let body = escape_html(text).replace('\n', "<br>");
    SharedString::from(format!("<p>{body}</p>"))
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
    TextView::markdown(id, markdown, window, cx)
        .style(
            TextViewStyle::default()
                .paragraph_gap(rems(0.5))
                .heading_font_size(|level, rem_size| match level {
                    1..=3 => rem_size * 1.,
                    4 => rem_size * 0.9,
                    _ => rem_size * 0.8,
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
