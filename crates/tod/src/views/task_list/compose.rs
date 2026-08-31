use gpui::{Context, ParentElement, Styled, Window, div};
use gpui_component::ActiveTheme;
use gpui_component::input::{Input, InputState};

use super::TaskListView;
use super::from_ticket::is_ticket_id;

impl TaskListView {
    pub(super) fn open_compose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_chrome_overlays(cx);
        if self.slide_edit_open {
            cx.emit(super::TaskListEvent::CloseTaskEdit);
        }
        if !self.compose_open {
            self.selection_before_compose = self.working_set.selected_id.clone();
            self.compose_open = true;
            self.compose_title_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
            self.list_state.update(cx, |state, cx| {
                state.set_selected_index(None, window, cx);
            });
            self.working_set.selected_id = None;
            self.last_selected = None;
        }
        self.compose_title_input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        self.status_line = "New task".into();
        cx.notify();
    }

    pub(super) fn close_compose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.compose_open {
            return;
        }
        self.compose_open = false;
        self.compose_title_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        if let Some(id) = self.selection_before_compose.take() {
            let visible =
                Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
            if visible.iter().any(|t| t.id == id) {
                self.select_task_by_id(&id, window, cx);
            } else {
                self.rebuild_visible_list(window, cx);
                self.focus_handle.focus(window);
            }
        } else {
            self.rebuild_visible_list(window, cx);
            self.focus_handle.focus(window);
        }
        cx.notify();
    }

    pub(super) fn submit_compose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.compose_title_input.read(cx).text().to_string();
        let value = raw.trim().to_string();
        if value.is_empty() {
            crate::ui::toast::error_toast(window, cx, "Enter a task title");
            self.compose_title_input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
            return;
        }

        let success = if is_ticket_id(&value) {
            self.import_from_ticket(&value, window, cx)
        } else {
            self.create_task_with_title(&value, window, cx);
            true
        };

        if success {
            self.compose_open = false;
            self.selection_before_compose = None;
            self.compose_title_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
            self.focus_handle.focus(window);
            cx.notify();
        } else {
            self.compose_title_input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        }
    }

    pub(super) fn render_compose_row(&self, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let theme = cx.theme();
        div()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .mb_1()
                    .child("New task — enter a title or ticket id"),
            )
            .child(Input::new(&self.compose_title_input).w_full())
    }
}
