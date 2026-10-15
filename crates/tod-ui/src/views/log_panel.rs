//! The log panel: everything tod wants the user to see went wrong (and what
//! the status bar said), in one window.
//!
//! The entries live in `tod_core::logging::buffer`; this view only reads
//! them. A plain scrollable list rather than `ui::item_list`: rows are
//! variable height (a long message expands in place) and the list is
//! read-only with no cursor, selection, or reordering for that component to
//! own. Its Copy and Clear actions are buttons in the header; the text is
//! selectable. Keyboard: no text input here, so Escape closes the window.

use crate::app::LogWindowControl;
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, ClipboardItem, Context, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Window, actions, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{TitleBar, h_flex, v_flex};
use std::collections::HashSet;
use std::time::Duration;
use tod_core::logging::buffer::{self, Level3, LogEntry, LogFilter};

const LOG_CONTEXT: &str = "LogPanel";
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// A message longer than this many characters is cut until expanded.
const COLLAPSED_CHARS: usize = 160;

actions!(log_panel, [LogPanelClose]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", LogPanelClose, Some(LOG_CONTEXT))]);
}

pub struct LogPanelView {
    focus_handle: FocusHandle,
    filter: LogFilter,
    expanded: HashSet<u64>,
    seen_generation: u64,
    window_control: LogWindowControl,
    _poll: gpui::Task<()>,
}

/// `HH:MM:SS` (UTC) of an entry.
fn clock(entry: &LogEntry) -> String {
    let secs = entry
        .at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{:02}:{:02}:{:02}", secs / 3600 % 24, secs / 60 % 60, secs % 60)
}

/// One entry as plain text, for Copy.
pub fn entry_text(entry: &LogEntry) -> String {
    format!(
        "{} {} [{}] {}",
        clock(entry),
        entry.level.label(),
        entry.source.label(),
        entry.message
    )
}

impl LogPanelView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, control: LogWindowControl) -> Self {
        // Opening the panel reads everything so far.
        mark_read();
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let alive = this.update(cx, |this, cx| {
                    let generation = buffer::generation();
                    if generation != this.seen_generation {
                        this.seen_generation = generation;
                        // The panel is open: what arrives is being seen.
                        mark_read();
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            focus_handle,
            filter: LogFilter::All,
            expanded: HashSet::new(),
            seen_generation: buffer::generation(),
            window_control: control,
            _poll: poll,
        }
    }

    fn set_filter(&mut self, filter: LogFilter, cx: &mut Context<Self>) {
        self.filter = filter;
        cx.notify();
    }

    fn copy_all(&self, cx: &mut Context<Self>) {
        let text = buffer::snapshot(self.filter)
            .iter()
            .map(entry_text)
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn render_row(&self, entry: &LogEntry, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let seq = entry.seq;
        let long = entry.message.chars().count() > COLLAPSED_CHARS;
        let open = self.expanded.contains(&seq);
        let shown: SharedString = if long && !open {
            let cut: String = entry.message.chars().take(COLLAPSED_CHARS).collect();
            format!("{}…", cut.lines().next().unwrap_or("")).into()
        } else {
            entry.message.clone().into()
        };
        let text = entry_text(entry);
        let row = style::row_wrapped(h_flex().id(("log-row", seq as usize)))
            .w_full()
            .items_start()
            .when(entry.level == Level3::Error, style::log_row_error)
            .when(entry.level == Level3::Warn, style::log_row_warning)
            .child(
                style::text_dense_muted(div())
                    .w(style::size::TIMESTAMP_COLUMN)
                    .flex_shrink_0()
                    .child(clock(entry)),
            )
            .child(div().w(style::size::TIMESTAMP_COLUMN).flex_shrink_0().child(entry.level.label()))
            .child(
                style::text_dense_muted(div())
                    .w(style::size::TIMESTAMP_COLUMN)
                    .flex_shrink_0()
                    .child(entry.source.label()),
            )
            .child(div().flex_1().min_w_0().child(selectable_text(
                SharedString::from(format!("log-msg-{seq}")),
                shown,
                window,
                cx,
            )))
            .when(long, |el| {
                el.child(
                    Button::new(("log-expand", seq as usize))
                        .ghost()
                        .compact()
                        .label(if open { "Less" } else { "More" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.expanded.remove(&seq) {
                                this.expanded.insert(seq);
                            }
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new(("log-copy", seq as usize))
                    .ghost()
                    .compact()
                    .label("Copy")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                    })),
            );
        row.into_any_element()
    }
}

/// The log's unread mark: entries before it have been seen.
pub fn mark_read() {
    UNREAD_FROM.store(buffer::next_seq(), std::sync::atomic::Ordering::Release);
}

static UNREAD_FROM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// (warnings and errors, errors) since the panel was last open.
pub fn unread() -> (usize, usize) {
    buffer::unread_since(UNREAD_FROM.load(std::sync::atomic::Ordering::Acquire))
}

impl Focusable for LogPanelView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for LogPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = buffer::snapshot(self.filter);
        let filter = self.filter;
        let rows: Vec<AnyElement> = entries
            .iter()
            .rev() // newest first: what just broke is at the top
            .map(|e| self.render_row(e, window, cx))
            .collect();
        let empty = rows.is_empty();
        style::panel(v_flex())
            .key_context(LOG_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(|this, _: &LogPanelClose, _, cx| this.window_control.close(cx)))
            .child(
                TitleBar::new().child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap(style::space::RELATED)
                        .child("Log")
                        .child(div().flex_1())
                        .child(
                            Button::new("log-close")
                                .label("Close")
                                .ghost()
                                .compact()
                                .on_click(cx.listener(|this, _, _, cx| this.window_control.close(cx))),
                        ),
                ),
            )
            .child(
                style::panel_header(h_flex())
                    .items_center()
                    .child(
                        Button::new("log-filter-all")
                            .label("All")
                            .compact()
                            .when(filter == LogFilter::All, |b| b.primary())
                            .when(filter != LogFilter::All, |b| b.outline())
                            .on_click(cx.listener(|this, _, _, cx| this.set_filter(LogFilter::All, cx))),
                    )
                    .child(
                        Button::new("log-filter-problems")
                            .label("Errors + warnings")
                            .compact()
                            .when(filter == LogFilter::ErrorsAndWarnings, |b| b.primary())
                            .when(filter != LogFilter::ErrorsAndWarnings, |b| b.outline())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_filter(LogFilter::ErrorsAndWarnings, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("log-copy-all")
                            .label("Copy all")
                            .outline()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| this.copy_all(cx))),
                    )
                    .child(
                        Button::new("log-clear")
                            .label("Clear")
                            .outline()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                buffer::clear();
                                crate::ui::journey::record_nav(
                                    cx,
                                    tod_journey::NavEvent::Keystroke {
                                        action: "LogPanelClear".into(),
                                        keystroke: "click".into(),
                                    },
                                );
                                this.expanded.clear();
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("log-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(empty, |el| {
                        el.child(style::text_dense_muted(div()).p(style::space::INSET).child("Nothing logged."))
                    })
                    .children(rows),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_core::logging::buffer::LogSource;

    #[test]
    fn entry_text_has_time_level_source_and_message() {
        let entry = LogEntry {
            seq: 0,
            at: std::time::UNIX_EPOCH + Duration::from_secs(3661),
            level: Level3::Error,
            source: LogSource::Agentd,
            message: "boom".into(),
            notified: false,
        };
        assert_eq!(entry_text(&entry), "01:01:01 ERROR [agentd] boom");
    }
}
