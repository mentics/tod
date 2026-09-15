//! Plan steps panel — structured, dependency-tracked plan steps for the
//! `planning` interview phase. Modeled closely on `views::obligations`, but
//! the list is flat (no phase/kind/section grouping) since a plan step's
//! ordering and structure come from its dependency graph, not a hierarchy.

mod delegate;

use crate::ui::key_context;
use crate::ui::list::{
    ListArrowDown, ListArrowUp, ListEnd, ListHome, ListPageDown, ListPageUp, viewport_row_count,
};
use crate::ui::pane_nav::{PaneFocusLeft, bind_modified_pane_nav};
use delegate::{PlanStepListDelegate, PlanStepRow, RowAction};
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, ScrollHandle, StatefulInteractiveElement,
    Styled, Subscription, Window, actions, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, TextareaState};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use tod_store::fleet::FleetStore;
use tod_store::outline::{OutlineMutation, PLAN_STEP_STATUSES, PlanStep, ReorderDirection};
use uuid::Uuid;

const PLAN_STEPS_CONTEXT: &str = "PlanSteps";
const INLINE_EDIT_ROWS: usize = 2;

actions!(
    plan_steps,
    [
        PlanStepsClose,
        PlanStepsEnter,
        PlanStepsCreateBelow,
        PlanStepsCreateAbove,
        PlanStepsMoveUp,
        PlanStepsMoveDown,
        PlanStepsEdit,
        PlanStepsCommitEdit,
        PlanStepsDelete,
        PlanStepsCycleStatus,
    ]
);

pub fn register_plan_steps_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(PLAN_STEPS_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("up", ListArrowUp, context),
        KeyBinding::new("down", ListArrowDown, context),
        KeyBinding::new("pageup", ListPageUp, context),
        KeyBinding::new("pagedown", ListPageDown, context),
        KeyBinding::new("home", ListHome, context),
        KeyBinding::new("end", ListEnd, context),
        KeyBinding::new("enter", PlanStepsEnter, context),
        KeyBinding::new("n", PlanStepsCreateBelow, context),
        KeyBinding::new("f2", PlanStepsEdit, context),
        KeyBinding::new("alt-enter", PlanStepsCreateAbove, context),
        KeyBinding::new("secondary-up", PlanStepsMoveUp, context),
        KeyBinding::new("secondary-down", PlanStepsMoveDown, context),
        KeyBinding::new("backspace", PlanStepsDelete, context),
        KeyBinding::new("delete", PlanStepsDelete, context),
        KeyBinding::new("t", PlanStepsCycleStatus, context),
        // Inline edit is a multi-line text area: arrows move the cursor as
        // usual, Escape abandons the edit, and Ctrl+Enter commits it.
        KeyBinding::new(
            "ctrl-enter",
            PlanStepsCommitEdit,
            Some(key_context::including_input(PLAN_STEPS_CONTEXT)),
        ),
    ]);
    // Left/Right are reserved for possible future collapse/expand, so
    // crossing back to the tree uses Ctrl+arrows, same as Obligations.
    bind_modified_pane_nav(cx, PLAN_STEPS_CONTEXT);
    key_context::bind_panel_escape(cx, PlanStepsClose, PLAN_STEPS_CONTEXT);
}

#[derive(Debug, Clone)]
pub enum PlanStepsEvent {
    Close,
    /// Ctrl+Left — move keyboard focus back to the task tree, leaving the panel open.
    FocusTaskList,
    /// Delete key with no plan step selected — delete the task in the tree.
    DeleteSelectedTask,
}

pub struct PlanStepsView {
    fleet: Arc<FleetStore>,
    node_id: Option<Uuid>,
    title: String,
    items: Vec<PlanStep>,
    focus_handle: FocusHandle,
    delegate: PlanStepListDelegate,
    scroll_handle: ScrollHandle,
    selected_index: Option<usize>,
    action_sink: Rc<RefCell<Vec<RowAction>>>,
    editing_id: Option<Uuid>,
    draft_id: Option<Uuid>,
    edit_original_body: Option<String>,
    inline_edit_input: Entity<TextareaState>,
    pending_abandon_edit: bool,
    pending_live_refresh: bool,
    selected_key: Option<String>,
    _inline_edit_subscription: Subscription,
}

impl PlanStepsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, fleet: Arc<FleetStore>) -> Self {
        let action_sink = Rc::new(RefCell::new(Vec::new()));
        let inline_edit_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(INLINE_EDIT_ROWS, INLINE_EDIT_ROWS)
                .placeholder("Plan step text… (Ctrl+Enter to save, Esc to cancel)")
        });
        let _inline_edit_subscription = cx.subscribe(&inline_edit_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur) {
                this.pending_abandon_edit = true;
                cx.notify();
            }
        });

        let delegate = PlanStepListDelegate::new(Vec::new(), action_sink.clone(), cx.weak_entity());

        let poll_entity = cx.weak_entity();
        let fleet_for_poll = fleet.clone();
        cx.spawn(async move |_, cx| {
            let mut fleet_rx = fleet_for_poll.subscribe_changes();
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
                let mut changed = false;
                while fleet_rx.try_recv().is_ok() {
                    changed = true;
                }
                if changed {
                    let Ok(()) = poll_entity.update(cx, |this, cx| {
                        this.pending_live_refresh = true;
                        cx.notify();
                    }) else {
                        break;
                    };
                }
            }
        })
        .detach();

        Self {
            fleet,
            node_id: None,
            title: String::new(),
            items: Vec::new(),
            focus_handle: cx.focus_handle(),
            delegate,
            scroll_handle: ScrollHandle::new(),
            selected_index: None,
            action_sink,
            editing_id: None,
            draft_id: None,
            edit_original_body: None,
            inline_edit_input,
            pending_abandon_edit: false,
            pending_live_refresh: false,
            selected_key: None,
            _inline_edit_subscription,
        }
    }

    pub fn is_open(&self) -> bool {
        self.node_id.is_some()
    }

    pub fn open(&mut self, node_id: Uuid, title: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.node_id = Some(node_id);
        self.title = title.to_string();
        self.clear_inline_edit_state(window, cx);
        self.reload(window, cx);
        self.focus_list(window, cx);
        cx.notify();
    }

    /// `focus` controls whether keyboard focus moves into the panel — true
    /// for an explicit "open plan steps" action, false when the panel is
    /// merely following tree selection and focus should stay put.
    pub fn retarget(
        &mut self,
        node_id: Uuid,
        title: &str,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.node_id == Some(node_id) {
            self.title = title.to_string();
            self.reload(window, cx);
            return;
        }
        self.node_id = Some(node_id);
        self.title = title.to_string();
        self.clear_inline_edit_state(window, cx);
        self.reload(window, cx);
        if focus {
            self.focus_list(window, cx);
        }
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.node_id.is_none() {
            return;
        }
        self.clear_inline_edit_state(window, cx);
        self.node_id = None;
        self.title.clear();
        self.items.clear();
        self.selected_key = None;
        cx.emit(PlanStepsEvent::Close);
        cx.notify();
    }

    fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_id else {
            return;
        };
        let _ = self.fleet.reload_if_stale();
        self.items = self
            .fleet
            .list_plan_steps_for_node(node_id)
            .unwrap_or_default();
        self.rebuild_visible(window, cx);
    }

    fn build_rows(&self) -> Vec<PlanStepRow> {
        self.items
            .iter()
            .cloned()
            .map(|step| {
                let depends_on = self
                    .fleet
                    .list_plan_step_dependencies(step.id)
                    .unwrap_or_default();
                let satisfies = self
                    .fleet
                    .list_plan_step_obligations(step.id)
                    .unwrap_or_default();
                PlanStepRow { step, depends_on, satisfies }
            })
            .collect()
    }

    fn rebuild_visible(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.build_rows();
        let selected = self.selected_key.clone();
        let previous_index = self.selected_index;
        let selected_ix = selected
            .as_ref()
            .and_then(|key| rows.iter().position(|r| r.key() == *key))
            .or(Some(0).filter(|_| !rows.is_empty()));

        if let Some(ix) = selected_ix {
            self.selected_key = Some(rows[ix].key());
            self.selected_index = Some(ix);
        } else {
            self.selected_key = None;
            self.selected_index = None;
        }

        self.delegate.set_rows(rows);
        self.delegate.set_selected_index(self.selected_index);
        self.delegate
            .set_inline_edit(self.editing_id.map(|id| id.to_string()), self.inline_edit_input.clone());
        if let Some(ix) = selected_ix {
            if previous_index != selected_ix {
                self.scroll_handle.scroll_to_top_of_item(ix);
            }
        }
        cx.notify();
    }

    fn select_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let key = self.delegate.rows().get(row_ix).map(|r| r.key());
        if self.selected_index != Some(row_ix) {
            if self.editing_id.is_some() {
                self.pending_abandon_edit = true;
            }
            self.selected_index = Some(row_ix);
            self.selected_key = key;
            self.delegate.set_selected_index(self.selected_index);
            self.scroll_handle.scroll_to_item(row_ix);
            cx.notify();
        }
    }

    fn selected_step(&self) -> Option<PlanStep> {
        self.delegate.selected_row().map(|r| r.step.clone()).or_else(|| {
            let key = self.selected_key.as_ref()?;
            self.items.iter().find(|s| &s.id.to_string() == key).cloned()
        })
    }

    fn sync_delegate_editing(&mut self, cx: &mut Context<Self>) {
        self.delegate
            .set_inline_edit(self.editing_id.map(|id| id.to_string()), self.inline_edit_input.clone());
        cx.notify();
    }

    fn clear_inline_edit_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_id = None;
        self.draft_id = None;
        self.edit_original_body = None;
        self.inline_edit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.sync_delegate_editing(cx);
    }

    fn is_editing(&self) -> bool {
        self.editing_id.is_some()
    }

    fn is_draft_edit(&self) -> bool {
        match (self.draft_id, self.editing_id) {
            (Some(draft), Some(editing)) => draft == editing,
            _ => false,
        }
    }

    fn edit_body(&self, cx: &Context<Self>) -> String {
        self.inline_edit_input.read(cx).text().to_string().trim().to_string()
    }

    fn start_inline_edit(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let body = self
            .items
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.body.clone())
            .unwrap_or_default();
        self.editing_id = Some(id);
        self.edit_original_body = Some(body.clone());
        self.selected_key = Some(id.to_string());
        self.inline_edit_input.update(cx, |input, cx| {
            input.set_value(&body, window, cx);
            input.focus(window, cx);
        });
        self.rebuild_visible(window, cx);
    }

    fn abandon_inline_edit(&mut self, window: &mut Window, cx: &mut Context<Self>, force_delete_draft: bool) {
        let Some(editing_id) = self.editing_id else {
            return;
        };
        let body = self.edit_body(cx);
        let is_draft = self.is_draft_edit();

        if is_draft && (force_delete_draft || body.is_empty()) {
            self.clear_inline_edit_state(window, cx);
            let _ = self
                .fleet
                .enqueue_outline(OutlineMutation::DeletePlanStep { step_id: editing_id });
            let _ = self.fleet.writer().flush();
            self.reload(window, cx);
            self.focus_list(window, cx);
            return;
        }

        if is_draft && !body.is_empty() {
            let _ = self.commit_inline_edit(window, cx);
            return;
        }

        if let Some(original) = self.edit_original_body.take() {
            if let Some(item) = self.items.iter_mut().find(|s| s.id == editing_id) {
                item.body = original;
            }
        }
        self.clear_inline_edit_state(window, cx);
        self.rebuild_visible(window, cx);
        self.focus_list(window, cx);
    }

    fn commit_inline_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editing_id) = self.editing_id else {
            return false;
        };
        let body = self.edit_body(cx);
        if body.is_empty() {
            if self.is_draft_edit() {
                self.clear_inline_edit_state(window, cx);
                let _ = self
                    .fleet
                    .enqueue_outline(OutlineMutation::DeletePlanStep { step_id: editing_id });
                let _ = self.fleet.writer().flush();
                self.reload(window, cx);
                self.focus_list(window, cx);
                return true;
            }
            crate::ui::toast::error_toast(window, cx, "Plan step cannot be empty");
            self.inline_edit_input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
            return false;
        }
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::UpdatePlanStepBody { step_id: editing_id, body: body.clone() })
        {
            crate::ui::toast::error_toast(window, cx, format!("Save failed: {err}"));
            return false;
        }
        if let Err(err) = self.fleet.writer().flush() {
            crate::ui::toast::error_toast(window, cx, format!("Save failed: {err}"));
            return false;
        }
        if let Some(item) = self.items.iter_mut().find(|s| s.id == editing_id) {
            item.body = body;
        }
        self.draft_id = None;
        self.clear_inline_edit_state(window, cx);
        self.selected_key = Some(editing_id.to_string());
        self.reload(window, cx);
        self.focus_list(window, cx);
        true
    }

    fn create_relative(&mut self, after: Option<Uuid>, before: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_id else {
            return;
        };
        let step_id = Uuid::new_v4();
        if let Err(err) = self.fleet.enqueue_outline(OutlineMutation::CreatePlanStep {
            step_id: Some(step_id),
            node_id,
            after_id: after,
            before,
            body: String::new(),
        }) {
            crate::ui::toast::error_toast(window, cx, format!("Create failed: {err}"));
            return;
        }
        if let Err(err) = self.fleet.writer().flush() {
            crate::ui::toast::error_toast(window, cx, format!("Create failed: {err}"));
            return;
        }
        self.draft_id = Some(step_id);
        self.reload(window, cx);
        self.start_inline_edit(step_id, window, cx);
    }

    fn on_smart_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            let saved = self.editing_id;
            if !self.commit_inline_edit(window, cx) {
                return;
            }
            if let Some(id) = saved {
                self.selected_key = Some(id.to_string());
                self.create_relative(Some(id), false, window, cx);
            }
            return;
        }
        match self.selected_step() {
            Some(step) => self.start_inline_edit(step.id, window, cx),
            None => self.create_relative(None, false, window, cx),
        }
    }

    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(step) = self.selected_step() else {
            return;
        };
        let id = step.id;
        let next_key = self
            .items
            .iter()
            .filter(|s| s.id != id)
            .find(|s| s.ordinal > step.ordinal)
            .map(|s| s.id.to_string())
            .or_else(|| self.items.iter().filter(|s| s.id != id).last().map(|s| s.id.to_string()));
        if let Err(err) = self.fleet.enqueue_outline(OutlineMutation::DeletePlanStep { step_id: id }) {
            crate::ui::toast::error_toast(window, cx, format!("Delete failed: {err}"));
            return;
        }
        let _ = self.fleet.writer().flush();
        self.selected_key = next_key;
        self.reload(window, cx);
        self.focus_list(window, cx);
    }

    fn move_selected(&mut self, direction: ReorderDirection, window: &mut Window, cx: &mut Context<Self>) {
        let Some(step) = self.selected_step() else {
            return;
        };
        let id = step.id;
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::ReorderPlanStep { step_id: id, direction })
        {
            crate::ui::toast::error_toast(window, cx, format!("Move failed: {err}"));
            return;
        }
        let _ = self.fleet.writer().flush();
        self.selected_key = Some(id.to_string());
        self.reload(window, cx);
        self.focus_list(window, cx);
    }

    fn cycle_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(step) = self.selected_step() else {
            return;
        };
        let current_ix = PLAN_STEP_STATUSES
            .iter()
            .position(|s| *s == step.status)
            .unwrap_or(0);
        let next = PLAN_STEP_STATUSES[(current_ix + 1) % PLAN_STEP_STATUSES.len()];
        if let Err(err) = self.fleet.enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
            step_id: step.id,
            status: next.to_string(),
        }) {
            crate::ui::toast::error_toast(window, cx, format!("Status update failed: {err}"));
            return;
        }
        let _ = self.fleet.writer().flush();
        self.selected_key = Some(step.id.to_string());
        self.reload(window, cx);
        self.focus_list(window, cx);
    }

    fn move_selection(&mut self, delta: i32, _window: &mut Window, cx: &mut Context<Self>) {
        let count = self.delegate.rows().len();
        if count == 0 {
            return;
        }
        let current = self.selected_index.unwrap_or(0);
        let next = if delta < 0 {
            current.saturating_sub((-delta) as usize)
        } else {
            (current + delta as usize).min(count.saturating_sub(1))
        };
        self.select_row(next, cx);
    }

    fn drain_row_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let actions: Vec<_> = self.action_sink.borrow_mut().drain(..).collect();
        for action in actions {
            match action {
                RowAction::StartEdit { step_id } => {
                    self.start_inline_edit(step_id, window, cx);
                }
                RowAction::Select { row_ix } => {
                    self.select_row(row_ix, cx);
                }
            }
        }
    }

    fn on_close(&mut self, _: &PlanStepsClose, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            self.abandon_inline_edit(window, cx, true);
            return;
        }
        self.close(window, cx);
    }

    fn on_enter(&mut self, _: &PlanStepsEnter, window: &mut Window, cx: &mut Context<Self>) {
        self.on_smart_enter(window, cx);
    }

    fn on_create_below(&mut self, _: &PlanStepsCreateBelow, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            return;
        }
        let after = self.selected_step().map(|s| s.id);
        self.create_relative(after, false, window, cx);
    }

    fn on_create_above(&mut self, _: &PlanStepsCreateAbove, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            return;
        }
        let after = self.selected_step().map(|s| s.id);
        self.create_relative(after, true, window, cx);
    }

    fn on_move_up(&mut self, _: &PlanStepsMoveUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selected(ReorderDirection::Up, window, cx);
    }

    fn on_move_down(&mut self, _: &PlanStepsMoveDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selected(ReorderDirection::Down, window, cx);
    }

    fn on_edit(&mut self, _: &PlanStepsEdit, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(step) = self.selected_step() {
            self.start_inline_edit(step.id, window, cx);
        }
    }

    fn on_commit_edit(&mut self, _: &PlanStepsCommitEdit, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_editing() {
            return;
        }
        let _ = self.commit_inline_edit(window, cx);
    }

    fn on_delete(&mut self, _: &PlanStepsDelete, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            return;
        }
        if self.selected_step().is_some() {
            self.delete_selected(window, cx);
        } else {
            cx.emit(PlanStepsEvent::DeleteSelectedTask);
        }
    }

    fn on_cycle_status(&mut self, _: &PlanStepsCycleStatus, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            return;
        }
        self.cycle_status(window, cx);
    }

    fn on_arrow_up(&mut self, _: &ListArrowUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, window, cx);
    }

    fn on_arrow_down(&mut self, _: &ListArrowDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, window, cx);
    }

    fn on_page_up(&mut self, _: &ListPageUp, window: &mut Window, cx: &mut Context<Self>) {
        let page = viewport_row_count(window.viewport_size().height).max(1);
        self.move_selection(-(page as i32), window, cx);
    }

    fn on_page_down(&mut self, _: &ListPageDown, window: &mut Window, cx: &mut Context<Self>) {
        let page = viewport_row_count(window.viewport_size().height).max(1);
        self.move_selection(page as i32, window, cx);
    }

    fn on_home(&mut self, _: &ListHome, _window: &mut Window, cx: &mut Context<Self>) {
        let count = self.delegate.rows().len();
        if count == 0 {
            return;
        }
        self.select_row(0, cx);
        self.scroll_handle.scroll_to_top_of_item(0);
    }

    fn on_end(&mut self, _: &ListEnd, _window: &mut Window, cx: &mut Context<Self>) {
        let count = self.delegate.rows().len();
        if count == 0 {
            return;
        }
        let last = count - 1;
        self.select_row(last, cx);
        self.scroll_handle.scroll_to_top_of_item(last);
    }
}

impl EventEmitter<PlanStepsEvent> for PlanStepsView {}

impl Focusable for PlanStepsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PlanStepsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_live_refresh {
            self.pending_live_refresh = false;
            self.reload(window, cx);
        }
        if self.pending_abandon_edit {
            self.pending_abandon_edit = false;
            self.abandon_inline_edit(window, cx, false);
        }
        self.drain_row_actions(window, cx);

        if !self.is_open() {
            return div().into_any_element();
        }

        let theme = cx.theme();
        let border = theme.border;
        let accent = theme.primary;
        let muted = theme.muted_foreground;

        v_flex()
            .key_context(PLAN_STEPS_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .border_l_2()
            .border_color(accent)
            .on_action(cx.listener(|this, _: &PaneFocusLeft, _, cx| {
                if this.editing_id.is_some() {
                    cx.propagate();
                    return;
                }
                cx.emit(PlanStepsEvent::FocusTaskList);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_enter))
            .on_action(cx.listener(Self::on_create_below))
            .on_action(cx.listener(Self::on_create_above))
            .on_action(cx.listener(Self::on_move_up))
            .on_action(cx.listener(Self::on_move_down))
            .on_action(cx.listener(Self::on_edit))
            .on_action(cx.listener(Self::on_commit_edit))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_cycle_status))
            .on_action(cx.listener(Self::on_arrow_up))
            .on_action(cx.listener(Self::on_arrow_down))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .bg(theme.secondary)
                    .child(
                        v_flex()
                            .gap_0p5()
                            .min_w_0()
                            .flex_1()
                            .child(div().text_sm().font_semibold().child("Plan Steps"))
                            .child(
                                div().text_xs().text_color(muted).overflow_hidden().child(
                                    crate::ui::selectable_text::selectable_text(
                                        "plan-steps-title",
                                        self.title.clone(),
                                        window,
                                        cx,
                                    )
                                    .text_color(muted),
                                ),
                            ),
                    )
                    .child(
                        Button::new("plan-steps-close")
                            .label("Close")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close(window, cx);
                            })),
                    ),
            )
            .child({
                let row_count = self.delegate.rows().len();
                let mut rows = Vec::with_capacity(row_count);
                for ix in 0..row_count {
                    if let Some(row) = self.delegate.render_row(ix, window, cx) {
                        rows.push(row);
                    }
                }
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("plan-steps-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                            .children(rows),
                    )
                    .child(
                        // Narrow right-edge strip, not the full row area: the
                        // Scrollbar element installs a click-to-jump handler
                        // across its entire bounds, which would otherwise
                        // swallow every mouse click meant for the rows below.
                        div()
                            .occlude()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(gpui::px(16.))
                            .child(Scrollbar::vertical(&self.scroll_handle)),
                    )
            })
            .child(
                div()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child("↑/↓ navigate · Enter edits · N adds · T cycles status · Cmd/Ctrl+↑/↓ reorders · Esc closes"),
            )
            .into_any_element()
    }
}
