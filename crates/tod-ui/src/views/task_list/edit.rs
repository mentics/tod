use std::path::PathBuf;

use gpui::{Context, Window};
use uuid::Uuid;

use tod_store::outline::{CreatePosition, OutlineMutation};

use crate::views::linear_import::parse_ticket_reference;

use super::TaskItem;
use super::TaskListView;
use super::from_ticket::TicketImportResult;

/// A new row being titled. It is not a node yet: the node is created only once
/// a title is committed, because its slug is derived from that title and never
/// changes afterwards.
#[derive(Clone, Debug)]
pub(super) struct DraftRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub anchor_id: Option<Uuid>,
    pub position: CreatePosition,
}

/// Splice `draft` into flattened outline rows where `CreateNode` will put it.
pub(super) fn insert_draft_row(tasks: &mut Vec<TaskItem>, draft: &DraftRow) {
    let position_of = |id: Uuid| tasks.iter().position(|t| t.id == id.to_string());
    // A row's descendants run until the next row no deeper than it.
    let subtree_end = |ix: usize| {
        let depth = tasks[ix].depth;
        tasks[ix + 1..]
            .iter()
            .position(|t| t.depth <= depth)
            .map_or(tasks.len(), |n| ix + 1 + n)
    };
    let last_child_of = |ix: usize| {
        (
            subtree_end(ix),
            tasks[ix].depth + 1,
            Some(tasks[ix].id.clone()),
        )
    };
    let (ix, depth, parent_id) = match (draft.anchor_id.and_then(position_of), draft.position) {
        (Some(anchor), CreatePosition::Child) => last_child_of(anchor),
        (Some(anchor), CreatePosition::Above) => {
            (anchor, tasks[anchor].depth, tasks[anchor].parent_id.clone())
        }
        (Some(anchor), CreatePosition::Below) => (
            subtree_end(anchor),
            tasks[anchor].depth,
            tasks[anchor].parent_id.clone(),
        ),
        (None, _) => match draft.parent_id.and_then(position_of) {
            Some(parent) => last_child_of(parent),
            None => (tasks.len(), 0, None),
        },
    };
    tasks.insert(
        ix,
        TaskItem {
            id: draft.id.to_string(),
            ticket_id: None,
            title: String::new(),
            lifecycle: String::new(),
            entity_path: PathBuf::new(),
            tags: Vec::new(),
            has_actions: false,
            has_files: false,
            live_run_count: 0,
            shells: Vec::new(),
            interaction_timestamp: chrono::Utc::now(),
            tree_ordinal: 0,
            parent_id,
            depth,
            collapsed: false,
            is_work_node: false,
            has_spec: false,
            has_agent: false,
            requirement_count: 0,
            constraint_count: 0,
            has_children: false,
            in_flight_activity: None,
            managed: false,
            external_id: None,
            source_type: None,
            managed_count: None,
            generator_status: None,
            generator_error: None,
        },
    );
    for (ordinal, task) in tasks.iter_mut().enumerate() {
        task.tree_ordinal = ordinal;
    }
}

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

    /// Whether `id` is the draft row, which has no node behind it yet.
    pub(super) fn is_draft_id(&self, id: &str) -> bool {
        self.draft
            .as_ref()
            .is_some_and(|draft| draft.id.to_string() == id)
    }

    pub(super) fn is_draft_edit(&self) -> bool {
        self.edit_open_for
            .as_deref()
            .is_some_and(|editing| self.is_draft_id(editing))
    }

    pub(super) fn cancel_pending_inline_enter(&mut self) {
        self.pending_inline_commit = false;
        self.inline_enter_generation = self.inline_enter_generation.saturating_add(1);
    }

    fn clear_inline_edit_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit_open_for.is_some() {
            // Return focus to the task list surface (not the nested list input) so Enter
            // keeps creating siblings instead of list Confirm re-selecting the row.
            self.focus_handle.focus(window, cx);
        }
        self.edit_open_for = None;
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

    /// Leave inline edit without Enter. The draft row is dropped when `force_delete_draft`
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
            self.discard_draft(window, cx);
            self.focus_list(window, cx);
            cx.notify();
            return;
        }

        if is_draft {
            let _ = self.commit_inline_edit(window, cx);
            self.focus_list(window, cx);
            cx.notify();
            return;
        }

        if !title.is_empty() && title != self.edit_original_title.as_deref().unwrap_or("") {
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
        let is_draft = self.is_draft_edit();
        if is_draft {
            if let Some(ticket) = parse_ticket_reference(&title) {
                return match self.import_from_ticket(&ticket, Some(&task_id), window, cx) {
                    TicketImportResult::Pending => false,
                    TicketImportResult::Completed(ok) => ok,
                };
            }
        }
        if title.is_empty() {
            if is_draft {
                self.discard_draft(window, cx);
                cx.notify();
                return true;
            }
            crate::ui::toast::error_toast(window, cx, "Title cannot be empty");
            self.inline_edit_input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
            return false;
        }
        if is_draft {
            let Some(draft) = self.draft.take() else {
                return false;
            };
            if self.create_node_at(&draft, &title, window, cx).is_none() {
                self.draft = Some(draft);
                self.inline_edit_input.update(cx, |input, cx| {
                    input.focus(window, cx);
                });
                return false;
            }
        } else {
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
        }
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
            if self.is_draft_edit() && self.inline_edit_title(cx).is_empty() {
                // Enter on an untitled row cancels it rather than stacking another.
                self.abandon_inline_edit(window, cx, true);
                return;
            }
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

    /// Before opening another draft: drop an untitled draft, or commit the edit in progress.
    fn settle_inline_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.is_editing() {
            return true;
        }
        if self.is_draft_edit() && self.inline_edit_title(cx).is_empty() {
            self.abandon_inline_edit(window, cx, true);
            return true;
        }
        self.commit_inline_edit(window, cx)
    }

    /// Open a titled-on-commit draft row at `position` relative to the selection.
    pub(super) fn create_tree_node_and_edit(
        &mut self,
        position: CreatePosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.settle_inline_edit(window, cx) {
            return;
        }
        let Some(draft) = self.draft_placement(position, window, cx) else {
            return;
        };
        if let (CreatePosition::Child, Some(anchor)) = (position, draft.anchor_id) {
            self.set_collapsed(&anchor.to_string(), false, window, cx);
        }
        let id = draft.id.to_string();
        self.draft = Some(draft);
        self.reload_all_tasks();
        self.apply_agent_activity();
        self.select_created_task(&id, window, cx);
        self.start_inline_edit(&id, window, cx);
    }

    /// Where a node created at `position` relative to the selection will go.
    pub(super) fn draft_placement(
        &mut self,
        position: CreatePosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<DraftRow> {
        let Some(list_id) = self.active_list_id else {
            self.show_error("Create a list first (Enter or Ctrl+Shift+L)", window, cx);
            return None;
        };
        let anchor_id = self
            .working_set
            .selected_id
            .as_deref()
            .and_then(|id| Uuid::parse_str(id).ok());
        let parent_id = if position == CreatePosition::Child {
            anchor_id
        } else {
            None
        };
        Some(DraftRow {
            id: Uuid::new_v4(),
            list_id,
            parent_id,
            anchor_id,
            position,
        })
    }

    /// Create the node `draft` describes, titled `title`, and select it.
    pub(super) fn create_node_at(
        &mut self,
        draft: &DraftRow,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        if let Err(err) = self.fleet.enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(draft.id),
            list_id: draft.list_id,
            parent_id: draft.parent_id,
            anchor_id: draft.anchor_id,
            position: draft.position,
            title: title.to_string(),
        }) {
            self.show_error(format!("Failed to create item: {err}"), window, cx);
            return None;
        }
        if let Err(err) = self.fleet.writer().flush() {
            self.show_error(format!("Failed to create item: {err}"), window, cx);
            return None;
        }
        self.live_refresh(window, cx);
        let id = draft.id.to_string();
        self.select_created_task(&id, window, cx);
        Some(id)
    }

    /// Drop the draft row without creating a node, returning selection to where it opened.
    fn discard_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        self.clear_inline_edit_state(window, cx);
        self.reload_all_tasks();
        self.apply_agent_activity();
        self.working_set.selected_id = draft.anchor_id.or(draft.parent_id).map(|id| id.to_string());
        self.rebuild_visible_list(window, cx);
    }

    /// Tab / Shift+Tab while titling a draft move where it will be created.
    pub(super) fn reparent_draft(
        &mut self,
        direction: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.draft.clone() else {
            return;
        };
        let draft_id = draft.id.to_string();
        let Some(ix) = self.all_tasks.iter().position(|t| t.id == draft_id) else {
            return;
        };
        let depth = self.all_tasks[ix].depth;
        let (parent_id, anchor_id, position) = if direction > 0 {
            // Become the last child of the previous sibling.
            let prev_sibling = self.all_tasks[..ix]
                .iter()
                .rev()
                .take_while(|t| t.depth >= depth)
                .find(|t| t.depth == depth)
                .and_then(|t| Uuid::parse_str(&t.id).ok());
            let Some(prev) = prev_sibling else {
                return;
            };
            (Some(prev), Some(prev), CreatePosition::Child)
        } else {
            // Sit immediately after the former parent.
            let parent = self.all_tasks[ix]
                .parent_id
                .as_deref()
                .and_then(|id| Uuid::parse_str(id).ok());
            let Some(parent) = parent else {
                return;
            };
            (None, Some(parent), CreatePosition::Below)
        };
        self.draft = Some(DraftRow {
            parent_id,
            anchor_id,
            position,
            ..draft
        });
        if let (CreatePosition::Child, Some(anchor)) = (position, anchor_id) {
            self.set_collapsed(&anchor.to_string(), false, window, cx);
        }
        self.reload_all_tasks();
        self.apply_agent_activity();
        self.rebuild_visible_list(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::large_fixture_set;
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn row(n: u128, depth: usize, parent: Option<u128>) -> TaskItem {
        let mut task = large_fixture_set(1)[0].clone();
        task.id = id(n).to_string();
        task.depth = depth;
        task.parent_id = parent.map(|p| id(p).to_string());
        task
    }

    /// a, a's child a1, then b.
    fn rows() -> Vec<TaskItem> {
        vec![row(1, 0, None), row(2, 1, Some(1)), row(3, 0, None)]
    }

    fn place(anchor: Option<u128>, position: CreatePosition) -> (usize, TaskItem) {
        let mut tasks = rows();
        let draft = DraftRow {
            id: id(99),
            list_id: id(100),
            parent_id: match position {
                CreatePosition::Child => anchor.map(id),
                _ => None,
            },
            anchor_id: anchor.map(id),
            position,
        };
        insert_draft_row(&mut tasks, &draft);
        let ix = tasks.iter().position(|t| t.id == id(99).to_string()).unwrap();
        (ix, tasks[ix].clone())
    }

    #[test]
    fn draft_below_skips_the_anchors_children() {
        let (ix, draft) = place(Some(1), CreatePosition::Below);
        assert_eq!((ix, draft.depth, draft.parent_id), (2, 0, None));
    }

    #[test]
    fn draft_child_is_last_child_of_anchor() {
        let (ix, draft) = place(Some(1), CreatePosition::Child);
        assert_eq!(
            (ix, draft.depth, draft.parent_id),
            (2, 1, Some(id(1).to_string()))
        );
    }

    #[test]
    fn draft_above_takes_the_anchors_place() {
        let (ix, draft) = place(Some(3), CreatePosition::Above);
        assert_eq!((ix, draft.depth, draft.parent_id), (2, 0, None));
    }

    #[test]
    fn draft_below_a_child_stays_under_its_parent() {
        let (ix, draft) = place(Some(2), CreatePosition::Below);
        assert_eq!(
            (ix, draft.depth, draft.parent_id),
            (2, 1, Some(id(1).to_string()))
        );
    }

    #[test]
    fn draft_without_anchor_goes_last() {
        let (ix, draft) = place(None, CreatePosition::Below);
        assert_eq!((ix, draft.depth, draft.parent_id), (3, 0, None));
    }
}
