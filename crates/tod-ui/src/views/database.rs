use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav};
use crate::ui::key_context;
use crate::ui::selectable_text::selectable_text;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Render, SharedString, Styled, Subscription, Window, actions, div,
    px,
};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::{ActiveTheme, Selectable, StyledExt, h_flex, v_flex};
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
}

const DATABASE_STOPS: [DatabaseStop; 3] =
    [DatabaseStop::Table, DatabaseStop::Sql, DatabaseStop::Run];

actions!(
    database_view,
    [
        DatabaseRunSql,
        DatabaseStopUp,
        DatabaseStopDown,
        DatabaseActivate,
        DatabaseEscape,
    ]
);

pub struct DatabaseView {
    fleet: Arc<FleetStore>,
    focus_handle: FocusHandle,
    app_nav: AppNavMenu,
    tables: Vec<String>,
    table_select: Entity<SelectState<Vec<String>>>,
    sql_input: Entity<InputState>,
    result: explore::QueryRows,
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
            InputState::new(window, cx)
                .multi_line(true)
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
        self.focus_handle.focus(window);
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
        self.focus_handle.focus(window);
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
                self.result = rows;
                self.error = None;
                self.status_line = SharedString::from(format!(
                    "Table {table} — {count} row{}",
                    if count == 1 { "" } else { "s" }
                ));
            }
            Err(err) => {
                self.result = explore::QueryRows::default();
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
                self.result = rows;
                self.error = None;
                self.status_line = SharedString::from(format!(
                    "Query — {count} row{}",
                    if count == 1 { "" } else { "s" }
                ));
            }
            Err(err) => {
                self.result = explore::QueryRows::default();
                self.error = Some(err.to_string());
                self.status_line = SharedString::from("Query failed");
            }
        }
        cx.notify();
        self.focus_handle.focus(window);
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
        let muted_bg = theme.muted;
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
            .on_action(cx.listener(|this, _: &DatabaseStopUp, window, cx| {
                this.move_stop(-1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &DatabaseStopDown, window, cx| {
                this.move_stop(1, window, cx);
                cx.stop_propagation();
            }))
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
                                        Input::new(&self.sql_input)
                                            .disabled(!self.sql_editing)
                                            .focus_bordered(self.sql_editing)
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
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("↑↓ control · Enter activate · Esc exit SQL edit"),
                    )
                    .child(
                        self.render_results_table(window, cx, border, foreground, muted, muted_bg),
                    ),
            )
            .on_action(cx.listener(|this, _: &DatabaseRunSql, window, cx| {
                this.run_sql(window, cx);
            }));

        self.bind_app_nav_toggle(root, cx)
    }
}

impl DatabaseView {
    fn render_results_table(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        border: gpui::Hsla,
        foreground: gpui::Hsla,
        muted: gpui::Hsla,
        muted_bg: gpui::Hsla,
    ) -> impl IntoElement {
        if self.result.columns.is_empty() && self.error.is_none() {
            return div()
                .flex_1()
                .min_h_0()
                .text_sm()
                .text_color(muted)
                .child("No results")
                .into_any_element();
        }

        let header = h_flex()
            .border_b_1()
            .border_color(border)
            .bg(muted_bg)
            .children(
                self.result
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(col_ix, name)| {
                        div()
                            .id(("db-col-header", col_ix))
                            .min_w(px(120.))
                            .max_w(px(360.))
                            .px_2()
                            .py_1()
                            .overflow_hidden()
                            .child(
                                selectable_text(("db-col-name", col_ix), name.clone(), window, cx)
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(foreground),
                            )
                    }),
            );

        let body = v_flex().children(self.result.rows.iter().enumerate().map(|(row_ix, row)| {
            h_flex()
                .id(("db-row", row_ix))
                .border_b_1()
                .border_color(border)
                .children(row.iter().enumerate().map(|(col_ix, cell)| {
                    div()
                        .id(("db-cell", row_ix * 1000 + col_ix))
                        .min_w(px(120.))
                        .max_w(px(360.))
                        .px_2()
                        .py_1()
                        .overflow_hidden()
                        .child(
                            selectable_text(
                                ("db-cell-text", row_ix * 1000 + col_ix),
                                cell.clone(),
                                window,
                                cx,
                            )
                            .text_xs()
                            .text_color(foreground),
                        )
                }))
        }));

        div()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .border_1()
            .border_color(border)
            .rounded_md()
            .child(
                v_flex()
                    .size_full()
                    .overflow_y_scrollbar()
                    .child(header)
                    .child(body),
            )
            .into_any_element()
    }
}

pub fn register_database_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(DATABASE_CONTEXT));
    let input_context = Some(key_context::including_input(DATABASE_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("up", DatabaseStopUp, context),
        KeyBinding::new("down", DatabaseStopDown, context),
        KeyBinding::new("enter", DatabaseActivate, context),
        KeyBinding::new("space", DatabaseActivate, context),
        KeyBinding::new("escape", DatabaseEscape, context),
        KeyBinding::new("escape", DatabaseEscape, input_context),
        KeyBinding::new("ctrl-enter", DatabaseRunSql, context),
        KeyBinding::new("ctrl-enter", DatabaseRunSql, input_context),
    ]);
}
