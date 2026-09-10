use gpui::{Context, Window};

use tod_store::outline::{CreatePosition, OutlineMutation};

use crate::views::linear_import::parse_ticket_reference;

use super::TaskListView;
use super::from_ticket::TicketImportResult;

impl TaskListView {
    pub(super) fn is_editing(&self) -> bool {
        self.edit_open_for.is_some()
    }

    pub(super) fn inline_edit_title(&self, cx: &Context<Self>) -> String {
        self.inline_edit_input
            .read(cx)
            .text()
            .to_string()
            .trim()
            .to_string()
    }

    pub(super) fn is_draft_edit(&self) -> bool {
        match (&self.draft_node_id, &self.edit_open_for) {
            (Some(draft), Some(editing)) => draft == editing,
            _ => false,
        }
    }

    pub(super) fn cancel_pending_inline_enter(&mut self) {
        self.pending_inline_commit = false;
        self.inline_enter_generation = self.inline_enter_generation.saturating_add(1);
    }

    fn clear_inline_edit_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit_open_for.is_some() {
            // Return focus to the task list surface (not the nested list input) so Enter
            // keeps creating siblings instead of list Confirm re-selecting the row.
            self.focus_handle.focus(window);
        }
        self.edit_open_for = None;
        self.draft_node_id = None;
        self.edit_original_title = None;
        self.inline_edit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.sync_delegate_editing(cx);
        self.status_line.clear();
    }

    pub(super) fn start_inline_edit(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self
            .all_tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.title.clone())
            .unwrap_or_default();
        self.close_chrome_overlays(cx);
        self.edit_open_for = Some(task_id.to_string());
        self.edit_original_title = Some(title.clone());
        self.inline_edit_input.update(cx, |input, cx| {
            input.set_value(&title, window, cx);
            input.focus(window, cx);
        });
        self.sync_delegate_editing(cx);
        self.status_line = "Enter adds sibling below, Escape to cancel".into();
        cx.notify();
    }

    /// Leave inline edit without Enter. Draft nodes are removed when `force_delete_draft`
    /// or when the title is still empty; otherwise typed text is saved.
    pub(super) fn abandon_inline_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        force_delete_draft: bool,
    ) {
        self.cancel_pending_inline_enter();
        let Some(editing_id) = self.edit_open_for.clone() else {
            return;
        };
        let title = self.inline_edit_title(cx);
        let is_draft = self.is_draft_edit();

        if is_draft && (force_delete_draft || title.is_empty()) {
            self.clear_inline_edit_state(window, cx);
            self.remove_outline_node(&editing_id, window, cx);
            self.focus_list(window, cx);
            cx.notify();
            return;
        }

        if is_draft && !title.is_empty() {
            if self.commit_inline_edit(window, cx) {
                self.draft_node_id = None;
            }
            self.focus_list(window, cx);
            cx.notify();
            return;
        }

        if !is_draft
            && !title.is_empty()
            && title != self.edit_original_title.as_deref().unwrap_or("")
        {
            let _ = self.commit_inline_edit(window, cx);
            self.focus_list(window, cx);
            cx.notify();
            return;
        }

        if let Some(original) = self.edit_original_title.take() {
            if let Some(task) = self.all_tasks.iter_mut().find(|t| t.id == editing_id) {
                task.title = original;
            }
        }
        self.clear_inline_edit_state(window, cx);
        self.focus_list(window, cx);
        cx.notify();
    }

    pub(super) fn leave_inline_edit_and_move(
        &mut self,
        delta: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_editing() {
            self.move_by_rows(delta, window, cx);
            return;
        }
        let was_empty_draft = self.is_draft_edit() && self.inline_edit_title(cx).is_empty();
        self.abandon_inline_edit(window, cx, false);
        if !was_empty_draft {
            self.move_by_rows(delta, window, cx);
        }
        self.focus_list(window, cx);
        cx.notify();
    }

    pub(super) fn commit_inline_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(task_id) = self.edit_open_for.clone() else {
            return false;
        };
        let title = self.inline_edit_title(cx);
        if self.is_draft_edit() {
            if let Some(ticket) = parse_ticket_reference(&title) {
                return match self.import_from_ticket(&ticket, Some(&task_id), window, cx) {
                    TicketImportResult::Pending => false,
                    TicketImportResult::Completed(ok) => ok,
                };
            }
        }
        if title.is_empty() {
            if self.is_draft_edit() {
                self.draft_node_id = None;
                self.edit_open_for = None;
                self.edit_original_title = None;
                self.inline_edit_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                });
                self.sync_delegate_editing(cx);
                self.status_line.clear();
                cx.notify();
                return true;
            }
            crate::ui::toast::error_toast(window, cx, "Title cannot be empty");
            self.inline_edit_input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
            return false;
        }
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return false;
        };
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::UpdateNodeTitle {
                node_id,
                title: title.clone(),
            })
        {
            self.show_error(format!("Failed to save title: {err}"), window, cx);
            return false;
        }
        if let Err(err) = self.fleet.writer().flush() {
            self.show_error(format!("Failed to save title: {err}"), window, cx);
            return false;
        }
        if let Some(task) = self.all_tasks.iter_mut().find(|t| t.id == task_id) {
            task.title = title;
        }
        self.draft_node_id = None;
        self.edit_open_for = None;
        self.edit_original_title = None;
        self.inline_edit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.sync_delegate_editing(cx);
        self.live_refresh(window, cx);
        self.status_line.clear();
        cx.notify();
        true
    }

    pub(super) fn sync_delegate_editing(&mut self, cx: &mut Context<Self>) {
        self.list_state.update(cx, |state, _| {
            state
                .delegate_mut()
                .set_inline_edit(self.edit_open_for.clone(), self.inline_edit_input.clone());
        });
    }

    pub(super) fn on_smart_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            let saved_id = self.edit_open_for.clone();
            if !self.commit_inline_edit(window, cx) {
                return;
            }
            if let Some(id) = saved_id {
                self.select_task_by_id(&id, window, cx);
            }
            self.create_tree_node_and_edit(CreatePosition::Below, window, cx);
            return;
        }
        if self.active_list_id.is_none() {
            self.pending_new_list = true;
            cx.notify();
            return;
        }
        self.create_tree_node_and_edit(CreatePosition::Below, window, cx);
    }

    pub(super) fn create_tree_node_and_edit(
        &mut self,
        position: CreatePosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.create_tree_node(position, window, cx) {
            self.draft_node_id = Some(id.clone());
            self.start_inline_edit(&id, window, cx);
        }
    }
}
