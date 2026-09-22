use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav};
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
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, ParentElement, Render, SharedString, Styled,
    Subscription, Window, actions, div, px,
};
use gpui_component::button::Button;
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::{ActiveTheme, Selectable, h_flex, v_flex};
use std::sync::Arc;
use tod_store::fleet::{FleetStore, explore};

const DATABASE_CONTEXT: &str = "Database";
const SQL_INPUT_WIDTH: f32 = 640.0;
const SQL_INPUT_ROWS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatabaseStop {
    Table,
    Sql,
    Run,
    /// The result rows. While the cursor is here Up/Down move it through the
    /// list; Up off its first row returns to the controls above.
    Results,
}

const DATABASE_STOPS: [DatabaseStop; 4] = [
    DatabaseStop::Table,
    DatabaseStop::Sql,
    DatabaseStop::Run,
    DatabaseStop::Results,
];

actions!(
    database_view,
    [DatabaseRunSql, DatabaseActivate, DatabaseEscape]
);

/// One row of the result: its cells, in the result's column order.
#[derive(Debug, Clone)]
struct ResultRow {
    cells: Vec<String>,
}

/// What the user did in the results list, queued for the view to apply.
#[derive(Debug, Clone)]
enum ResultAction {
    Select {
        row_ix: usize,
    },
    /// Something the list can report but a flat, read-only one never does.
    Ignored,
}

impl From<ItemListEvent> for ResultAction {
    fn from(event: ItemListEvent) -> Self {
        match event {
            ItemListEvent::Select { row_ix } => Self::Select { row_ix },
            // The rows are a query's result, in the order it returned them.
            ItemListEvent::ToggleGroup { .. }
            | ItemListEvent::ToggleMark { .. }
            | ItemListEvent::Drop(_) => Self::Ignored,
        }
    }
}

/// The result's own columns, in its order: a query row has a value for every
/// one of them, which is what makes them columns. The last takes the slack,
/// since no column of a query result means more than another. They are keyed
/// by position, so a query that selects the same name twice still lines up.
fn result_columns(names: &[String]) -> Vec<ColumnSpec> {
    let last = names.len().saturating_sub(1);
    names
        .iter()
        .enumerate()
        .map(|(ix, name)| {
            let key = column_key(ix);
            if ix == last {
                ColumnSpec::content(key, name.clone())
            } else {
                ColumnSpec::fixed(key, name.clone(), style::size::TABLE_CELL)
            }
        })
        .collect()
}

fn column_key(ix: usize) -> String {
    format!("c{ix}")
}

pub struct DatabaseView {
    fleet: Arc<FleetStore>,
    focus_handle: FocusHandle,
    app_nav: AppNavMenu,
    tables: Vec<String>,
    table_select: Entity<SelectState<Vec<String>>>,
    sql_input: Entity<TextareaState>,
    result: explore::QueryRows,
    /// The rows, the cursor, the columns and the scrolling: everything every
    /// list in the app shares.
    list: ItemList<ResultRow>,
    host: RowHost<ResultAction>,
    status_line: SharedString,
    error: Option<String>,
    focus_stop: DatabaseStop,
    sql_editing: bool,
    _table_select_subscription: Subscription,
}

impl DatabaseView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, fleet: Arc<FleetStore>) -> Self {
        let tables = Self::load_tables(&fleet);
        let table_select =
            cx.new(|cx| SelectState::new(tables.clone(), None, window, cx).searchable(true));
        let sql_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(SQL_INPUT_ROWS)
                .placeholder("Enter to edit · SQL query (read-only)")
        });

        let _table_select_subscription = cx.subscribe(&table_select, |this, _, event, cx| {
            if let SelectEvent::Confirm(Some(table)) = event {
                this.on_table_selected(table.clone(), cx);
            }
        });

        let mut this = Self {
            fleet,
            focus_handle: cx.focus_handle(),
            app_nav: AppNavMenu::default(),
            tables,
            table_select,
            sql_input,
            result: explore::QueryRows::default(),
            list: ItemList::new(),
            host: RowHost::for_entity(cx.weak_entity()),
            status_line: SharedString::from("Select a table or run SQL"),
            error: None,
            focus_stop: DatabaseStop::Table,
            sql_editing: false,
            _table_select_subscription,
        };

        if let Some(first) = this.tables.first().cloned() {
            this.table_select.update(cx, |select, cx| {
                select.set_selected_value(&first, window, cx);
            });
            this.load_table(&first, cx);
        }

        this
    }

    fn text_editing(&self) -> bool {
        self.sql_editing
    }

    fn stop_index(stop: DatabaseStop) -> usize {
        DATABASE_STOPS.iter().position(|s| *s == stop).unwrap_or(0)
    }

    fn stop_focused(&self, stop: DatabaseStop) -> bool {
        self.focus_stop == stop && !self.text_editing()
            || (stop == DatabaseStop::Sql && self.sql_editing)
    }

    fn move_stop(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        let idx = Self::stop_index(self.focus_stop) as i32;
        let len = DATABASE_STOPS.len() as i32;
        let next = ((idx + delta).rem_euclid(len)) as usize;
        self.focus_stop = DATABASE_STOPS[next];
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn enter_sql_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_stop = DatabaseStop::Sql;
        self.sql_editing = true;
        cx.notify();
        let input = self.sql_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    fn exit_sql_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sql_editing {
            return;
        }
        self.sql_editing = false;
        self.focus_stop = DatabaseStop::Sql;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn activate_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        match self.focus_stop {
            DatabaseStop::Table => {
                self.table_select.update(cx, |select, cx| {
                    select.focus(window, cx);
                });
            }
            DatabaseStop::Sql => self.enter_sql_edit(window, cx),
            DatabaseStop::Run => self.run_sql(window, cx),
            // A result row is a read-only value: there is nothing to
            // activate.
            DatabaseStop::Results => {}
        }
    }

    fn handle_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sql_editing {
            self.exit_sql_edit(window, cx);
        }
    }

    fn load_tables(fleet: &FleetStore) -> Vec<String> {
        let projection = fleet.projection();
        let guard = projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        explore::list_tables(&conn).unwrap_or_default()
    }

    fn refresh_tables(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tables = Self::load_tables(&self.fleet);
        if tables == self.tables {
            return;
        }
        self.tables = tables;
        self.table_select.update(cx, |select, cx| {
            select.set_items(self.tables.clone(), window, cx);
        });
    }

    fn on_table_selected(&mut self, table: String, cx: &mut Context<Self>) {
        self.load_table(&table, cx);
    }

    fn load_table(&mut self, table: &str, cx: &mut Context<Self>) {
        let _ = self.fleet.reload_if_stale();
        let projection = self.fleet.projection();
        let guard = projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        match explore::query_table(&conn, table) {
            Ok(rows) => {
                let count = rows.rows.len();
                self.show_result(rows);
                self.error = None;
                self.status_line = SharedString::from(format!(
                    "Table {table} — {count} row{}",
                    if count == 1 { "" } else { "s" }
                ));
            }
            Err(err) => {
                self.show_result(explore::QueryRows::default());
                self.error = Some(err.to_string());
                self.status_line = SharedString::from(format!("Table {table}"));
            }
        }
        cx.notify();
    }

    fn run_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql = self.sql_input.read(cx).text().to_string();
        let _ = self.fleet.reload_if_stale();
        let projection = self.fleet.projection();
        let guard = projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        match explore::execute_sql(&conn, &sql, 500) {
            Ok(rows) => {
                let count = rows.rows.len();
                self.show_result(rows);
                self.error = None;
                self.status_line = SharedString::from(format!(
                    "Query — {count} row{}",
                    if count == 1 { "" } else { "s" }
                ));
            }
            Err(err) => {
                self.show_result(explore::QueryRows::default());
                self.error = Some(err.to_string());
                self.status_line = SharedString::from("Query failed");
            }
        }
        cx.notify();
        self.focus_handle.focus(window, cx);
    }

    /// Show `result`: its columns become the list's columns and its rows the
    /// list's rows. A new result starts at the top — the cursor is held by
    /// key, and one result's row keys mean nothing in another's.
    fn show_result(&mut self, result: explore::QueryRows) {
        self.list.set_columns(result_columns(&result.columns));
        self.list.set_cursor_key(None);
        self.list.set_rows(
            result
                .rows
                .iter()
                .enumerate()
                .map(|(ix, cells)| {
                    ItemListRow::item(
                        format!("r{ix}"),
                        ResultRow {
                            cells: cells.clone(),
                        },
                    )
                })
                .collect(),
        );
        self.result = result;
    }

    fn drain_row_actions(&mut self, cx: &mut Context<Self>) {
        for action in self.host.drain() {
            match action {
                ResultAction::Select { row_ix } => {
                    self.focus_stop = DatabaseStop::Results;
                    self.list.set_cursor(row_ix);
                    cx.notify();
                }
                ResultAction::Ignored => {}
            }
        }
    }

    /// Up/Down: the results list owns them while the cursor is in it, the ring
    /// of controls otherwise. Up off the list's first row hands them back.
    fn on_arrow_up(&mut self, _: &ItemListUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_stop == DatabaseStop::Results && !self.list.is_empty() {
            if self.list.move_cursor(-1) {
                cx.notify();
                return;
            }
            self.move_stop(-1, window, cx);
            return;
        }
        self.move_stop(-1, window, cx);
    }

    fn on_arrow_down(&mut self, _: &ItemListDown, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_stop == DatabaseStop::Results && !self.list.is_empty() {
            if self.list.move_cursor(1) {
                cx.notify();
            }
            return;
        }
        self.move_stop(1, window, cx);
    }

    fn on_page_up(&mut self, _: &ItemListPageUp, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<ResultRow>::page_rows(window.viewport_size().height) as i32;
        self.move_list_cursor(-page, cx);
    }

    fn on_page_down(&mut self, _: &ItemListPageDown, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<ResultRow>::page_rows(window.viewport_size().height) as i32;
        self.move_list_cursor(page, cx);
    }

    fn on_home(&mut self, _: &ItemListHome, _: &mut Window, cx: &mut Context<Self>) {
        if self.in_results() && self.list.cursor_home() {
            cx.notify();
        }
    }

    fn on_end(&mut self, _: &ItemListEnd, _: &mut Window, cx: &mut Context<Self>) {
        if self.in_results() && self.list.cursor_end() {
            cx.notify();
        }
    }

    /// Whether the list is the thing the keyboard is on.
    fn in_results(&self) -> bool {
        self.focus_stop == DatabaseStop::Results && !self.text_editing()
    }

    fn move_list_cursor(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.in_results() && self.list.move_cursor(delta) {
            cx.notify();
        }
    }

    /// One result row: a cell per column, each selectable so a value can be
    /// copied out.
    fn render_result_row(
        row: &ResultRow,
        state: ItemRowState<'_>,
        host: &RowHost<ResultAction>,
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
                select_host.push(ResultAction::Select { row_ix }.into(), cx);
            })
            .when(state.highlighted, style::highlighted)
            .children(row.cells.iter().enumerate().map(|(col_ix, cell)| {
                state
                    .column(&column_key(col_ix), style::text_dense(div()))
                    .child(selectable_text(
                        ("db-cell", row_ix * 1000 + col_ix),
                        cell.clone(),
                        window,
                        cx,
                    ))
            }))
            .into_any_element()
    }
}

impl HasAppNav for DatabaseView {
    fn app_nav_mut(&mut self) -> &mut AppNavMenu {
        &mut self.app_nav
    }

    fn app_nav_current(&self) -> Option<AppDestination> {
        Some(AppDestination::Database)
    }

    fn app_nav_fallback_focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Focusable for DatabaseView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DatabaseView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_row_actions(cx);
        self.refresh_tables(window, cx);
        key_context::set_input_tab_stop(&self.sql_input, self.sql_editing, cx);
        if !self.sql_editing && self.sql_input.read(cx).focus_handle(cx).is_focused(window) {
            self.enter_sql_edit(window, cx);
        }

        let theme = cx.theme().clone();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let danger = theme.danger;
        let list_active = theme.list_active;
        let list_active_border = theme.list_active_border;

        let status = if let Some(err) = &self.error {
            SharedString::from(format!("{err}"))
        } else {
            self.status_line.clone()
        };
        let status_color = if self.error.is_some() { danger } else { muted };

        let table_focused = self.stop_focused(DatabaseStop::Table);
        let sql_focused = self.stop_focused(DatabaseStop::Sql);
        let run_focused = self.stop_focused(DatabaseStop::Run);

        let root = v_flex()
            .key_context(DATABASE_CONTEXT)
            .size_full()
            .bg(theme.background)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_arrow_up))
            .on_action(cx.listener(Self::on_arrow_down))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .on_action(cx.listener(|this, _: &DatabaseActivate, window, cx| {
                this.activate_stop(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &DatabaseEscape, window, cx| {
                this.handle_escape(window, cx);
                cx.stop_propagation();
            }))
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .child(self.render_app_nav(window, cx)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .p_4()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_start()
                            .flex_wrap()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .when(table_focused, |el| {
                                        el.bg(list_active)
                                            .border_1()
                                            .border_color(list_active_border)
                                    })
                                    .child(div().text_sm().text_color(foreground).child("Table"))
                                    .child(
                                        Select::new(&self.table_select)
                                            .placeholder("Choose table")
                                            .menu_width(px(240.)),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w(px(SQL_INPUT_WIDTH))
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_text()
                                    .when(sql_focused, |el| {
                                        el.bg(list_active)
                                            .border_1()
                                            .border_color(list_active_border)
                                    })
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            if !this.sql_editing {
                                                this.enter_sql_edit(window, cx);
                                            }
                                        }),
                                    )
                                    .child(div().text_sm().text_color(foreground).child("SQL"))
                                    .child(
                                        Textarea::new(&self.sql_input)
                                            .disabled(!self.sql_editing)
                                            .w_full(),
                                    ),
                            )
                            .child(
                                Button::new("database-run-sql")
                                    .label("Run")
                                    .selected(run_focused)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.run_sql(window, cx);
                                    })),
                            ),
                    )
                    .child(
                        selectable_text("database-status", status, window, cx)
                            .text_sm()
                            .text_color(status_color),
                    )
                    .child(style::text_dense_muted(div()).child(
                        "↑↓ move · Enter activate · ↓ past Run moves through the rows · Esc exit SQL edit",
                    ))
                    .child(self.render_results(window, cx)),
            )
            .on_action(cx.listener(|this, _: &DatabaseRunSql, window, cx| {
                this.run_sql(window, cx);
            }));

        self.bind_app_nav_toggle(root, cx)
    }
}

impl DatabaseView {
    /// The result, as the item list: the query's columns are the list's
    /// columns, so the header names them and every cell lines up under its
    /// own.
    fn render_results(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.list.is_empty() {
            return style::empty_message(div())
                .flex_1()
                .min_h_0()
                .child(if self.error.is_some() {
                    "No results"
                } else if self.result.columns.is_empty() {
                    "No results"
                } else {
                    "No rows"
                })
                .into_any_element();
        }

        let host = self.host.clone();
        let edge = if self.in_results() {
            cx.theme().list_active_border
        } else {
            cx.theme().border
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_1()
            .border_color(edge)
            .rounded_md()
            .child(self.list.render(
                "database-results",
                &self.host,
                move |row, state, window, cx| {
                    Self::render_result_row(row, state, &host, window, cx)
                },
                window,
                cx,
            ))
            .into_any_element()
    }
}

pub fn register_database_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(DATABASE_CONTEXT));
    let input_context = Some(key_context::including_input(DATABASE_CONTEXT));
    // The results are an item list: navigation comes from the one key set. A
    // query row is a read-only value, so editing, creation, reordering,
    // marking and search stay unbound.
    bind_item_list_keys(cx, DATABASE_CONTEXT, ItemListKeys::default());
    cx.bind_keys([
        KeyBinding::new("enter", DatabaseActivate, context),
        KeyBinding::new("space", DatabaseActivate, context),
        KeyBinding::new("escape", DatabaseEscape, context),
        KeyBinding::new("escape", DatabaseEscape, input_context),
        KeyBinding::new("ctrl-enter", DatabaseRunSql, context),
        KeyBinding::new("ctrl-enter", DatabaseRunSql, input_context),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_result_columns_are_the_querys_own_and_the_last_takes_the_slack() {
        let columns = result_columns(&["id".into(), "title".into(), "status".into()]);
        let labels: Vec<&str> = columns.iter().map(|c| c.label.as_ref()).collect();
        assert_eq!(labels, vec!["id", "title", "status"]);
        assert_eq!(columns[0].width, Some(style::size::TABLE_CELL));
        assert_eq!(columns[1].width, Some(style::size::TABLE_CELL));
        // Exactly one content column, as the component requires.
        assert_eq!(columns[2].width, None);
    }

    #[test]
    fn a_query_that_names_a_column_twice_still_gets_two_of_them() {
        // Columns are keyed by position, so `SELECT a.id, b.id` lines up.
        let columns = result_columns(&["id".into(), "id".into()]);
        assert_eq!(columns[0].key.as_ref(), "c0");
        assert_eq!(columns[1].key.as_ref(), "c1");
        assert_eq!(columns[0].width, Some(style::size::TABLE_CELL));
        assert_eq!(columns[1].width, None);
    }

    #[test]
    fn a_single_column_result_is_all_content() {
        let columns = result_columns(&["count(*)".into()]);
        assert_eq!(columns.len(), 1);
        assert_eq!(columns[0].width, None);
    }

    #[test]
    fn no_columns_at_all_declares_none() {
        assert!(result_columns(&[]).is_empty());
    }

    #[test]
    fn the_rows_are_the_last_stop_so_down_from_run_reaches_them() {
        assert_eq!(
            DATABASE_STOPS.last(),
            Some(&DatabaseStop::Results),
            "Down past Run is how the keyboard gets into the results"
        );
        assert_eq!(
            DatabaseView::stop_index(DatabaseStop::Results),
            DATABASE_STOPS.len() - 1
        );
    }
}
