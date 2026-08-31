use tod_store::fleet::{FleetStore, explore};
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav};
use crate::ui::selectable_text::selectable_text;
use gpui::{
    AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, Window, actions, div, px,
};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use std::sync::Arc;

const SQL_INPUT_WIDTH: f32 = 640.0;
const SQL_INPUT_ROWS: usize = 4;

actions!(database_view, [DatabaseRunSql]);

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
                .placeholder("SQL query (read-only)")
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

        let theme = cx.theme().clone();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let danger = theme.danger;
        let muted_bg = theme.muted;

        let status = if let Some(err) = &self.error {
            SharedString::from(format!("{err}"))
        } else {
            self.status_line.clone()
        };
        let status_color = if self.error.is_some() { danger } else { muted };

        let root = v_flex()
            .size_full()
            .bg(theme.background)
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
                                    .child(div().text_sm().text_color(foreground).child("SQL"))
                                    .child(Input::new(&self.sql_input).w_full()),
                            )
                            .child(Button::new("database-run-sql").label("Run").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.run_sql(window, cx);
                                }),
                            )),
                    )
                    .child(
                        selectable_text("database-status", status, window, cx)
                            .text_sm()
                            .text_color(status_color),
                    )
                    .child(self.render_results_table(window, cx, border, foreground, muted, muted_bg)),
            )
            .track_focus(&self.focus_handle);

        self.bind_app_nav_toggle(
            root.on_action(cx.listener(|this, _: &DatabaseRunSql, window, cx| {
                this.run_sql(window, cx);
            })),
            cx,
        )
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
