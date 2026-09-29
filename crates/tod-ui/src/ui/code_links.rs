//! `path:line` references in data text become links that open the code.
//!
//! [`selectable_text`](super::selectable_text) and `selectable_markdown` link
//! every reference [`find_code_refs`] finds, so an agent's "see
//! `src/foo.rs:42`" can be clicked wherever it is shown. A click dispatches
//! [`OpenCodeRef`]; the view that knows which node the text is about handles
//! it with [`open_code_ref`], and the shell root is the fallback (the task
//! tree's selection). A relative path is resolved against that node's Files
//! directory.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, AppContext as _, ClickEvent, MouseButton, SharedString, Window};
use tod_store::fleet::{
    CodeLocation, FleetStore, code_editors, find_code_refs, open_code_location,
};
use uuid::Uuid;

use tod_agent::devcontainer::{NoSshd, install_sshd};

use crate::ui::toast::{error_toast, error_toast_with_action, info_toast};

/// Open a code reference clicked in text. Dispatched by the link, handled by
/// the view that knows the node; never bound to a key.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = code_links, no_json)]
pub struct OpenCodeRef {
    /// The link target: `path`, `path:line`, or `path:line:column`.
    pub target: SharedString,
}

/// The link handler for text views: a code reference dispatches
/// [`OpenCodeRef`], a URL opens in the browser.
pub fn on_link_click(url: &SharedString, event: &ClickEvent, window: &mut Window, cx: &mut App) {
    let opens = match event {
        ClickEvent::Mouse(click) => click.up.button == MouseButton::Left,
        ClickEvent::Keyboard(_) => true,
        _ => false,
    };
    if !opens {
        return;
    }
    if CodeLocation::parse(url).is_some() {
        window.dispatch_action(
            Box::new(OpenCodeRef {
                target: url.clone(),
            }),
            cx,
        );
    } else {
        cx.open_url(url);
    }
}

/// Open `target` in the first available code editor, off the UI thread,
/// resolving a relative path against `node`'s Files directory. Failures show
/// as an error toast.
pub fn open_code_ref(
    fleet: Arc<FleetStore>,
    node: Option<Uuid>,
    target: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(location) = CodeLocation::parse(target) else {
        return;
    };
    let Some(editor) = code_editors().first().copied() else {
        error_toast(window, cx, "No code editor is supported on this machine");
        return;
    };
    let task = cx.background_spawn(async move {
        let node = node.map(|node| node.to_string());
        open_code_location(&fleet, editor, node.as_deref(), &location)
    });
    window
        .spawn(cx, async move |cx| {
            if let Err(err) = task.await {
                let _ = cx.update(|window, cx| {
                    let message = format!("Open code failed: {err:#}");
                    let Some(missing) = err.downcast_ref::<NoSshd>() else {
                        return error_toast(window, cx, message);
                    };
                    let container = missing.container.clone();
                    let action: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |window, cx| {
                        install_sshd_in(container.clone(), window, cx);
                    });
                    error_toast_with_action(
                        window,
                        cx,
                        message,
                        Some(("Install OpenSSH in the container".into(), action)),
                    );
                });
            }
        })
        .detach();
}

/// Install OpenSSH in `container` off the UI thread, and say how it went.
fn install_sshd_in(container: String, window: &mut Window, cx: &mut App) {
    info_toast(window, cx, "Installing OpenSSH in the container…");
    let task = cx.background_spawn(async move { install_sshd(&container) });
    window
        .spawn(cx, async move |cx| {
            let result = task.await;
            let _ = cx.update(|window, cx| match result {
                Ok(()) => info_toast(window, cx, "OpenSSH installed. Open the code again."),
                Err(err) => error_toast(window, cx, format!("{err:#}")),
            });
        })
        .detach();
}

/// Link every code reference in `markdown`, leaving code blocks and existing
/// links alone. An inline code span that is exactly one reference becomes a
/// link around the span.
pub fn linkify_markdown(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut fence: Option<String> = None;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker: String = trimmed
            .chars()
            .take_while(|c| *c == '`' || *c == '~')
            .collect();
        if let Some(open) = &fence {
            if marker.len() >= open.len() && marker.starts_with(&open[..1]) {
                fence = None;
            }
            out.push_str(line);
            continue;
        }
        if marker.len() >= 3
            && (marker.chars().all(|c| c == '`') || marker.chars().all(|c| c == '~'))
        {
            fence = Some(marker);
            out.push_str(line);
            continue;
        }
        linkify_markdown_line(line, &mut out);
    }
    out
}

fn linkify_markdown_line(line: &str, out: &mut String) {
    let bytes = line.as_bytes();
    let mut text_start = 0;
    let mut i = 0;
    while i < line.len() {
        match bytes[i] {
            b'`' => {
                let run = line[i..].bytes().take_while(|b| *b == b'`').count();
                let ticks = &line[i..i + run];
                let Some(close) = line[i + run..].find(ticks) else {
                    i += run;
                    continue;
                };
                let content_end = i + run + close;
                let span_end = content_end + run;
                push_linked_text(&line[text_start..i], out);
                let content = &line[i + run..content_end];
                let refs = find_code_refs(content.trim());
                match refs.as_slice() {
                    [(range, _)] if range.len() == content.trim().len() => {
                        out.push('[');
                        out.push_str(&line[i..span_end]);
                        out.push_str("](");
                        out.push_str(content.trim());
                        out.push(')');
                    }
                    _ => out.push_str(&line[i..span_end]),
                }
                i = span_end;
                text_start = i;
            }
            b'[' => {
                // An existing `[text](target)` link is left as written.
                let Some(end) = link_end(line, i) else {
                    i += 1;
                    continue;
                };
                push_linked_text(&line[text_start..i], out);
                out.push_str(&line[i..end]);
                i = end;
                text_start = i;
            }
            b'<' => {
                // An autolink or inline HTML.
                let Some(close) = line[i..].find('>') else {
                    i += 1;
                    continue;
                };
                push_linked_text(&line[text_start..i], out);
                out.push_str(&line[i..i + close + 1]);
                i += close + 1;
                text_start = i;
            }
            _ => i += 1,
        }
    }
    push_linked_text(&line[text_start..], out);
}

/// Where the `[text](target)` link starting at `start` ends, if it is one.
fn link_end(line: &str, start: usize) -> Option<usize> {
    let close = start + line[start..].find("](")?;
    let target_end = close + 2 + line[close + 2..].find(')')?;
    Some(target_end + 1)
}

fn push_linked_text(text: &str, out: &mut String) {
    let mut at = 0;
    for (range, _) in find_code_refs(text) {
        out.push_str(&text[at..range.start]);
        let reference = &text[range.clone()];
        out.push('[');
        out.push_str(reference);
        out.push_str("](");
        out.push_str(reference);
        out.push(')');
        at = range.end;
    }
    out.push_str(&text[at..]);
}

/// `line` as escaped HTML with every code reference linked.
pub fn linkify_html_line(line: &str, escape: impl Fn(&str) -> String) -> String {
    let mut out = String::new();
    let mut at = 0;
    for (range, _) in find_code_refs(line) {
        out.push_str(&escape(&line[at..range.start]));
        let reference = escape(&line[range.clone()]);
        out.push_str("<a href=\"");
        out.push_str(&reference);
        out.push_str("\">");
        out.push_str(&reference);
        out.push_str("</a>");
        at = range.end;
    }
    out.push_str(&escape(&line[at..]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_references_in_prose() {
        assert_eq!(
            linkify_markdown("See src/foo.rs:42 and a.rs:1:2."),
            "See [src/foo.rs:42](src/foo.rs:42) and [a.rs:1:2](a.rs:1:2)."
        );
    }

    #[test]
    fn links_a_code_span_that_is_one_reference() {
        assert_eq!(
            linkify_markdown("In `src/foo.rs:42`, x"),
            "In [`src/foo.rs:42`](src/foo.rs:42), x"
        );
        assert_eq!(linkify_markdown("`let x = a.rs:3;`"), "`let x = a.rs:3;`");
    }

    #[test]
    fn leaves_code_blocks_and_links_alone() {
        let block = "```\nsrc/foo.rs:42\n```\nafter b.rs:1\n";
        assert_eq!(
            linkify_markdown(block),
            "```\nsrc/foo.rs:42\n```\nafter [b.rs:1](b.rs:1)\n"
        );
        let link = "[the file](src/foo.rs:42) and <https://x.dev/a.rs:1>";
        assert_eq!(linkify_markdown(link), link);
    }

    #[test]
    fn links_references_in_html_lines() {
        let escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;");
        assert_eq!(
            linkify_html_line("a < b at x.rs:3", escape),
            "a &lt; b at <a href=\"x.rs:3\">x.rs:3</a>"
        );
    }
}

#[cfg(test)]
mod click_tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{
        AppContext as _, Context, FocusHandle, InteractiveElement as _, IntoElement, Modifiers,
        ParentElement as _, Render, Styled as _, TestAppContext, Window, div, point, px,
    };
    use gpui_component::Root;

    use super::OpenCodeRef;
    use crate::ui::selectable_text::selectable_text;

    struct LinkHost {
        focus: FocusHandle,
        opened: Rc<RefCell<Vec<String>>>,
    }

    impl Render for LinkHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let opened = self.opened.clone();
            div()
                .track_focus(&self.focus)
                .w(px(240.))
                .on_action(move |action: &OpenCodeRef, _, _| {
                    opened.borrow_mut().push(action.target.to_string());
                })
                .child(selectable_text("link-test", "a.rs:3", window, cx))
        }
    }

    #[gpui::test]
    fn clicking_a_reference_dispatches_open_code_ref(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let opened = Rc::new(RefCell::new(Vec::new()));
        let host_opened = opened.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|cx| LinkHost {
                focus: cx.focus_handle(),
                opened: host_opened,
            });
            let focus = host.read(cx).focus.clone();
            window.focus(&focus, cx);
            Root::new(host, window, cx)
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_click(point(px(6.), px(8.)), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(*opened.borrow(), vec!["a.rs:3".to_string()]);
    }
}
