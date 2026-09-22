//! Command history window — view and undo recent mutations.
//!
//! The list itself — the cursor, the keys, the scrolling, the column header —
//! is [`crate::ui::item_list`]. Only what a history entry *is* lives here: a
//! time and what changed, and undoing through it.

use crate::app::HistoryWindowControl;
use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::item_list::keyboard::{
    ItemListDown, ItemListEnd, ItemListHome, ItemListPageDown, ItemListPageUp, ItemListUp,
};
use crate::ui::item_list::{
    ColumnSpec, ItemList, ItemListEvent, ItemListKeys, ItemListRow, ItemRowState,
    bind_item_list_keys,
};
use crate::ui::key_context;
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::views::rows::RowHost;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyBinding,
    MouseButton, ParentElement, Render, Styled, Window, actions, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use std::sync::Arc;
use tod_store::fleet::FleetStore;
use tod_store::fleet::command_log::CommandEntry;
use uuid::Uuid;

const HISTORY_CONTEXT: &str = "CommandHistory";

const COLUMN_TIME: &str = "time";
const COLUMN_CHANGE: &str = "change";

/// The history is a table of two values every entry has: when it happened and
/// what it changed. Nothing an entry only sometimes carries, so nothing stays
/// in the content column as trailing context.
fn history_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::fixed(COLUMN_TIME, COLUMN_TIME, style::size::TIMESTAMP_COLUMN),
        ColumnSpec::content(COLUMN_CHANGE, COLUMN_CHANGE),
    ]
}

actions!(command_history, [CommandHistoryClose, CommandHistoryUndo]);

pub fn register_command_history_keyboard_bindings(cx: &mut App) {
    // A history entry cannot be edited, created, reordered or marked — it
    // already happened — so the window takes navigation and nothing else from
    // the one key set.
    bind_item_list_keys(cx, HISTORY_CONTEXT, ItemListKeys::default());
    let context = Some(key_context::excluding_input(HISTORY_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("enter", CommandHistoryUndo, context),
        KeyBinding::new("ctrl-z", CommandHistoryUndo, context),
    ]);
    key_context::bind_panel_escape(cx, CommandHistoryClose, HISTORY_CONTEXT);
}

/// One entry in the list: what it changed, and when.
#[derive(Debug, Clone)]
struct HistoryItem {
    id: Uuid,
    label: String,
    time: String,
}

/// What the user did in the list, queued for the view to apply.
#[derive(Debug, Clone)]
enum HistoryAction {
    Select { row_ix: usize },
    /// Something the list can report but a flat, read-only one never does.
    Ignored,
}

impl From<ItemListEvent> for HistoryAction {
    fn from(event: ItemListEvent) -> Self {
        match event {
            ItemListEvent::Select { row_ix } => Self::Select { row_ix },
            ItemListEvent::ToggleGroup { .. } | ItemListEvent::ToggleMark { .. } => Self::Ignored,
        }
    }
}

pub struct CommandHistoryView {
    fleet: Arc<FleetStore>,
    window_control: HistoryWindowControl,
    focus_handle: FocusHandle,
    /// The rows, the cursor and the scrolling: everything every list in the
    /// app shares.
    list: ItemList<HistoryItem>,
    host: RowHost<HistoryAction>,
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
            list: ItemList::new().with_columns(history_columns()),
            host: RowHost::for_entity(cx.weak_entity()),
            status_line: String::new(),
        }
    }

    fn entries(&self) -> Vec<CommandEntry> {
        self.fleet
            .command_log()
            .lock()
            .expect("command log mutex")
            .entries()
            .iter()
            .cloned()
            .rev()
            .collect()
    }

    /// Rebuild the rows from the log. The list keeps the cursor on the entry
    /// it was on, by id, so undoing one does not move it somewhere unrelated.
    fn reload_entries(&mut self) {
        let rows = self
            .entries()
            .into_iter()
            .map(|entry| {
                ItemListRow::item(
                    entry.id.to_string(),
                    HistoryItem {
                        id: entry.id,
                        time: Self::format_time(entry.created_at),
                        label: entry.label,
                    },
                )
            })
            .collect();
        self.list.set_rows(rows);
    }

    fn undo_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.list.cursor_item().map(|item| item.id) else {
            return;
        };
        match self.fleet.undo_through(id) {
            Ok(labels) if !labels.is_empty() => {
                self.status_line = format!("Undid: {}", labels.join(", "));
            }
            Ok(_) => self.status_line = "Nothing to undo".into(),
            Err(err) => self.status_line = format!("Undo failed: {err}"),
        }
        self.reload_entries();
        cx.notify();
    }

    fn drain_row_actions(&mut self, cx: &mut Context<Self>) {
        for action in self.host.drain() {
            match action {
                HistoryAction::Select { row_ix } => {
                    if self.list.set_cursor(row_ix) {
                        cx.notify();
                    }
                }
                HistoryAction::Ignored => {}
            }
        }
    }

    fn on_close(&mut self, _: &CommandHistoryClose, _: &mut Window, cx: &mut Context<Self>) {
        self.window_control.close(cx);
    }

    fn on_undo(&mut self, _: &CommandHistoryUndo, _: &mut Window, cx: &mut Context<Self>) {
        self.undo_cursor(cx);
    }

    fn on_arrow_up(&mut self, _: &ItemListUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(-1, cx);
    }

    fn on_arrow_down(&mut self, _: &ItemListDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(1, cx);
    }

    fn on_page_up(&mut self, _: &ItemListPageUp, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<HistoryItem>::page_rows(window.viewport_size().height) as i32;
        self.move_cursor(-page, cx);
    }

    fn on_page_down(&mut self, _: &ItemListPageDown, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<HistoryItem>::page_rows(window.viewport_size().height) as i32;
        self.move_cursor(page, cx);
    }

    fn on_home(&mut self, _: &ItemListHome, _: &mut Window, cx: &mut Context<Self>) {
        if self.list.cursor_home() {
            cx.notify();
        }
    }

    fn on_end(&mut self, _: &ItemListEnd, _: &mut Window, cx: &mut Context<Self>) {
        if self.list.cursor_end() {
            cx.notify();
        }
    }

    fn move_cursor(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.list.move_cursor(delta) {
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

    /// One entry: when it happened, and what it changed.
    fn render_entry(
        item: &HistoryItem,
        state: ItemRowState<'_>,
        host: &RowHost<HistoryAction>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let row_ix = state.row_ix;
        let select_host = host.clone();
        style::row(h_flex())
            .w_full()
            .items_center()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                select_host.push(HistoryAction::Select { row_ix }.into(), cx);
            })
            .when(state.highlighted, style::highlighted)
            .child(
                state
                    .column(COLUMN_TIME, style::text_dense_muted(div()))
                    .child(selectable_text(
                        format!("history-time-{}", item.id),
                        item.time.clone(),
                        window,
                        cx,
                    )),
            )
            .child(
                state
                    .column(COLUMN_CHANGE, div())
                    .child(selectable_text(
                        format!("history-label-{}", item.id),
                        item.label.clone(),
                        window,
                        cx,
                    )),
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
        self.drain_row_actions(cx);
        self.reload_entries();
        let theme = cx.theme();
        let border = theme.border;

        v_flex()
            .key_context(HISTORY_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_arrow_up))
            .on_action(cx.listener(Self::on_arrow_down))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
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
            .child(if self.list.is_empty() {
                style::empty_message(div())
                    .p(style::space::INSET)
                    .child("No undo history")
                    .into_any_element()
            } else {
                let host = self.host.clone();
                self.list.render(
                    "command-history-list",
                    &self.host,
                    move |item, state, window, cx| {
                        Self::render_entry(item, state, &host, window, cx)
                    },
                    window,
                    cx,
                )
            })
            .child(
                style::panel_footer(v_flex())
                    .flex_shrink_0()
                    .child(
                        style::text_dense_muted(div())
                            .child("↑↓ select · Enter or Ctrl+Z undo through the selected change"),
                    )
                    .when(!self.status_line.is_empty(), |el| {
                        el.child(
                            style::text_dense_muted(div()).child(selectable_text(
                                "command-history-status",
                                self.status_line.clone(),
                                window,
                                cx,
                            )),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_history_is_a_table_of_time_and_what_changed() {
        let columns = history_columns();
        let labels: Vec<&str> = columns.iter().map(|c| c.label.as_ref()).collect();
        assert_eq!(labels, vec![COLUMN_TIME, COLUMN_CHANGE]);
        assert_eq!(columns[0].width, Some(style::size::TIMESTAMP_COLUMN));
        // The label is the content column: exactly one, as the component
        // requires.
        assert_eq!(columns[1].width, None);
    }

    #[test]
    fn a_time_it_cannot_read_still_leaves_the_column_filled() {
        assert_eq!(CommandHistoryView::format_time(i64::MAX), "—");
    }
}
