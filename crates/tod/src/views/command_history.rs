//! Command history window — view and undo recent mutations.

use crate::app::HistoryWindowControl;
use crate::fleet::FleetStore;
use crate::fleet::command_log::CommandEntry;
use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::key_context;
use crate::ui::selectable_text::selectable_text;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyBinding, MouseButton,
    ParentElement, Render, Styled, Window, actions, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use std::sync::Arc;

const HISTORY_CONTEXT: &str = "CommandHistory";

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
            self.selected -= 1;
            cx.notify();
        }
    }

    fn on_select_down(
        &mut self,
        _: &CommandHistorySelectDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
            cx.notify();
        }
    }

    fn format_time(ms: i64) -> String {
        use chrono::{TimeZone, Utc};
        Utc.timestamp_millis_opt(ms)
            .single()
            .map(|t| t.format("%H:%M:%S").to_string())
            .unwrap_or_else(|| "—".into())
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
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;

        v_flex()
            .key_context(HISTORY_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_select_up))
            .on_action(cx.listener(Self::on_select_down))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .bg(theme.secondary)
                    .child(div().text_sm().font_semibold().child("Command history"))
                    .child(div().flex_1())
                    .child(chrome_control_with_shortcut(
                        Button::new("history-close")
                            .label("Close")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.window_control.close(cx);
                            })),
                        window,
                        &CommandHistoryClose,
                        HISTORY_CONTEXT,
                        cx,
                    )),
            )
            .child(div().flex_1().min_h_0().overflow_y_scrollbar().child(
                if self.entries.is_empty() {
                    div()
                        .p_4()
                        .text_color(muted)
                        .child("No undo history")
                        .into_any_element()
                } else {
                    v_flex()
                        .gap_0()
                        .children(self.entries.iter().enumerate().map(|(ix, entry)| {
                            let selected = ix == self.selected;
                            let label = entry.label.clone();
                            let time = Self::format_time(entry.created_at);
                            let bg = if selected {
                                theme.list_active
                            } else {
                                theme.background
                            };
                            div()
                                .id(("history-row", ix))
                                .px_3()
                                .py_2()
                                .bg(bg)
                                .border_b_1()
                                .border_color(border)
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        this.selected = ix;
                                        this.undo_selected(window, cx);
                                    }),
                                )
                                .child(
                                    h_flex()
                                        .justify_between()
                                        .gap_2()
                                        .child(div().text_sm().child(label))
                                        .child(div().text_xs().text_color(muted).child(time)),
                                )
                                .into_any_element()
                        }))
                        .into_any_element()
                },
            ))
            .when(!self.status_line.is_empty(), |el| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(muted)
                        .border_t_1()
                        .border_color(border)
                        .child(
                            selectable_text(
                                "command-history-status",
                                self.status_line.clone(),
                                window,
                                cx,
                            )
                            .text_xs()
                            .text_color(muted),
                        ),
                )
            })
    }
}
