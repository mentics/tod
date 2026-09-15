//! Selectable read-only text for user/data display (errors, transcripts, query results, etc.).
//!
//! Use for any non-input surface showing data the user may need to copy while troubleshooting.
//! Static chrome (button labels, section headings) may stay plain `div` text.
//!
//! Copying is gpui-component's: a drag-selection focuses the text view, whose
//! own Ctrl/Cmd+C binding copies it. The right-click menu added here copies the
//! same window-wide selection.

use gpui::{
    App, ClipboardItem, Div, ElementId, InteractiveElement, ParentElement, SharedString, Stateful,
    StyleRefinement, Styled, Window, div, px, rems,
};
use gpui_base::TextSelection;
use gpui_component::ActiveTheme;
use gpui_component::menu::{ContextMenu, ContextMenuExt, PopupMenuItem};
use gpui_component::text::{TextView, TextViewStyle};

/// A selectable text view with a right-click Copy menu.
pub type SelectableText = ContextMenu<Stateful<Div>>;

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

/// Wrap `view` so right-clicking it offers Copy for the current selection.
///
/// The context menu needs a parent element to hang off, and `TextView` is not
/// one; the wrapper carries `id` so each menu keeps its own state.
///
/// The selection is read when the menu opens: the left mouse-down that clicks
/// Copy clears the window's selection before the item's handler runs.
fn with_copy_menu(id: ElementId, view: TextView) -> SelectableText {
    div().id(id).child(view).context_menu(|menu, window, cx| {
        let text = TextSelection::selected_text(window, cx).trim().to_string();
        menu.item(
            PopupMenuItem::new("Copy")
                .disabled(text.is_empty())
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                }),
        )
    })
}

/// Plain data text the user can drag-select and copy (Ctrl/Cmd+C or right-click).
pub fn selectable_text(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    _window: &mut Window,
    _cx: &mut App,
) -> SelectableText {
    let id = id.into();
    let text = text.into();
    let view = TextView::html(id.clone(), plain_text_html(&text))
        .selectable(true)
        .style(TextViewStyle::default().paragraph_gap(rems(0.)));
    with_copy_menu(id, view)
}

/// Markdown text the user can drag-select and copy (Ctrl/Cmd+C or right-click).
pub fn selectable_markdown(
    id: impl Into<ElementId>,
    markdown: impl Into<SharedString>,
    _window: &mut Window,
    cx: &mut App,
) -> SelectableText {
    let id = id.into();
    let view = TextView::markdown(id.clone(), markdown)
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
        .selectable(true);
    with_copy_menu(id, view)
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Context, IntoElement, Modifiers, MouseButton, ParentElement as _, Render,
        Styled as _, TestAppContext, VisualTestContext, Window, div, point, px,
    };
    use gpui_base::TextSelection;
    use gpui_component::Root;

    use super::selectable_text;

    struct TextHost;

    impl Render for TextHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(240.))
                .child(selectable_text("copy-test", "alpha beta", window, cx))
        }
    }

    fn drag_select(cx: &mut TestAppContext) -> &mut VisualTestContext {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|_| TextHost);
            Root::new(host, window, cx)
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (start, end) = (point(px(1.), px(8.)), point(px(230.), px(8.)));
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx
    }

    #[gpui::test]
    fn copy_shortcut_copies_a_drag_selection(cx: &mut TestAppContext) {
        let cx = drag_select(cx);
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-c"
        } else {
            "ctrl-c"
        });
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some("alpha beta"));
    }

    #[gpui::test]
    fn right_click_copy_copies_a_drag_selection(cx: &mut TestAppContext) {
        let cx = drag_select(cx);
        let at = point(px(20.), px(8.));
        cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
        // The menu is built on the next frame and opens at the click; its
        // only item, Copy, sits just below and right of it.
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_click(point(px(50.), px(24.)), Modifiers::default());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
            assert!(
                !TextSelection::has_selection(window, cx),
                "clicking the menu clears the selection, so Copy must not read it then"
            );
        });
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some("alpha beta"));
    }
}
