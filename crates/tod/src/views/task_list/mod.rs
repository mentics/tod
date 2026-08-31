mod compose;
mod credential_prompt;
mod delegate;
mod edit;
pub(crate) mod fixtures;
mod from_ticket;
use from_ticket::PendingTicketImport;
mod model;
mod row_menu;
mod working_set;

pub use model::SortKey;
pub use model::TaskItem;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::interview::{TodPaths, interview_work_remains};
use crate::process::interview_phase_for_lifecycle;
use crate::ui::actionable::{chrome_control_with_shortcut, render_shortcut_pill};
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav, on_app_nav_toggle};
use crate::ui::key_context;
use crate::ui::list::{
    ListArrowDown, ListArrowUp, ListEnd, ListHome, ListPageDown, ListPageUp, ListView,
    viewport_row_count,
};
use crate::ui::selectable_text::selectable_text;
use delegate::{RowAction, TaskListDelegate};
use fixtures::load_tasks_from_store;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, KeyBinding,
    ParentElement, Render, Styled, Subscription, Timer, Window, actions, div,
    prelude::FluentBuilder, px,
};
use gpui_component::IndexPath;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::list::{ListEvent, ListState};
use gpui_component::menu::PopupMenu;
use gpui_component::{ActiveTheme, Disableable, Sizable, StyledExt};
use model::{ListWorkingSet as WorkingSet, filter_and_sort_tasks, nearest_visible_id};
use row_menu::RowMenuKind;
use tod_store::fleet::{FleetStore, validate_interview_workspace};
use tod_store::outline::{CreatePosition, OutlineMutation, ReorderDirection};
use working_set::{load_working_set, save_working_set};

actions!(
    task_list,
    [
        TaskListOpen,
        TaskListNewTask,
        TaskListFocusSearch,
        TaskListSortToggle,
        TaskListClearTagFilter,
        TaskListRowAgents,
        TaskListRowShells,
        TaskListRowLifecycle,
        TaskListRowEdit,
        TaskListOpenEditPanel,
        TaskListOpenObligations,
        TaskListTag1,
        TaskListTag2,
        TaskListTag3,
        TaskListTag4,
        TaskListTag5,
        TaskListTag6,
        TaskListTag7,
        TaskListTag8,
        TaskListTag9,
        TaskListTag0,
        TaskListDismissOverlay,
        TaskListIndent,
        TaskListOutdent,
        TaskListSelectParent,
        TaskListExpand,
        TaskListCreateBelow,
        TaskListCreateChild,
        TaskListCreateAbove,
        TaskListNewList,
        TaskListNextList,
        TaskListPrevList,
        TaskListEnter,
        TaskListMoveUp,
        TaskListMoveDown,
        TaskListEditNavUp,
        TaskListEditNavDown,
        TaskListDelete,
    ]
);

const TASK_LIST_CONTEXT: &str = "TaskList";

pub fn register_task_list_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(TASK_LIST_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("up", ListArrowUp, context),
        KeyBinding::new("down", ListArrowDown, context),
        KeyBinding::new("pageup", ListPageUp, context),
        KeyBinding::new("pagedown", ListPageDown, context),
        KeyBinding::new("home", ListHome, context),
        KeyBinding::new("end", ListEnd, context),
        KeyBinding::new("enter", TaskListEnter, context),
        KeyBinding::new("n", TaskListCreateBelow, context),
        KeyBinding::new("/", TaskListFocusSearch, context),
        KeyBinding::new("s", TaskListSortToggle, context),
        KeyBinding::new("cmd-shift-t", TaskListClearTagFilter, context),
        KeyBinding::new("a", TaskListRowAgents, context),
        KeyBinding::new("l", TaskListRowLifecycle, context),
        KeyBinding::new("t", TaskListRowShells, context),
        KeyBinding::new("o", TaskListOpenObligations, context),
        KeyBinding::new("e", TaskListOpenEditPanel, context),
        KeyBinding::new("f2", TaskListRowEdit, context),
        KeyBinding::new("1", TaskListTag1, context),
        KeyBinding::new("2", TaskListTag2, context),
        KeyBinding::new("3", TaskListTag3, context),
        KeyBinding::new("4", TaskListTag4, context),
        KeyBinding::new("5", TaskListTag5, context),
        KeyBinding::new("6", TaskListTag6, context),
        KeyBinding::new("7", TaskListTag7, context),
        KeyBinding::new("8", TaskListTag8, context),
        KeyBinding::new("9", TaskListTag9, context),
        KeyBinding::new("0", TaskListTag0, context),
        KeyBinding::new("tab", TaskListIndent, context),
        KeyBinding::new("shift-tab", TaskListOutdent, context),
        KeyBinding::new("left", TaskListSelectParent, context),
        KeyBinding::new("right", TaskListExpand, context),
        KeyBinding::new("shift-enter", TaskListCreateChild, context),
        KeyBinding::new(
            "shift-enter",
            TaskListCreateChild,
            Some(key_context::including_input(TASK_LIST_CONTEXT)),
        ),
        KeyBinding::new("alt-enter", TaskListCreateAbove, context),
        KeyBinding::new("ctrl-shift-l", TaskListNewList, context),
        KeyBinding::new("ctrl-tab", TaskListNextList, context),
        KeyBinding::new("ctrl-shift-tab", TaskListPrevList, context),
        KeyBinding::new("ctrl-up", TaskListMoveUp, context),
        KeyBinding::new("ctrl-down", TaskListMoveDown, context),
        KeyBinding::new("delete", TaskListDelete, context),
        KeyBinding::new("backspace", TaskListDelete, context),
        // Inline title edit: Escape cancels; arrows leave the field and move selection.
        KeyBinding::new(
            "up",
            TaskListEditNavUp,
            Some(key_context::including_input(TASK_LIST_CONTEXT)),
        ),
        KeyBinding::new(
            "down",
            TaskListEditNavDown,
            Some(key_context::including_input(TASK_LIST_CONTEXT)),
        ),
    ]);
    key_context::bind_panel_escape(cx, TaskListDismissOverlay, TASK_LIST_CONTEXT);
}

#[derive(Debug, Clone)]
pub enum TaskListEvent {
    OpenInterview {
        task_id: String,
        node_id: uuid::Uuid,
        lifecycle: String,
        title: String,
    },
    OpenTaskEdit {
        task_id: String,
        _title: String,
    },
    OpenObligations {
        task_id: String,
        title: String,
    },
    CloseTaskEdit,
    CloseObligations,
    CloseAgentPanel,
    OpenLifecycle {
        _task_id: String,
        lifecycle: String,
    },
    OpenAgentDetail {
        task_id: String,
        agent_id: Option<String>,
    },
    OpenShell {
        task_id: String,
        shell_id: Option<String>,
        agent_id: Option<String>,
    },
}

pub struct TaskListView {
    all_tasks: Vec<TaskItem>,
    working_set: WorkingSet,
    search_query: String,
    list_state: Entity<ListState<TaskListDelegate>>,
    list_view: ListView<TaskListDelegate>,
    search_input: Entity<InputState>,
    focus_handle: FocusHandle,
    last_selected: Option<IndexPath>,
    pending_revert: Option<IndexPath>,
    action_sink: Rc<RefCell<Vec<RowAction>>>,
    compose_open: bool,
    compose_title_input: Entity<InputState>,
    credential_prompt_open: bool,
    credential_input: Entity<InputState>,
    pending_credential_request: Option<credential_prompt::PendingCredentialRequest>,
    pending_credential_submit: bool,
    selection_before_compose: Option<String>,
    open_row_menu: Option<(RowMenuKind, String)>,
    row_menu: Option<Entity<PopupMenu>>,
    _row_menu_subscription: Option<Subscription>,
    sort_menu_open: bool,
    edit_open_for: Option<String>,
    slide_edit_open: bool,
    obligations_open: bool,
    agent_panel_open: bool,
    /// Node created for inline edit that is not yet committed with Enter.
    draft_node_id: Option<String>,
    edit_original_title: Option<String>,
    inline_edit_input: Entity<InputState>,
    pending_inline_commit: bool,
    /// Bumped when inline Enter is cancelled so deferred commit handlers no-op.
    inline_enter_generation: u64,
    pending_abandon_edit: bool,
    pending_compose_submit: bool,
    pending_live_refresh: bool,
    pending_new_list: bool,
    pending_create_below: bool,
    ticket_import_generation: u64,
    pending_ticket_import: Option<PendingTicketImport>,
    status_line: String,
    config_dir: PathBuf,
    fleet: Arc<FleetStore>,
    active_list_id: Option<uuid::Uuid>,
    outline_lists: Vec<tod_store::outline::types::OutlineList>,
    app_nav: AppNavMenu,
    _list_subscription: Subscription,
    _compose_subscription: Subscription,
    _credential_subscription: Subscription,
    _inline_edit_subscription: Subscription,
}

impl TaskListView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, fleet: Arc<FleetStore>) -> Self {
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let config_dir = paths.config_dir().to_path_buf();

        let outline_lists = fleet.list_outline_lists().unwrap_or_default();
        let mut working_set = load_working_set(&config_dir);
        let active_list_id = working_set
            .active_list_id
            .as_deref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .or_else(|| outline_lists.first().map(|l| l.id));
        if let Some(id) = active_list_id {
            working_set.active_list_id = Some(id.to_string());
        }
        let all_tasks = load_tasks_from_store(&fleet, active_list_id);
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search tasks…"));

        let inline_edit_input = cx.new(|cx| InputState::new(window, cx).placeholder("Item title…"));
        let _inline_edit_subscription = cx.subscribe(&inline_edit_input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.inline_enter_generation = this.inline_enter_generation.saturating_add(1);
                this.pending_inline_commit = true;
                cx.notify();
            } else if matches!(event, InputEvent::Blur) {
                this.pending_abandon_edit = true;
                cx.notify();
            }
        });

        let compose_title_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("Title or ticket id (e.g. TOD-142)"));
        let credential_input = cx.new(|cx| InputState::new(window, cx).placeholder("lin_api_…"));
        let _compose_subscription = cx.subscribe(&compose_title_input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.pending_compose_submit = true;
                cx.notify();
            }
        });
        let _credential_subscription = cx.subscribe(&credential_input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.pending_credential_submit = true;
                cx.notify();
            }
        });

        let action_sink = Rc::new(RefCell::new(Vec::new()));
        let visible = Self::visible_tasks(&all_tasks, "", &working_set);
        let initial_selection = Self::initial_selection(&visible, &working_set);

        if let Some(id) = &initial_selection.1 {
            working_set.selected_id = Some(id.clone());
        }

        let delegate = TaskListDelegate::new(visible.clone(), action_sink.clone());
        let list_state = cx.new(|cx| ListState::new(delegate, window, cx).searchable(false));

        list_state.update(cx, |state, cx| {
            state
                .delegate_mut()
                .set_tag_filter(working_set.tag_filter.clone());
            state.set_selected_index(initial_selection.0, window, cx);
        });

        let list_view = ListView::new(list_state.clone());
        let focus_handle = cx.focus_handle();

        let _list_subscription = cx.subscribe(&list_state, |this, _state, event, cx| match event {
            // Keyboard navigation emits Select; clicks and Enter emit Confirm.
            ListEvent::Select(ix) => {
                this.close_sort_menu(cx);
                this.clamp_selection(*ix, cx);
                if this.pending_revert.is_none() {
                    this.sync_selected_id(cx);
                }
            }
            ListEvent::Confirm(ix) => {
                this.close_sort_menu(cx);
                this.clamp_selection(*ix, cx);
                if this.pending_revert.is_none() {
                    this.sync_selected_id(cx);
                }
            }
            ListEvent::Cancel => {}
        });

        let view = Self {
            all_tasks,
            working_set,
            search_query: String::new(),
            list_state,
            list_view,
            search_input,
            focus_handle,
            last_selected: initial_selection.0,
            pending_revert: None,
            action_sink,
            compose_open: false,
            compose_title_input,
            credential_prompt_open: false,
            credential_input,
            pending_credential_request: None,
            pending_credential_submit: false,
            selection_before_compose: None,
            open_row_menu: None,
            row_menu: None,
            _row_menu_subscription: None,
            sort_menu_open: false,
            edit_open_for: None,
            slide_edit_open: false,
            obligations_open: false,
            agent_panel_open: false,
            draft_node_id: None,
            edit_original_title: None,
            inline_edit_input,
            pending_inline_commit: false,
            inline_enter_generation: 0,
            pending_abandon_edit: false,
            pending_compose_submit: false,
            pending_live_refresh: false,
            pending_new_list: false,
            pending_create_below: false,
            ticket_import_generation: 0,
            pending_ticket_import: None,
            status_line: String::new(),
            config_dir,
            fleet: fleet.clone(),
            active_list_id,
            outline_lists,
            app_nav: AppNavMenu::default(),
            _list_subscription,
            _compose_subscription,
            _credential_subscription,
            _inline_edit_subscription,
        };

        cx.defer_in(window, move |this, window, cx| {
            this.list_state.update(cx, |state, cx| {
                state.set_selected_index(this.last_selected, window, cx);
                if this.last_selected.is_some() {
                    state.scroll_to_selected_item(window, cx);
                }
                state.focus(window, cx);
            });
            this.focus_handle.focus(window);
        });

        let poll_entity = cx.weak_entity();
        let fleet_for_poll = fleet.clone();
        cx.spawn(async move |_, cx| {
            let mut fleet_rx = fleet_for_poll.subscribe_changes();
            let mut ticks = 0u32;
            loop {
                Timer::after(std::time::Duration::from_millis(500)).await;
                let mut changed = false;
                while fleet_rx.try_recv().is_ok() {
                    changed = true;
                }
                ticks = ticks.saturating_add(1);
                if changed || ticks >= 6 {
                    ticks = 0;
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

        view
    }

    fn visible_tasks(tasks: &[TaskItem], search: &str, ws: &WorkingSet) -> Vec<TaskItem> {
        filter_and_sort_tasks(tasks, search, ws)
    }

    fn initial_selection(
        visible: &[TaskItem],
        ws: &WorkingSet,
    ) -> (Option<IndexPath>, Option<String>) {
        if visible.is_empty() {
            return (None, None);
        }
        if let Some(id) = &ws.selected_id {
            if let Some(row) = visible.iter().position(|t| &t.id == id) {
                return (Some(IndexPath::new(row)), Some(id.clone()));
            }
        }
        (Some(IndexPath::default()), Some(visible[0].id.clone()))
    }

    fn rebuild_visible_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous_id = self.working_set.selected_id.clone().or_else(|| {
            self.list_state
                .read(cx)
                .delegate()
                .selected_item()
                .map(|t| t.id.clone())
        });

        let visible = Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);

        let next_id = previous_id
            .as_ref()
            .and_then(|id| {
                nearest_visible_id(&self.all_tasks, &self.search_query, &self.working_set, id)
            })
            .or_else(|| visible.first().map(|t| t.id.clone()));

        self.working_set.selected_id = next_id.clone();
        let selected_ix = next_id
            .as_ref()
            .and_then(|id| visible.iter().position(|t| &t.id == id))
            .map(IndexPath::new);

        self.list_state.update(cx, |state, cx| {
            state.delegate_mut().set_items(visible);
            state
                .delegate_mut()
                .set_tag_filter(self.working_set.tag_filter.clone());
            state
                .delegate_mut()
                .set_inline_edit(self.edit_open_for.clone(), self.inline_edit_input.clone());
            state
                .delegate_mut()
                .set_row_menu(self.open_row_menu.clone(), self.row_menu.clone());
            state.set_selected_index(selected_ix, window, cx);
            if selected_ix.is_some() {
                state.scroll_to_selected_item(window, cx);
            }
            cx.notify();
        });

        self.last_selected = selected_ix;
        self.pending_revert = None;
        self.persist_working_set();
        cx.notify();
    }

    fn sync_selected_id(&mut self, cx: &mut Context<Self>) {
        let previous = self.working_set.selected_id.clone();
        let Some(item) = self.list_state.read(cx).delegate().selected_item().cloned() else {
            return;
        };
        let new_id = item.id.clone();
        if self.edit_open_for.is_some() && previous.as_deref() != Some(new_id.as_str()) {
            self.pending_abandon_edit = true;
        }
        if self.slide_edit_open && previous.as_deref() != Some(new_id.as_str()) {
            self.emit_open_edit_for(&new_id, cx);
        }
        self.working_set.selected_id = Some(new_id);
        self.persist_working_set();
        cx.notify();
    }

    fn persist_working_set(&mut self) {
        self.working_set.active_list_id = self.active_list_id.map(|id| id.to_string());
        save_working_set(&self.config_dir, &self.working_set);
    }

    fn drain_row_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let actions: Vec<RowAction> = self.action_sink.borrow_mut().drain(..).collect();
        for action in actions {
            self.handle_row_action(action, window, cx);
        }
    }

    fn handle_row_action(
        &mut self,
        action: RowAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            RowAction::OpenEdit { task_id } => {
                self.dismiss_compose_for_row_action(window, cx);
                self.select_task_by_id(&task_id, window, cx);
                self.open_task_edit_panel(&task_id, window, cx);
            }
            RowAction::InlineEdit { task_id } => {
                self.dismiss_compose_for_row_action(window, cx);
                self.select_task_by_id(&task_id, window, cx);
                self.start_inline_edit(&task_id, window, cx);
            }
            RowAction::ToggleTagFilter { task_id, tag } => {
                self.select_task_by_id(&task_id, window, cx);
                self.toggle_tag_filter(&tag, window, cx);
            }
            RowAction::AgentsControl { task_id } => {
                self.select_task_by_id(&task_id, window, cx);
                self.bump_interaction(&task_id, window, cx);
                self.handle_agents_control(&task_id, window, cx);
            }
            RowAction::ShellsControl { task_id } => {
                self.select_task_by_id(&task_id, window, cx);
                self.bump_interaction(&task_id, window, cx);
                self.handle_shells_control(&task_id, window, cx);
            }
            RowAction::LifecycleControl {
                task_id,
                _lifecycle: _,
            } => {
                self.select_task_by_id(&task_id, window, cx);
                self.run_lifecycle_next(&task_id, window, cx);
            }
            RowAction::ToggleCollapsed { task_id } => {
                self.toggle_collapsed(&task_id, window, cx);
            }
            RowAction::OpenObligations { task_id } => {
                self.dismiss_compose_for_row_action(window, cx);
                self.select_task_by_id(&task_id, window, cx);
                self.open_obligations_panel(&task_id, window, cx);
            }
        }
    }

    fn set_collapsed(
        &mut self,
        task_id: &str,
        collapsed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(node_id) = uuid::Uuid::parse_str(task_id) else {
            return;
        };
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) else {
            return;
        };
        if task.collapsed == collapsed {
            return;
        }
        let _ = self
            .fleet
            .enqueue_outline(OutlineMutation::SetNodeCollapsed { node_id, collapsed });
        let _ = self.fleet.writer().flush();
        self.live_refresh(window, cx);
    }

    fn toggle_collapsed(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) else {
            return;
        };
        self.set_collapsed(task_id, !task.collapsed, window, cx);
    }

    fn create_tree_node(
        &mut self,
        position: CreatePosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let list_id = match self.active_list_id {
            Some(id) => id,
            None => {
                self.status_line = "Create a list first (Enter or Ctrl+Shift+L)".into();
                cx.notify();
                return None;
            }
        };
        let before_ids: std::collections::HashSet<_> =
            self.all_tasks.iter().map(|t| t.id.clone()).collect();
        let new_node_id = uuid::Uuid::new_v4();
        let anchor = self
            .working_set
            .selected_id
            .as_deref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok());
        let parent_id = if position == CreatePosition::Child {
            anchor
        } else {
            None
        };
        if let Err(err) = self.fleet.enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(new_node_id),
            list_id,
            parent_id,
            anchor_id: anchor,
            position,
            title: String::new(),
        }) {
            self.status_line = format!("Failed to create item: {err}");
            cx.notify();
            return None;
        }
        if let Err(err) = self.fleet.writer().flush() {
            self.status_line = format!("Failed to create item: {err}");
            cx.notify();
            return None;
        }
        self.live_refresh(window, cx);
        let new_id = if self
            .all_tasks
            .iter()
            .any(|t| t.id == new_node_id.to_string())
        {
            Some(new_node_id.to_string())
        } else {
            self.all_tasks
                .iter()
                .find(|t| !before_ids.contains(&t.id))
                .map(|t| t.id.clone())
        };
        if let Some(ref id) = new_id {
            self.select_created_task(id, window, cx);
        }
        new_id
    }

    fn reload_outline_lists(&mut self) {
        self.outline_lists = self.fleet.list_outline_lists().unwrap_or_default();
    }

    fn active_list_title(&self) -> String {
        let Some(id) = self.active_list_id else {
            return "No list".into();
        };
        self.outline_lists
            .iter()
            .find(|l| l.id == id)
            .map(|l| l.title.clone())
            .unwrap_or_else(|| "List".into())
    }

    fn create_new_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_outline_lists();
        let n = self.outline_lists.len() + 1;
        let slug = format!("list-{n}");
        let title = format!("List {n}");
        if self
            .fleet
            .enqueue_outline(OutlineMutation::CreateList {
                slug: slug.clone(),
                title: title.clone(),
            })
            .is_err()
        {
            self.status_line = "Failed to create list".into();
            cx.notify();
            return;
        }
        let _ = self.fleet.writer().flush();
        let _ = self.fleet.reload_if_stale();
        self.reload_outline_lists();
        let Some(new_id) = self
            .outline_lists
            .iter()
            .find(|l| l.slug == slug)
            .map(|l| l.id)
        else {
            self.status_line = "List created but not found".into();
            cx.notify();
            return;
        };
        self.switch_active_list(new_id, window, cx);
        self.status_line = format!("Created {title}");
        cx.notify();
    }

    fn switch_active_list(
        &mut self,
        list_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_list_id = Some(list_id);
        self.working_set.selected_id = None;
        self.live_refresh(window, cx);
        self.persist_working_set();
        cx.notify();
    }

    fn cycle_list(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_outline_lists();
        if self.outline_lists.is_empty() {
            self.status_line = "No lists yet — press Enter to create one".into();
            cx.notify();
            return;
        }
        let current_ix = self
            .active_list_id
            .and_then(|id| self.outline_lists.iter().position(|l| l.id == id))
            .unwrap_or(0);
        let len = self.outline_lists.len();
        let next_ix = (current_ix as i32 + delta).rem_euclid(len as i32) as usize;
        let next_id = self.outline_lists[next_ix].id;
        let title = self.outline_lists[next_ix].title.clone();
        self.switch_active_list(next_id, window, cx);
        self.status_line = format!("Switched to {title}");
    }

    fn bump_interaction(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let now = chrono::Utc::now();
        if let Some(task) = self.all_tasks.iter_mut().find(|t| t.id == task_id) {
            task.interaction_timestamp = now;
        }
        self.rebuild_visible_list(window, cx);
    }

    fn toggle_tag_filter(&mut self, tag: &str, window: &mut Window, cx: &mut Context<Self>) {
        match &self.working_set.tag_filter {
            Some(active) if active.eq_ignore_ascii_case(tag) => {
                self.working_set.tag_filter = None;
            }
            _ => {
                self.working_set.tag_filter = Some(tag.to_string());
            }
        }
        self.rebuild_visible_list(window, cx);
    }

    fn handle_agents_control(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent_count = self
            .all_tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.agents.len())
            .unwrap_or(0);
        let task_title = self
            .all_tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.title.clone())
            .unwrap_or_default();
        if agent_count == 0 {
            self.close_chrome_overlays(cx);
            if self.slide_edit_open {
                cx.emit(TaskListEvent::CloseTaskEdit);
            }
            if self.obligations_open {
                cx.emit(TaskListEvent::CloseObligations);
            }
            cx.emit(TaskListEvent::OpenAgentDetail {
                task_id: task_id.to_string(),
                agent_id: None,
            });
            self.status_line = format!("Launch agent config for {task_title}");
        } else {
            self.toggle_agents_menu(task_id, window, cx);
        }
    }

    fn handle_shells_control(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) else {
            return;
        };
        if task.shells.is_empty() {
            if task.agents.len() > 1 {
                self.toggle_shell_agent_picker(task_id, window, cx);
                return;
            }
            cx.emit(TaskListEvent::OpenShell {
                task_id: task_id.to_string(),
                shell_id: None,
                agent_id: None,
            });
        } else {
            self.toggle_shells_menu(task_id, window, cx);
        }
    }

    fn dismiss_compose_for_row_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.compose_open {
            return;
        }
        self.compose_open = false;
        self.selection_before_compose = None;
        self.compose_title_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
    }

    fn run_lifecycle_next(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let lifecycle = self
            .all_tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.lifecycle.clone());
        let Some(lifecycle) = lifecycle else {
            return;
        };
        self.bump_interaction(task_id, window, cx);
        self.handle_lifecycle_control(task_id, &lifecycle, cx);
    }

    fn handle_lifecycle_control(&mut self, task_id: &str, lifecycle: &str, cx: &mut Context<Self>) {
        if interview_phase_for_lifecycle(lifecycle).is_some() {
            if let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) {
                if task.is_work_node {
                    let Ok(node_id) = uuid::Uuid::parse_str(&task.id) else {
                        self.status_line = "Interview unavailable — invalid task id.".into();
                        return;
                    };
                    if interview_work_remains(node_id, lifecycle) {
                        if let Ok(Some(task_row)) = self.fleet.get_task(task_id) {
                            if task_row.repo.as_ref().is_none_or(|r| r.trim().is_empty()) {
                                self.status_line =
                                    "Set repository on task before starting interview.".into();
                                return;
                            }
                            let repo = task_row.repo.as_deref().unwrap_or("");
                            let branch = task_row.branch.as_deref().unwrap_or("");
                            if let Err(err) =
                                validate_interview_workspace(PathBuf::from(repo).as_path(), branch)
                            {
                                self.status_line = format!("Interview workspace: {err:#}").into();
                                return;
                            }
                        }
                        cx.emit(TaskListEvent::OpenInterview {
                            task_id: task_id.to_string(),
                            node_id,
                            lifecycle: lifecycle.to_string(),
                            title: task.title.clone(),
                        });
                        self.status_line = format!("Opening interview for {}", task.title);
                    } else {
                        cx.emit(TaskListEvent::OpenLifecycle {
                            _task_id: task_id.to_string(),
                            lifecycle: lifecycle.to_string(),
                        });
                        self.status_line = format!("Lifecycle panel: {lifecycle}");
                    }
                } else {
                    self.status_line = "Interview unavailable — task is not a work node.".into();
                }
                return;
            }
        }
        cx.emit(TaskListEvent::OpenLifecycle {
            _task_id: task_id.to_string(),
            lifecycle: lifecycle.to_string(),
        });
        self.status_line = format!("Lifecycle panel: {lifecycle}");
    }

    /// Open the lifecycle transition panel for a task (bypasses interview routing).
    pub fn open_lifecycle_panel(&mut self, task_id: &str, lifecycle: &str, cx: &mut Context<Self>) {
        cx.emit(TaskListEvent::OpenLifecycle {
            _task_id: task_id.to_string(),
            lifecycle: lifecycle.to_string(),
        });
        self.status_line = format!("Lifecycle panel: {lifecycle}");
    }

    pub fn restore_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_list(window, cx);
    }

    pub(super) fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.list_state.update(cx, |state, cx| {
            state.focus(window, cx);
        });
        self.focus_handle.focus(window);
    }

    fn select_task_by_id(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let visible = Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
        if let Some(row) = visible.iter().position(|t| t.id == task_id) {
            let ix = IndexPath::new(row);
            self.last_selected = Some(ix);
            self.working_set.selected_id = Some(task_id.to_string());
            self.list_state.update(cx, |state, cx| {
                state.set_selected_index(Some(ix), window, cx);
                state.scroll_to_selected_item(window, cx);
            });
            self.persist_working_set();
        }
    }

    fn emit_open_edit_for(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) else {
            return;
        };
        cx.emit(TaskListEvent::OpenTaskEdit {
            task_id: task_id.to_string(),
            _title: task.title.clone(),
        });
        self.status_line = format!("Edit: {}", task.title);
    }

    pub fn open_task_edit_panel(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_chrome_overlays(cx);
        if self.obligations_open {
            cx.emit(TaskListEvent::CloseObligations);
        }
        self.emit_open_edit_for(task_id, cx);
        self.bump_interaction(task_id, window, cx);
        cx.notify();
    }

    fn emit_open_obligations_for(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) else {
            return;
        };
        if !task.has_spec {
            return;
        }
        cx.emit(TaskListEvent::OpenObligations {
            task_id: task_id.to_string(),
            title: task.title.clone(),
        });
        self.status_line = format!("Obligations: {}", task.title);
    }

    pub fn open_obligations_panel(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) else {
            return;
        };
        if !task.has_spec {
            self.status_line = "Obligations require the Spec capability.".into();
            cx.notify();
            return;
        }
        self.close_chrome_overlays(cx);
        if self.slide_edit_open {
            cx.emit(TaskListEvent::CloseTaskEdit);
        }
        self.emit_open_obligations_for(task_id, cx);
        self.bump_interaction(task_id, window, cx);
        cx.notify();
    }

    pub fn set_slide_edit_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.slide_edit_open = open;
        if !open {
            self.status_line.clear();
        }
        cx.notify();
    }

    pub fn set_obligations_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.obligations_open = open;
        if !open {
            self.status_line.clear();
        }
        cx.notify();
    }

    pub fn set_agent_panel_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.agent_panel_open = open;
        if !open {
            self.status_line.clear();
        }
        cx.notify();
    }

    pub fn request_live_refresh(&mut self, cx: &mut Context<Self>) {
        self.pending_live_refresh = true;
        cx.notify();
    }

    fn selected_task(&self, cx: &Context<Self>) -> Option<TaskItem> {
        self.list_state
            .read(cx)
            .delegate()
            .selected_item()
            .cloned()
            .or_else(|| {
                self.last_selected.and_then(|ix| {
                    self.list_state
                        .read(cx)
                        .delegate()
                        .items()
                        .get(ix.row)
                        .cloned()
                })
            })
    }

    /// Remove a node and its subtree from the outline tree.
    pub(super) fn remove_outline_node(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(node_id) = uuid::Uuid::parse_str(task_id) else {
            return;
        };
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::DeleteNode { node_id })
        {
            self.status_line = format!("Delete failed: {err}");
            cx.notify();
            return;
        }
        self.finish_node_removal(task_id, window, cx);
    }

    fn finish_node_removal(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.fleet.writer().flush() {
            self.status_line = format!("Delete failed: {err}");
            cx.notify();
            return;
        }
        let _ = self.fleet.reload_if_stale();

        let visible_before =
            Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
        let selected = self.working_set.selected_id.as_deref();
        if self.slide_edit_open {
            cx.emit(TaskListEvent::CloseTaskEdit);
        }
        if self.obligations_open {
            cx.emit(TaskListEvent::CloseObligations);
        }
        if self.agent_panel_open {
            cx.emit(TaskListEvent::CloseAgentPanel);
        }
        if self.edit_open_for.as_deref() == Some(task_id) {
            self.edit_open_for = None;
        }
        self.all_tasks = load_tasks_from_store(&self.fleet, self.active_list_id);
        let visible_after =
            Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
        self.working_set.selected_id =
            model::selection_after_delete(&visible_before, &visible_after, selected, task_id);
        self.rebuild_visible_list(window, cx);
    }

    fn live_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible_before =
            Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
        let selected = self.working_set.selected_id.clone();
        let _ = self.fleet.reload_if_stale();
        self.reload_outline_lists();
        if self.active_list_id.is_none() {
            self.active_list_id = self
                .working_set
                .active_list_id
                .as_deref()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .filter(|id| self.outline_lists.iter().any(|l| l.id == *id))
                .or_else(|| self.outline_lists.first().map(|l| l.id));
        }
        self.all_tasks = load_tasks_from_store(&self.fleet, self.active_list_id);
        if let Some(sel) = selected.clone() {
            if !self.all_tasks.iter().any(|t| t.id == sel) {
                let visible_after =
                    Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
                self.working_set.selected_id = model::selection_after_delete(
                    &visible_before,
                    &visible_after,
                    selected.as_deref(),
                    &sel,
                );
            } else if let Some(id) = selected {
                self.working_set.selected_id = Some(id);
            }
        }
        self.rebuild_visible_list(window, cx);
    }

    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.live_refresh(window, cx);
    }

    pub fn set_status_message(&mut self, message: String, cx: &mut Context<Self>) {
        self.status_line = message;
        cx.notify();
    }

    fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.working_set.tag_filter = None;
        self.search_query.clear();
        self.search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.rebuild_visible_list(window, cx);
    }

    fn clamp_selection(&mut self, ix: IndexPath, cx: &mut Context<Self>) {
        let count = self.list_state.read(cx).delegate().items_count();

        if let Some(last) = self.last_selected {
            if count > 0 {
                let wrapped_up = last.row == 0 && ix.row == count - 1;
                let wrapped_down = last.row == count - 1 && ix.row == 0;
                if wrapped_up || wrapped_down {
                    self.pending_revert = Some(last);
                    cx.notify();
                    return;
                }
            }
        }

        self.last_selected = Some(ix);
    }

    fn apply_pending_revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(revert_to) = self.pending_revert.take() {
            self.last_selected = Some(revert_to);
            self.list_state.update(cx, |state, cx| {
                state.set_selected_index(Some(revert_to), window, cx);
                state.scroll_to_selected_item(window, cx);
            });
            self.sync_selected_id(cx);
        }
    }

    fn move_to_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.close_sort_menu(cx);
        let count = self.list_state.read(cx).delegate().items_count();
        if count == 0 {
            return;
        }
        let row = row.min(count - 1);
        let ix = IndexPath::new(row);
        self.last_selected = Some(ix);
        self.list_state.update(cx, |state, cx| {
            state.set_selected_index(Some(ix), window, cx);
            state.scroll_to_selected_item(window, cx);
        });
        self.sync_selected_id(cx);
    }

    fn move_by_rows(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.list_state.read(cx).delegate().items_count();
        if count == 0 {
            return;
        }
        let current = self.last_selected.unwrap_or_default().row;
        let new_row = if delta >= 0 {
            current.saturating_add(delta as usize).min(count - 1)
        } else {
            current.saturating_sub((-delta) as usize)
        };
        if new_row == current {
            return;
        }
        self.move_to_row(new_row, window, cx);
    }

    fn page_delta(&self, window: &Window) -> usize {
        viewport_row_count(window.viewport_size().height)
    }

    fn on_new_task(&mut self, _: &TaskListNewTask, window: &mut Window, cx: &mut Context<Self>) {
        self.on_create_below(&TaskListCreateBelow, window, cx);
    }

    fn on_create_below(
        &mut self,
        _: &TaskListCreateBelow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_list_id.is_none() {
            self.pending_new_list = true;
            cx.notify();
            return;
        }
        self.create_tree_node_and_edit(CreatePosition::Below, window, cx);
    }

    fn on_enter(&mut self, _: &TaskListEnter, window: &mut Window, cx: &mut Context<Self>) {
        self.on_smart_enter(window, cx);
    }

    fn on_move_up(&mut self, _: &TaskListMoveUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selected_sibling(ReorderDirection::Up, window, cx);
    }

    fn on_move_down(&mut self, _: &TaskListMoveDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selected_sibling(ReorderDirection::Down, window, cx);
    }

    fn move_selected_sibling(
        &mut self,
        direction: ReorderDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task_id) = self.working_set.selected_id.clone() else {
            return;
        };
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return;
        };
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::ReorderSibling { node_id, direction })
        {
            self.status_line = format!("Failed to move item: {err}");
            cx.notify();
            return;
        }
        if let Err(err) = self.fleet.writer().flush() {
            self.status_line = format!("Failed to move item: {err}");
            cx.notify();
            return;
        }
        self.live_refresh(window, cx);
        self.select_task_by_id(&task_id, window, cx);
    }

    fn on_new_list(&mut self, _: &TaskListNewList, window: &mut Window, cx: &mut Context<Self>) {
        self.create_new_list(window, cx);
    }

    fn on_next_list(&mut self, _: &TaskListNextList, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_list(1, window, cx);
    }

    fn on_prev_list(&mut self, _: &TaskListPrevList, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_list(-1, window, cx);
    }

    fn on_edit_nav_up(
        &mut self,
        _: &TaskListEditNavUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_editing() {
            return;
        }
        self.leave_inline_edit_and_move(-1, window, cx);
    }

    fn on_edit_nav_down(
        &mut self,
        _: &TaskListEditNavDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_editing() {
            return;
        }
        self.leave_inline_edit_and_move(1, window, cx);
    }

    fn on_dismiss_overlay(
        &mut self,
        _: &TaskListDismissOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.slide_edit_open {
            cx.emit(TaskListEvent::CloseTaskEdit);
        } else if self.obligations_open {
            cx.emit(TaskListEvent::CloseObligations);
        } else if self.agent_panel_open {
            cx.emit(TaskListEvent::CloseAgentPanel);
        } else if self.is_editing() {
            self.abandon_inline_edit(window, cx, true);
        } else if self.compose_open {
            self.close_compose(window, cx);
        } else if self.credential_prompt_open {
            self.cancel_credential_prompt(window, cx);
        } else if self.sort_menu_open {
            self.close_sort_menu(cx);
        } else if self.open_row_menu.is_some() {
            self.close_row_menu(cx);
        }
    }

    fn on_row_agents(
        &mut self,
        _: &TaskListRowAgents,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task_id) = self
            .working_set
            .selected_id
            .clone()
            .or_else(|| self.selected_task(cx).map(|t| t.id))
        else {
            return;
        };
        self.handle_row_action(RowAction::AgentsControl { task_id }, window, cx);
    }

    fn on_row_shells(
        &mut self,
        _: &TaskListRowShells,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task_id) = self
            .working_set
            .selected_id
            .clone()
            .or_else(|| self.selected_task(cx).map(|t| t.id))
        else {
            return;
        };
        self.handle_row_action(RowAction::ShellsControl { task_id }, window, cx);
    }

    fn on_row_lifecycle(
        &mut self,
        _: &TaskListRowLifecycle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task_id) = self
            .working_set
            .selected_id
            .clone()
            .or_else(|| self.selected_task(cx).map(|t| t.id))
        else {
            return;
        };
        let Some(lifecycle) = self
            .all_tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.lifecycle.clone())
            .filter(|lc| !lc.is_empty())
        else {
            return;
        };
        self.handle_row_action(
            RowAction::LifecycleControl {
                task_id,
                _lifecycle: lifecycle,
            },
            window,
            cx,
        );
    }

    fn on_tag_digit(&mut self, digit: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.selected_task(cx) else {
            return;
        };
        let tags = task.sorted_tags();
        let tag_ix = if digit == 0 { 9 } else { digit - 1 };
        let Some(tag) = tags.get(tag_ix) else {
            return;
        };
        self.toggle_tag_filter(tag, window, cx);
    }

    fn on_tag1(&mut self, _: &TaskListTag1, w: &mut Window, cx: &mut Context<Self>) {
        self.on_tag_digit(1, w, cx);
    }
    fn on_tag2(&mut self, _: &TaskListTag2, w: &mut Window, cx: &mut Context<Self>) {
        self.on_tag_digit(2, w, cx);
    }
    fn on_tag3(&mut self, _: &TaskListTag3, w: &mut Window, cx: &mut Context<Self>) {
        self.on_tag_digit(3, w, cx);
    }
    fn on_tag4(&mut self, _: &TaskListTag4, w: &mut Window, cx: &mut Context<Self>) {
        self.on_tag_digit(4, w, cx);
    }
    fn on_tag5(&mut self, _: &TaskListTag5, w: &mut Window, cx: &mut Context<Self>) {
        self.on_tag_digit(5, w, cx);
    }
    fn on_tag6(&mut self, _: &TaskListTag6, w: &mut Window, cx: &mut Context<Self>) {
        self.on_tag_digit(6, w, cx);
    }
    fn on_tag7(&mut self, _: &TaskListTag7, w: &mut Window, cx: &mut Context<Self>) {
        self.on_tag_digit(7, w, cx);
    }
    fn on_tag8(&mut self, _: &TaskListTag8, w: &mut Window, cx: &mut Context<Self>) {
        self.on_tag_digit(8, w, cx);
    }
    fn on_tag9(&mut self, _: &TaskListTag9, w: &mut Window, cx: &mut Context<Self>) {
        self.on_tag_digit(9, w, cx);
    }
    fn on_tag0(&mut self, _: &TaskListTag0, w: &mut Window, cx: &mut Context<Self>) {
        self.on_tag_digit(0, w, cx);
    }

    fn close_sort_menu(&mut self, cx: &mut Context<Self>) {
        if self.sort_menu_open {
            self.sort_menu_open = false;
            cx.notify();
        }
    }

    fn close_chrome_overlays(&mut self, cx: &mut Context<Self>) {
        self.close_sort_menu(cx);
        self.close_row_menu(cx);
    }

    fn cycle_sort_and_show_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.working_set
            .set_sort_key(self.working_set.sort_key.cycle());
        self.sort_menu_open = true;
        self.rebuild_visible_list(window, cx);
    }

    fn sync_search_from_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).text().to_string();
        if query != self.search_query {
            self.search_query = query;
            cx.defer_in(window, |this, window, cx| {
                this.rebuild_visible_list(window, cx);
            });
        }
    }

    fn on_focus_search(
        &mut self,
        _: &TaskListFocusSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_sort_menu(cx);
        self.search_input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
    }

    fn on_sort_toggle(
        &mut self,
        _: &TaskListSortToggle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_sort_and_show_menu(window, cx);
    }

    fn on_clear_tag_filter(
        &mut self,
        _: &TaskListClearTagFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_sort_menu(cx);
        self.working_set.tag_filter = None;
        self.rebuild_visible_list(window, cx);
    }

    fn on_arrow_up(&mut self, _: &ListArrowUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_by_rows(-1, window, cx);
    }

    fn on_arrow_down(&mut self, _: &ListArrowDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_by_rows(1, window, cx);
    }

    fn on_page_up(&mut self, _: &ListPageUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_by_rows(-(self.page_delta(window) as i32), window, cx);
    }

    fn on_page_down(&mut self, _: &ListPageDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_by_rows(self.page_delta(window) as i32, window, cx);
    }

    fn on_home(&mut self, _: &ListHome, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to_row(0, window, cx);
    }

    fn on_end(&mut self, _: &ListEnd, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.list_state.read(cx).delegate().items_count();
        if count > 0 {
            self.move_to_row(count - 1, window, cx);
        }
    }

    fn on_open(&mut self, _: &TaskListOpen, window: &mut Window, cx: &mut Context<Self>) {
        self.on_smart_enter(window, cx);
    }

    fn on_indent(&mut self, _: &TaskListIndent, window: &mut Window, cx: &mut Context<Self>) {
        self.reparent_selected(1, window, cx);
    }

    fn on_outdent(&mut self, _: &TaskListOutdent, window: &mut Window, cx: &mut Context<Self>) {
        self.reparent_selected(-1, window, cx);
    }

    fn on_select_parent(
        &mut self,
        _: &TaskListSelectParent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task_id) = self.working_set.selected_id.clone() else {
            return;
        };
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) else {
            return;
        };
        if task.has_children && !task.collapsed {
            self.set_collapsed(&task_id, true, window, cx);
        } else {
            self.select_parent(window, cx);
        }
    }

    fn select_parent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.working_set.selected_id.clone() else {
            return;
        };
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return;
        };
        let parent_id = {
            let projection = self.fleet.projection();
            let guard = projection.lock().unwrap();
            let conn = guard.connection();
            let outline = tod_store::outline::repos::OutlineRepo::new(&conn);
            outline
                .get_entry(node_id)
                .ok()
                .flatten()
                .and_then(|entry| entry.parent_id)
        };
        let Some(parent_id) = parent_id else {
            return;
        };
        self.select_task_by_id(&parent_id.to_string(), window, cx);
    }

    fn on_expand(&mut self, _: &TaskListExpand, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.working_set.selected_id.clone() else {
            return;
        };
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) else {
            return;
        };
        if task.has_children && task.collapsed {
            self.set_collapsed(&task_id, false, window, cx);
        }
    }

    fn on_create_child(
        &mut self,
        _: &TaskListCreateChild,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_editing() {
            let title = self.inline_edit_title(cx);
            if self.is_draft_edit() && title.is_empty() {
                self.abandon_inline_edit(window, cx, true);
            } else if !self.commit_inline_edit(window, cx) {
                return;
            }
        }
        self.create_tree_node_and_edit(CreatePosition::Child, window, cx);
    }

    fn on_create_above(
        &mut self,
        _: &TaskListCreateAbove,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.create_tree_node_and_edit(CreatePosition::Above, window, cx);
    }

    fn delete_selected_node(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.selected_task(cx) else {
            return;
        };
        self.remove_outline_node(&task.id, window, cx);
    }

    fn on_delete(&mut self, _: &TaskListDelete, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            return;
        }
        self.delete_selected_node(window, cx);
    }

    fn reparent_selected(&mut self, direction: i32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.working_set.selected_id.clone() else {
            return;
        };
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return;
        };
        let Some(list_id) = self.active_list_id else {
            return;
        };
        let rows = self.fleet.flatten_outline(list_id).unwrap_or_default();
        let Some(ix) = rows.iter().position(|r| r.node.id == node_id) else {
            return;
        };
        let (new_parent, ordinal) = {
            let projection = self.fleet.projection();
            let guard = projection.lock().unwrap();
            let conn = guard.connection();
            let outline = tod_store::outline::repos::OutlineRepo::new(&conn);
            if direction > 0 {
                if ix == 0 {
                    return;
                }
                let prev = &rows[ix - 1];
                let ord = outline
                    .next_ordinal(list_id, Some(prev.node.id))
                    .unwrap_or(0);
                (Some(prev.node.id), ord)
            } else {
                let Some(entry) = outline.get_entry(node_id).ok().flatten() else {
                    return;
                };
                if entry.parent_id.is_none() {
                    return;
                };
                let parent_entry = outline.get_entry(entry.parent_id.unwrap()).ok().flatten();
                let grandparent = parent_entry.and_then(|p| p.parent_id);
                let ord = entry.ordinal + 1;
                (grandparent, ord)
            }
        };
        let _ = self.fleet.enqueue_outline(OutlineMutation::ReparentNode {
            node_id,
            parent_id: new_parent,
            ordinal,
        });
        let _ = self.fleet.writer().flush();
        self.live_refresh(window, cx);
    }

    fn on_row_edit(&mut self, _: &TaskListRowEdit, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self
            .working_set
            .selected_id
            .clone()
            .or_else(|| self.selected_task(cx).map(|t| t.id))
        else {
            return;
        };
        self.start_inline_edit(&task_id, window, cx);
    }

    fn on_open_edit_panel(
        &mut self,
        _: &TaskListOpenEditPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task_id) = self
            .working_set
            .selected_id
            .clone()
            .or_else(|| self.selected_task(cx).map(|t| t.id))
        else {
            return;
        };
        self.open_task_edit_panel(&task_id, window, cx);
    }

    fn on_open_obligations(
        &mut self,
        _: &TaskListOpenObligations,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task_id) = self
            .working_set
            .selected_id
            .clone()
            .or_else(|| self.selected_task(cx).map(|t| t.id))
        else {
            return;
        };
        self.open_obligations_panel(&task_id, window, cx);
    }

    fn render_header(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement {
        let theme = cx.theme().clone();
        let border = theme.border;
        let muted_foreground = theme.muted_foreground;
        let sort_label = format!(
            "Sort {} {}",
            self.working_set.sort_key.label(),
            self.working_set.sort_direction.arrow()
        );

        let mut search = Input::new(&self.search_input).cleanable(true).w_full();
        if let Some(pill) =
            render_shortcut_pill(window, &TaskListFocusSearch, TASK_LIST_CONTEXT, cx)
        {
            search = search.suffix(pill);
        }

        let list_title = self.active_list_title();
        let list_count = self.outline_lists.len();

        let row = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(border)
            .child(self.render_app_nav(window, cx))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("prev-list")
                            .label("◀")
                            .small()
                            .disabled(list_count <= 1)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_prev_list(&TaskListPrevList, window, cx);
                            })),
                    )
                    .child(div().text_sm().min_w(px(80.)).child(list_title))
                    .child(
                        Button::new("next-list")
                            .label("▶")
                            .small()
                            .disabled(list_count <= 1)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_next_list(&TaskListNextList, window, cx);
                            })),
                    )
                    .child(chrome_control_with_shortcut(
                        Button::new("new-list")
                            .label("New list")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_new_list(&TaskListNewList, window, cx);
                            })),
                        window,
                        &TaskListNewList,
                        TASK_LIST_CONTEXT,
                        cx,
                    )),
            )
            .child(chrome_control_with_shortcut(
                Button::new("new-task")
                    .label("New item")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_create_below(&TaskListCreateBelow, window, cx);
                    })),
                window,
                &TaskListCreateBelow,
                TASK_LIST_CONTEXT,
                cx,
            ))
            .child(div().flex_1().min_w_0().child(search));

        let mut row = row;

        if let Some(tag) = &self.working_set.tag_filter {
            row = row
                .child(div().text_xs().text_color(muted_foreground).child("Tag"))
                .child(
                    Button::new("active-tag-filter")
                        .label(tag.clone())
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.working_set.tag_filter = None;
                            this.rebuild_visible_list(window, cx);
                        })),
                )
                .child(chrome_control_with_shortcut(
                    Button::new("clear-tag-filter")
                        .label("Clear tag")
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_clear_tag_filter(&TaskListClearTagFilter, window, cx);
                        })),
                    window,
                    &TaskListClearTagFilter,
                    TASK_LIST_CONTEXT,
                    cx,
                ));
        }

        row = row.child(chrome_control_with_shortcut(
            Button::new("sort-toggle")
                .label(sort_label)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.cycle_sort_and_show_menu(window, cx);
                })),
            window,
            &TaskListSortToggle,
            TASK_LIST_CONTEXT,
            cx,
        ));
        row
    }

    fn render_sort_menu_overlay(&self, cx: &mut Context<Self>) -> Option<impl gpui::IntoElement> {
        if !self.sort_menu_open {
            return None;
        }
        let theme = cx.theme();
        let active_key = self.working_set.sort_key;
        let active_dir = self.working_set.sort_direction;
        Some(
            div()
                .absolute()
                .top_10()
                .right_3()
                .min_w_40()
                .p_1()
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .shadow_lg()
                .rounded_md()
                .v_flex()
                .gap_0p5()
                .children(SortKey::ALL.into_iter().enumerate().map(|(idx, key)| {
                    let direction = if key == active_key {
                        active_dir
                    } else {
                        WorkingSet::initial_direction_for_key(key)
                    };
                    let label = format!("{} {}", key.label(), direction.arrow());
                    let highlighted = key == active_key;
                    let mut btn = Button::new(("sort-option", idx))
                        .label(label)
                        .ghost()
                        .w_full();
                    if highlighted {
                        btn = btn.primary();
                    }
                    btn.on_click(cx.listener(move |this, _, window, cx| {
                        this.working_set.set_sort_key(key);
                        this.close_sort_menu(cx);
                        this.rebuild_visible_list(window, cx);
                    }))
                })),
        )
    }

    fn body_state(&self, cx: &Context<Self>) -> BodyState {
        let total = self.all_tasks.len();
        let visible_count = self.list_state.read(cx).delegate().items_count();
        if total == 0 {
            BodyState::Empty
        } else if visible_count == 0 {
            BodyState::NoMatches
        } else {
            BodyState::List
        }
    }
}

enum BodyState {
    Empty,
    NoMatches,
    List,
}

impl HasAppNav for TaskListView {
    fn app_nav_mut(&mut self) -> &mut AppNavMenu {
        &mut self.app_nav
    }

    fn app_nav_current(&self) -> Option<AppDestination> {
        Some(AppDestination::Tasks)
    }

    fn app_nav_fallback_focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Focusable for TaskListView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EventEmitter<TaskListEvent> for TaskListView {}

impl Render for TaskListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        self.sync_search_from_input(window, cx);
        if self.pending_live_refresh {
            self.pending_live_refresh = false;
            self.live_refresh(window, cx);
        }
        self.apply_pending_revert(window, cx);
        if self.pending_compose_submit {
            self.pending_compose_submit = false;
            self.submit_compose(window, cx);
        }
        if self.pending_credential_submit {
            self.pending_credential_submit = false;
            self.submit_credential_prompt(window, cx);
        }
        if self.pending_new_list {
            self.pending_new_list = false;
            self.create_new_list(window, cx);
        }
        if self.pending_create_below {
            self.pending_create_below = false;
            self.create_tree_node_and_edit(CreatePosition::Below, window, cx);
        }
        if let Some(pending) = self.pending_ticket_import.take() {
            self.apply_pending_ticket_import(pending, window, cx);
        }
        if self.pending_abandon_edit {
            self.pending_abandon_edit = false;
            self.abandon_inline_edit(window, cx, false);
        }
        if self.pending_inline_commit {
            self.pending_inline_commit = false;
            let generation = self.inline_enter_generation;
            cx.defer_in(window, move |this, window, cx| {
                if this.inline_enter_generation != generation {
                    return;
                }
                this.on_smart_enter(window, cx);
            });
        }
        self.drain_row_actions(window, cx);

        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let body_state = self.body_state(cx);

        let body = div().flex_1().min_h_0().overflow_hidden().v_flex();
        let body = if self.compose_open {
            body.child(self.render_compose_row(cx))
        } else {
            body
        };
        let body = match body_state {
            BodyState::Empty => body
                .flex()
                .items_center()
                .justify_center()
                .text_color(muted)
                .child(if self.active_list_id.is_some() {
                    "Press Enter to add an item · F2 or double-click to edit"
                } else {
                    "Press Enter to create your first list"
                }),
            BodyState::NoMatches => body
                .v_flex()
                .items_center()
                .justify_center()
                .gap_3()
                .child(div().text_color(muted).child("No tasks match."))
                .child(
                    Button::new("clear-all-filters")
                        .label("Clear filters")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.clear_filters(window, cx);
                        })),
                ),
            BodyState::List => body.child(self.list_view.render(window, cx)),
        };

        let root = div()
            .key_context(TASK_LIST_CONTEXT)
            .track_focus(&self.focus_handle)
            .v_flex()
            .size_full()
            .relative()
            .on_action(cx.listener(Self::on_arrow_up))
            .on_action(cx.listener(Self::on_arrow_down))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_new_task))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_dismiss_overlay))
            .on_action(cx.listener(Self::on_sort_toggle))
            .on_action(cx.listener(Self::on_clear_tag_filter))
            .on_action(cx.listener(Self::on_row_agents))
            .on_action(cx.listener(Self::on_row_lifecycle))
            .on_action(cx.listener(Self::on_row_shells))
            .on_action(cx.listener(Self::on_row_edit))
            .on_action(cx.listener(Self::on_open_edit_panel))
            .on_action(cx.listener(Self::on_open_obligations))
            .on_action(cx.listener(Self::on_tag1))
            .on_action(cx.listener(Self::on_tag2))
            .on_action(cx.listener(Self::on_tag3))
            .on_action(cx.listener(Self::on_tag4))
            .on_action(cx.listener(Self::on_tag5))
            .on_action(cx.listener(Self::on_tag6))
            .on_action(cx.listener(Self::on_tag7))
            .on_action(cx.listener(Self::on_tag8))
            .on_action(cx.listener(Self::on_tag9))
            .on_action(cx.listener(Self::on_tag0))
            .on_action(cx.listener(Self::on_enter))
            .on_action(cx.listener(Self::on_create_below))
            .on_action(cx.listener(Self::on_move_up))
            .on_action(cx.listener(Self::on_move_down))
            .on_action(cx.listener(Self::on_edit_nav_up))
            .on_action(cx.listener(Self::on_edit_nav_down))
            .on_action(cx.listener(Self::on_new_list))
            .on_action(cx.listener(Self::on_next_list))
            .on_action(cx.listener(Self::on_prev_list))
            .on_action(cx.listener(Self::on_indent))
            .on_action(cx.listener(Self::on_outdent))
            .on_action(cx.listener(Self::on_select_parent))
            .on_action(cx.listener(Self::on_expand))
            .on_action(cx.listener(Self::on_create_child))
            .on_action(cx.listener(Self::on_create_above))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(on_app_nav_toggle::<Self>))
            .child(self.render_header(window, cx))
            .child(body)
            .when(!self.status_line.is_empty(), |el| {
                el.child(
                    div().px_3().py_1().border_t_1().border_color(border).child(
                        selectable_text("task-list-status", self.status_line.clone(), window, cx)
                            .text_xs()
                            .text_color(muted),
                    ),
                )
            })
            .when_some(self.render_sort_menu_overlay(cx), |el, menu| el.child(menu))
            .when(self.credential_prompt_open, |el| {
                el.child(self.render_credential_prompt_overlay(cx))
            });

        root
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::large_fixture_set;
    use super::model::ListWorkingSet;
    use super::model::filter_and_sort_tasks;
    use super::model::selection_after_delete;
    use super::model::{SortDirection, SortKey};

    #[test]
    fn large_fixture_set_reaches_scale_target() {
        let tasks = large_fixture_set(500);
        assert_eq!(tasks.len(), 500);
        let visible = filter_and_sort_tasks(&tasks, "", &ListWorkingSet::default_sort());
        assert_eq!(visible.len(), 500);
    }

    #[test]
    fn delete_moves_selection_to_nearest_visible() {
        let tasks = large_fixture_set(3);
        let ws = ListWorkingSet::default_sort();
        let visible_before = filter_and_sort_tasks(&tasks, "", &ws);
        let deleted_id = visible_before[1].id.clone();
        let remaining: Vec<_> = tasks
            .iter()
            .filter(|t| t.id != deleted_id)
            .cloned()
            .collect();
        let visible_after = filter_and_sort_tasks(&remaining, "", &ws);
        let next = selection_after_delete(
            &visible_before,
            &visible_after,
            Some(&deleted_id),
            &deleted_id,
        );
        assert!(next.is_some());
        assert_ne!(next.unwrap(), deleted_id);
    }

    #[test]
    fn delete_last_visible_task_clears_selection() {
        let tasks = large_fixture_set(1);
        let only = tasks[0].clone();
        let ws = ListWorkingSet::default_sort();
        let visible_before = filter_and_sort_tasks(&tasks, "", &ws);
        let visible_after: Vec<_> = Vec::new();
        let next =
            selection_after_delete(&visible_before, &visible_after, Some(&only.id), &only.id);
        assert!(next.is_none());
    }

    #[test]
    fn title_sort_flattens_display_depth() {
        let mut parent = large_fixture_set(1)[0].clone();
        parent.depth = 0;
        parent.title = "Parent".into();
        let mut child = parent.clone();
        child.id = "child-id".into();
        child.depth = 1;
        child.title = "Alpha child".into();
        child.tree_ordinal = 1;
        let ws = ListWorkingSet {
            sort_key: SortKey::Title,
            sort_direction: SortDirection::Asc,
            ..ListWorkingSet::default_sort()
        };
        let visible = filter_and_sort_tasks(&[parent, child], "", &ws);
        assert_eq!(visible[0].title, "Alpha child");
        assert_eq!(visible[0].depth, 0);
        assert!(!visible[0].has_children);
    }
}
