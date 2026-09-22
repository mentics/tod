//! Command history window — view and undo recent mutations.

use crate::app::HistoryWindowControl;
use crate::ui::actionable::render_shortcut_pill;
use crate::ui::key_context;
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyBinding, MouseButton,
    ParentElement, Pixels, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::Scrollbar;
use gpui_component::{Icon, IconName, Sizable as _, StyledExt, TitleBar, h_flex, v_flex};
use std::sync::Arc;
use tod_store::fleet::FleetStore;
use tod_store::fleet::command_log::CommandEntry;

const HISTORY_CONTEXT: &str = "CommandHistory";

/// The right-edge strip the vertical scrollbar is drawn in.
const SCROLLBAR_WIDTH: Pixels = px(16.);

actions!(
    command_history,
    [
        CommandHistoryClose,
        CommandHistoryUndo,
        CommandHistorySelectUp,
        CommandHistorySelectDown,
    ]
);

pub fn register_command_history_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(HISTORY_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("up", CommandHistorySelectUp, context),
        KeyBinding::new("down", CommandHistorySelectDown, context),
        KeyBinding::new("enter", CommandHistoryUndo, context),
        KeyBinding::new("ctrl-z", CommandHistoryUndo, context),
    ]);
    key_context::bind_panel_escape(cx, CommandHistoryClose, HISTORY_CONTEXT);
}

pub struct CommandHistoryView {
    fleet: Arc<FleetStore>,
    window_control: HistoryWindowControl,
    focus_handle: FocusHandle,
    entries: Vec<CommandEntry>,
    selected: usize,
    scroll_handle: ScrollHandle,
    status_line: String,
}

impl CommandHistoryView {
    pub fn new(
        _window: &mut Window,
        cx: &mut Context<Self>,
        fleet: Arc<FleetStore>,
        window_control: HistoryWindowControl,
    ) -> Self {
        Self {
            fleet,
            window_control,
            focus_handle: cx.focus_handle(),
            entries: Vec::new(),
            selected: 0,
            scroll_handle: ScrollHandle::new(),
            status_line: String::new(),
        }
    }

    fn reload_entries(&mut self) {
        self.entries = self
            .fleet
            .command_log()
            .lock()
            .expect("command log mutex")
            .entries()
            .iter()
            .cloned()
            .rev()
            .collect();
        if self.selected >= self.entries.len() && !self.entries.is_empty() {
            self.selected = self.entries.len() - 1;
        }
    }

    fn undo_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return;
        };
        match self.fleet.undo_through(entry.id) {
            Ok(labels) if !labels.is_empty() => {
                self.status_line = format!("Undid: {}", labels.join(", "));
            }
            Ok(_) => self.status_line = "Nothing to undo".into(),
            Err(err) => self.status_line = format!("Undo failed: {err}"),
        }
        self.reload_entries();
        cx.notify();
        let _ = window;
    }

    fn on_close(&mut self, _: &CommandHistoryClose, _: &mut Window, cx: &mut Context<Self>) {
        self.window_control.close(cx);
    }

    fn on_undo(&mut self, _: &CommandHistoryUndo, window: &mut Window, cx: &mut Context<Self>) {
        self.undo_selected(window, cx);
    }

    fn on_select_up(&mut self, _: &CommandHistorySelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected > 0 {
            self.select(self.selected - 1, cx);
        }
    }

    fn on_select_down(
        &mut self,
        _: &CommandHistorySelectDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected + 1 < self.entries.len() {
            self.select(self.selected + 1, cx);
        }
    }

    /// Move the selection and keep it on screen — arrow keys otherwise walk the
    /// highlight out of the viewport and the list looks frozen.
    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = ix;
        self.scroll_handle.scroll_to_item(ix);
        cx.notify();
    }

    fn format_time(ms: i64) -> String {
        use chrono::{TimeZone, Utc};
        Utc.timestamp_millis_opt(ms)
            .single()
            .map(|t| t.format("%H:%M:%S").to_string())
            .unwrap_or_else(|| "—".into())
    }

    /// The window is opened with `TitleBar::title_bar_options()`, which leaves
    /// it without a system caption — the view has to draw one, or the window
    /// cannot be dragged, minimized, or closed by its own chrome.
    fn render_title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .gap(style::space::RELATED)
                .child("Command history")
                .child(div().flex_1())
                // The pill sits beside the button, not under it as
                // `chrome_control_with_shortcut` puts it: a title bar has no
                // room below, and the pill lands on top of the label.
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap(style::space::INLINE)
                        .child(
                            Button::new("history-close")
                                .label("Close")
                                .ghost()
                                .compact()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.window_control.close(cx);
                                })),
                        )
                        .when_some(
                            render_shortcut_pill(window, &CommandHistoryClose, HISTORY_CONTEXT, cx),
                            |el, pill| el.child(pill),
                        ),
                ),
        )
    }

    fn render_rows(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.entries.is_empty() {
            return div()
                .flex_1()
                .min_h_0()
                .p(style::space::SECTION)
                .child(style::empty_message(div()).child("No undo history"))
                .into_any_element();
        }

        let rows = self
            .entries
            .iter()
            .enumerate()
            .map(|(ix, entry)| {
                let selected = ix == self.selected;
                let group = SharedString::from(format!("history-row-{ix}"));
                // Clicking the row only selects it. Undo is destructive and
                // takes every later command with it, so it needs its own
                // deliberate click.
                let row = h_flex()
                    .id(("history-row", ix))
                    .group(group.clone())
                    .w_full()
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.select(ix, cx)),
                    );
                style::row(row)
                    .when(selected, style::highlighted)
                    // The label takes the slack and truncates; without
                    // `min_w_0` a long one pushes the time off the window.
                    .child(div().flex_1().min_w_0().child(entry.label.clone()))
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .items_center()
                            // Hidden rather than absent, so showing the button
                            // never moves the time beside it.
                            .when(!selected, |el| {
                                el.invisible().group_hover(group, |style| style.visible())
                            })
                            .child(
                                Button::new(("history-undo", ix))
                                    .icon(Icon::new(IconName::Undo2))
                                    .label("Undo")
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Undo this command and every one after it")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.selected = ix;
                                        this.undo_selected(window, cx);
                                    })),
                            ),
                    )
                    .child(
                        style::text_dense_muted(div())
                            .flex_shrink_0()
                            .child(Self::format_time(entry.created_at)),
                    )
            })
            .collect::<Vec<_>>();

        div()
            .flex_1()
            .min_h_0()
            .relative()
            .child(
                div()
                    .id("command-history-scroll")
                    .v_flex()
                    .size_full()
                    .py(style::space::INLINE)
                    .pl(style::space::INLINE)
                    // Clear of the scrollbar strip, so the selected row's
                    // highlight edge is not hidden behind it.
                    .pr(SCROLLBAR_WIDTH + style::space::INLINE)
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .children(rows),
            )
            .child(
                // A narrow right-edge strip, not the full row area: the
                // Scrollbar installs a click-to-jump handler across its whole
                // bounds, which would swallow clicks meant for the rows.
                div()
                    .occlude()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(SCROLLBAR_WIDTH)
                    .child(Scrollbar::vertical(&self.scroll_handle)),
            )
            .into_any_element()
    }
}

impl Focusable for CommandHistoryView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CommandHistoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.reload_entries();

        style::panel(v_flex())
            .key_context(HISTORY_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_select_up))
            .on_action(cx.listener(Self::on_select_down))
            .child(self.render_title_bar(window, cx))
            .child(self.render_rows(cx))
            .when(!self.status_line.is_empty(), |el| {
                el.child(
                    // The text style goes on the wrapper: a `TextView` renders
                    // its own colour, so styling it directly does not take.
                    style::text_dense_muted(div())
                        .flex_shrink_0()
                        .px(style::space::INSET)
                        .py(style::space::RELATED)
                        .border_t(style::size::BORDER)
                        .border_color(style::color::divider())
                        .child(style::text_dense_muted(selectable_text(
                            "command-history-status",
                            self.status_line.clone(),
                            window,
                            cx,
                        ))),
                )
            })
    }
}
