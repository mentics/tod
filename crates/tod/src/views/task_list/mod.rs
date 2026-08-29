mod compose;
mod delegate;
pub(crate) mod fixtures;
mod from_ticket;
mod model;
mod row_menu;
mod working_set;

pub use model::TaskItem;
pub use model::{ListWorkingSet, SortKey};

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::fleet::{FleetMutation, FleetStore};
use crate::interview::{TodPaths, interview_work_remains};
use crate::process::interview_phase_for_lifecycle;
use crate::ui::actionable::{chrome_control_with_shortcut, render_shortcut_pill};
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav, on_app_nav_toggle};
use crate::ui::key_context::{self, INPUT};
use crate::ui::list::{
    ListArrowDown, ListArrowUp, ListEnd, ListHome, ListPageDown, ListPageUp, ListView,
    viewport_row_count,
};
use delegate::{RowAction, TaskListDelegate};
use fixtures::load_tasks_from_store;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Render, Styled, Subscription, Timer, Window, actions, div,
    prelude::FluentBuilder, px,
};
use gpui_component::IndexPath;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::list::{ListEvent, ListState};
use gpui_component::{ActiveTheme, Sizable, StyledExt};
use model::{ListWorkingSet as WorkingSet, filter_and_sort_tasks, nearest_visible_id};
use row_menu::RowMenuKind;
use working_set::{load_working_set, save_working_set};

actions!(
    task_list,
    [
        TaskListOpen,
        TaskListNewTask,
        TaskListFocusSearch,
        TaskListClearFilters,
        TaskListSortToggle,
        TaskListClearTagFilter,
        TaskListRowAgents,
        TaskListRowShells,
        TaskListRowEdit,
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
        KeyBinding::new("enter", TaskListOpen, context),
        KeyBinding::new("n", TaskListNewTask, context),
        KeyBinding::new("/", TaskListFocusSearch, context),
        KeyBinding::new("escape", TaskListDismissOverlay, context),
        KeyBinding::new("s", TaskListSortToggle, context),
        KeyBinding::new("cmd-shift-t", TaskListClearTagFilter, context),
        KeyBinding::new("a", TaskListRowAgents, context),
        KeyBinding::new("t", TaskListRowShells, context),
        KeyBinding::new("e", TaskListRowEdit, context),
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
        // Edit-specific: Escape cancels compose/overlays while a text field is focused.
        KeyBinding::new("escape", TaskListDismissOverlay, Some(INPUT)),
    ]);
}

#[derive(Debug, Clone)]
pub enum TaskListEvent {
    OpenInterview {
        task_id: String,
        entity_path: PathBuf,
        lifecycle: String,
        title: String,
    },
    OpenTaskEdit {
        task_id: String,
        title: String,
    },
    OpenNewTaskCompose,
    CloseTaskEdit,
    OpenLifecycle {
        task_id: String,
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
    DeleteTask {
        task_id: String,
    },
    StatusMessage(String),
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
    selection_before_compose: Option<String>,
    open_row_menu: Option<(RowMenuKind, String)>,
    sort_menu_open: bool,
    edit_open_for: Option<String>,
    pending_compose_submit: bool,
    pending_lifecycle_next: Option<String>,
    pending_live_refresh: bool,
    pending_delete_task_id: Option<String>,
    status_line: String,
    config_dir: PathBuf,
    fleet: Arc<FleetStore>,
    app_nav: AppNavMenu,
    _list_subscription: Subscription,
    _compose_subscription: Subscription,
}

impl TaskListView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, fleet: Arc<FleetStore>) -> Self {
        let paths = TodPaths::discover().ok();
        let config_dir = paths
            .as_ref()
            .map(|p| p.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".local/.config/tod"));
        let repo_root = paths
            .as_ref()
            .map(|p| p.repo_root().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        let mut working_set = load_working_set(&config_dir);
        let all_tasks = load_tasks_from_store(&fleet, &repo_root);
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search tasks…"));

        let compose_title_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("Title or ticket id (e.g. TOD-142)"));
        let _compose_subscription = cx.subscribe(&compose_title_input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.pending_compose_submit = true;
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
            ListEvent::Select(ix) => {
                this.close_sort_menu(cx);
                this.clamp_selection(*ix, cx);
                if this.pending_revert.is_none() {
                    this.sync_selected_id(cx);
                }
            }
            ListEvent::Confirm(_) => {
                if let Some(id) = this
                    .working_set
                    .selected_id
                    .clone()
                    .or_else(|| this.selected_task(cx).map(|t| t.id))
                {
                    this.pending_lifecycle_next = Some(id);
                    cx.notify();
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
            selection_before_compose: None,
            open_row_menu: None,
            sort_menu_open: false,
            edit_open_for: None,
            pending_compose_submit: false,
            pending_lifecycle_next: None,
            pending_live_refresh: false,
            pending_delete_task_id: None,
            status_line: String::new(),
            config_dir,
            fleet: fleet.clone(),
            app_nav: AppNavMenu::default(),
            _list_subscription,
            _compose_subscription,
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
            state.set_selected_index(selected_ix, window, cx);
            if selected_ix.is_some() {
                state.scroll_to_selected_item(window, cx);
            }
            cx.notify();
        });

        self.last_selected = selected_ix;
        self.pending_revert = None;
        self.persist_working_set();
        self.sync_edit_follows_selection(previous_id, cx);
        cx.notify();
    }

    fn sync_edit_follows_selection(&mut self, previous_id: Option<String>, cx: &mut Context<Self>) {
        if self.edit_open_for.is_none() {
            return;
        }
        let new_id = self.working_set.selected_id.clone();
        if new_id == previous_id {
            return;
        }
        let Some(id) = new_id else {
            self.edit_open_for = None;
            cx.emit(TaskListEvent::CloseTaskEdit);
            return;
        };
        let Some(task) = self.all_tasks.iter().find(|t| t.id == id) else {
            return;
        };
        self.edit_open_for = Some(id.clone());
        cx.emit(TaskListEvent::OpenTaskEdit {
            task_id: id,
            title: task.title.clone(),
        });
    }

    fn sync_selected_id(&mut self, cx: &mut Context<Self>) {
        let previous = self.working_set.selected_id.clone();
        let Some(item) = self.list_state.read(cx).delegate().selected_item().cloned() else {
            return;
        };
        self.working_set.selected_id = Some(item.id.clone());
        self.persist_working_set();
        self.sync_edit_follows_selection(previous, cx);
    }

    fn persist_working_set(&self) {
        save_working_set(&self.config_dir, &self.working_set);
    }

    fn drain_row_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let actions: Vec<RowAction> = self.action_sink.borrow_mut().drain(..).collect();
        let suppress_row_chrome: std::collections::HashSet<String> = actions
            .iter()
            .filter_map(|action| match action {
                RowAction::OpenEdit { task_id }
                | RowAction::AgentsControl { task_id }
                | RowAction::ShellsControl { task_id }
                | RowAction::LifecycleControl { task_id, .. }
                | RowAction::ToggleTagFilter { task_id, .. } => Some(task_id.clone()),
                _ => None,
            })
            .collect();
        for action in actions {
            if let RowAction::RowChrome { task_id } = &action {
                if suppress_row_chrome.contains(task_id) {
                    continue;
                }
            }
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
                self.bump_interaction(&task_id, window, cx);
                self.emit_open_edit_for(&task_id, cx);
            }
            RowAction::RowChrome { task_id } => {
                self.dismiss_compose_for_row_action(window, cx);
                self.select_task_by_id(&task_id, window, cx);
                self.run_lifecycle_next(&task_id, window, cx);
            }
            RowAction::ToggleTagFilter { task_id, tag } => {
                self.select_task_by_id(&task_id, window, cx);
                self.toggle_tag_filter(&tag, window, cx);
            }
            RowAction::AgentsControl { task_id } => {
                self.select_task_by_id(&task_id, window, cx);
                self.bump_interaction(&task_id, window, cx);
                self.handle_agents_control(&task_id, cx);
            }
            RowAction::ShellsControl { task_id } => {
                self.select_task_by_id(&task_id, window, cx);
                self.bump_interaction(&task_id, window, cx);
                self.handle_shells_control(&task_id, cx);
            }
            RowAction::LifecycleControl {
                task_id,
                lifecycle: _,
            } => {
                self.select_task_by_id(&task_id, window, cx);
                self.run_lifecycle_next(&task_id, window, cx);
            }
        }
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

    fn handle_agents_control(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) else {
            return;
        };
        if task.agents.is_empty() {
            cx.emit(TaskListEvent::OpenAgentDetail {
                task_id: task_id.to_string(),
                agent_id: None,
            });
            self.status_line = format!("Creating agent for {}", task.title);
        } else {
            self.toggle_agents_menu(task_id, cx);
        }
    }

    fn handle_shells_control(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) else {
            return;
        };
        if task.shells.is_empty() {
            if task.agents.len() > 1 {
                self.toggle_shell_agent_picker(task_id, cx);
                return;
            }
            cx.emit(TaskListEvent::OpenShell {
                task_id: task_id.to_string(),
                shell_id: None,
                agent_id: None,
            });
        } else {
            self.toggle_shells_menu(task_id, cx);
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
                if Self::is_process_backed(&task.entity_path) {
                    if interview_work_remains(&task.entity_path, lifecycle) {
                        cx.emit(TaskListEvent::OpenInterview {
                            task_id: task_id.to_string(),
                            entity_path: task.entity_path.clone(),
                            lifecycle: lifecycle.to_string(),
                            title: task.title.clone(),
                        });
                        self.status_line = format!("Opening interview for {}", task.title);
                    } else {
                        cx.emit(TaskListEvent::OpenLifecycle {
                            task_id: task_id.to_string(),
                            lifecycle: lifecycle.to_string(),
                        });
                        self.status_line = format!("Lifecycle panel: {lifecycle}");
                    }
                } else {
                    self.status_line =
                        "Interview unavailable — task has no linked process directory.".into();
                }
                return;
            }
        }
        cx.emit(TaskListEvent::OpenLifecycle {
            task_id: task_id.to_string(),
            lifecycle: lifecycle.to_string(),
        });
        self.status_line = format!("Lifecycle panel: {lifecycle}");
    }

    /// Open the lifecycle transition panel for a task (bypasses interview routing).
    pub fn open_lifecycle_panel(&mut self, task_id: &str, lifecycle: &str, cx: &mut Context<Self>) {
        cx.emit(TaskListEvent::OpenLifecycle {
            task_id: task_id.to_string(),
            lifecycle: lifecycle.to_string(),
        });
        self.status_line = format!("Lifecycle panel: {lifecycle}");
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
        self.edit_open_for = Some(task_id.to_string());
        cx.emit(TaskListEvent::OpenTaskEdit {
            task_id: task_id.to_string(),
            title: task.title.clone(),
        });
        self.status_line = format!("Edit: {}", task.title);
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

    /// Queue permanent delete (task-crud integration); runs on next render when window is available.
    pub fn schedule_remove_task(&mut self, task_id: String, cx: &mut Context<Self>) {
        self.pending_delete_task_id = Some(task_id);
        cx.notify();
    }

    /// Remove a permanently deleted task and move selection to the nearest visible row.
    pub fn remove_task(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.fleet.enqueue(FleetMutation::DeleteTask {
            id: task_id.to_string(),
        }) {
            self.status_line = format!("Delete failed: {err}");
            cx.notify();
            return;
        }
        if let Err(err) = self.fleet.writer().flush() {
            self.status_line = format!("Delete failed: {err}");
            cx.notify();
            return;
        }
        let _ = self.fleet.reload_if_stale();

        let visible_before =
            Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
        let selected = self.working_set.selected_id.as_deref();
        self.all_tasks.retain(|t| t.id != task_id);
        if self.edit_open_for.as_deref() == Some(task_id) {
            self.edit_open_for = None;
            cx.emit(TaskListEvent::CloseTaskEdit);
        }
        let visible_after =
            Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
        self.working_set.selected_id =
            model::selection_after_delete(&visible_before, &visible_after, selected, task_id);
        self.rebuild_visible_list(window, cx);
    }

    fn is_process_backed(entity_path: &std::path::Path) -> bool {
        entity_path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .windows(2)
            .any(|w| w[0] == "doc" && w[1] == "process")
    }

    fn live_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible_before =
            Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
        let selected = self.working_set.selected_id.clone();
        let paths = TodPaths::discover().ok();
        let repo_root = paths
            .as_ref()
            .map(|p| p.repo_root())
            .unwrap_or_else(|| std::path::Path::new("."));
        let _ = self.fleet.reload_if_stale();
        let scanned = load_tasks_from_store(&self.fleet, repo_root);
        self.merge_live_tasks(scanned);
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

    fn merge_live_tasks(&mut self, scanned: Vec<TaskItem>) {
        let scanned_paths: std::collections::HashSet<_> =
            scanned.iter().map(|t| t.entity_path.clone()).collect();
        self.all_tasks.retain(|t| {
            !Self::is_process_backed(&t.entity_path) || scanned_paths.contains(&t.entity_path)
        });
        for fresh in scanned {
            if let Some(existing) = self
                .all_tasks
                .iter_mut()
                .find(|t| t.entity_path == fresh.entity_path)
            {
                existing.title = fresh.title;
                existing.lifecycle = fresh.lifecycle;
                existing.ticket_id = fresh.ticket_id.clone();
                existing.tags = fresh.tags.clone();
                if !fresh.agents.is_empty() {
                    existing.agents = fresh.agents.clone();
                }
                if !fresh.shells.is_empty() {
                    existing.shells = fresh.shells.clone();
                }
            } else if !self.all_tasks.iter().any(|t| t.id == fresh.id) {
                self.all_tasks.push(fresh);
            }
        }
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

    fn page_delta(&self) -> usize {
        viewport_row_count(px(728.))
    }

    fn on_new_task(&mut self, _: &TaskListNewTask, window: &mut Window, cx: &mut Context<Self>) {
        self.open_compose(window, cx);
        cx.emit(TaskListEvent::OpenNewTaskCompose);
    }

    fn on_dismiss_overlay(
        &mut self,
        _: &TaskListDismissOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.compose_open {
            self.close_compose(window, cx);
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

    fn on_clear_filters(
        &mut self,
        _: &TaskListClearFilters,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_sort_menu(cx);
        self.clear_filters(window, cx);
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
        self.move_by_rows(-(self.page_delta() as i32), window, cx);
    }

    fn on_page_down(&mut self, _: &ListPageDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_by_rows(self.page_delta() as i32, window, cx);
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
        let Some(id) = self
            .working_set
            .selected_id
            .clone()
            .or_else(|| self.selected_task(cx).map(|t| t.id))
        else {
            return;
        };
        self.run_lifecycle_next(&id, window, cx);
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
        self.handle_row_action(RowAction::OpenEdit { task_id }, window, cx);
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

        let mut row = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(border)
            .child(self.render_app_nav(window, cx))
            .child(chrome_control_with_shortcut(
                Button::new("new-task")
                    .label("New task")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_new_task(&TaskListNewTask, window, cx);
                    })),
                window,
                &TaskListNewTask,
                TASK_LIST_CONTEXT,
                cx,
            ))
            .child(div().flex_1().min_w_0().child(search));

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
        if let Some(task_id) = self.pending_delete_task_id.take() {
            self.remove_task(&task_id, window, cx);
        }
        self.apply_pending_revert(window, cx);
        if self.pending_compose_submit {
            self.pending_compose_submit = false;
            self.submit_compose(window, cx);
        }
        if let Some(task_id) = self.pending_lifecycle_next.take() {
            self.run_lifecycle_next(&task_id, window, cx);
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
                .child("No tasks"),
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

        let mut root = div()
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
            .on_action(cx.listener(Self::on_row_shells))
            .on_action(cx.listener(Self::on_row_edit))
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
            .on_action(cx.listener(on_app_nav_toggle::<Self>))
            .child(self.render_header(window, cx))
            .child(body)
            .when(!self.status_line.is_empty(), |el| {
                el.child(
                    div()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(border)
                        .text_xs()
                        .text_color(muted)
                        .child(self.status_line.clone()),
                )
            })
            .when_some(self.render_sort_menu_overlay(cx), |el, menu| el.child(menu))
            .when_some(self.render_row_menu_overlay(cx), |el, menu| el.child(menu));

        root
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::large_fixture_set;
    use super::model::ListWorkingSet;
    use super::model::filter_and_sort_tasks;
    use super::model::selection_after_delete;

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
}
