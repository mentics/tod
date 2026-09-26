mod compose;
mod context_menu;
mod credential_prompt;
use credential_prompt::PendingCredentialRequest;
mod delegate;
mod edit;
pub(crate) mod fixtures;
mod from_ticket;
use from_ticket::PendingTicketImport;
pub(crate) use tod_core::task::model;
mod row_menu;
pub(crate) use tod_core::task::working_set;

pub use model::SortKey;
pub use model::TaskItem;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::interview::TodPaths;
use crate::ui::journey::Source;
use crate::ui::actionable::{chrome_control_with_shortcut, render_shortcut_pill};
use crate::ui::agent_chat::{OpenAgentChat, OpenConversation};
use crate::ui::report_problem::{OpenReportDialog, ReportProblem};
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav, on_app_nav_toggle};
use crate::ui::key_context;
use crate::ui::list::{
    ListArrowDown, ListArrowUp, ListEnd, ListHome, ListPageDown, ListPageUp, ListView,
    viewport_row_count,
};
use crate::ui::pane_nav::{PaneFocusRight, bind_modified_pane_nav};
use crate::views::incoming_check::{IncomingCheck, outcome_line};
use delegate::{RowAction, TaskListDelegate};
use fixtures::load_tasks_from_store;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, KeyBinding,
    ParentElement, Render, Styled, Subscription, Window, actions, div, prelude::FluentBuilder, px,
};
use gpui_component::IndexPath;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::list::{ListEvent, ListState};
use gpui_component::menu::PopupMenu;
use gpui_component::{ActiveTheme, Disableable, Sizable, StyledExt};
use model::{ListWorkingSet as WorkingSet, filter_and_sort_tasks, nearest_visible_id};
use row_menu::RowMenuKind;
use tod_core::process::interview_phase_for_lifecycle;
use tod_store::fleet::{FleetStore, code_editors, validate_interview_workspace};
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
        TaskListOpenCode,
        TaskListOpenActionPanel,
        TaskListRowLifecycle,
        TaskListRowEdit,
        TaskListOpenEditPanel,
        TaskListOpenEditPanelCtrl,
        TaskListOpenObligations,
        TaskListOpenPlan,
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
        TaskListRefreshGenerator,
        TaskListOpenExternal,
        TaskListCopy,
        TaskListPaste,
        TaskListToggleMark,
        TaskListCheckIncoming,
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
        KeyBinding::new("c", TaskListOpenCode, context),
        KeyBinding::new("f", TaskListOpenActionPanel, context),
        KeyBinding::new("r", TaskListRefreshGenerator, context),
        KeyBinding::new("x", TaskListOpenExternal, context),
        KeyBinding::new("o", TaskListOpenObligations, context),
        KeyBinding::new("p", TaskListOpenPlan, context),
        KeyBinding::new("e", TaskListOpenEditPanel, context),
        // Unified view only: Ctrl+E opens the item's panel as a Ctrl+click
        // would, beside the current column rather than replacing it
        // (`doc/ui/unified-view.md` "Keys"). The plain Tasks view has no
        // columns, so it treats this the same as `e`.
        KeyBinding::new("ctrl-e", TaskListOpenEditPanelCtrl, context),
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
        // While the row being edited is a freshly created draft node, Tab/Shift-Tab
        // still reparent it instead of being swallowed by the input's tab handling.
        KeyBinding::new(
            "tab",
            TaskListIndent,
            Some(key_context::including_input(TASK_LIST_CONTEXT)),
        ),
        KeyBinding::new(
            "shift-tab",
            TaskListOutdent,
            Some(key_context::including_input(TASK_LIST_CONTEXT)),
        ),
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
        KeyBinding::new("secondary-up", TaskListMoveUp, context),
        KeyBinding::new("secondary-down", TaskListMoveDown, context),
        KeyBinding::new("delete", TaskListDelete, context),
        KeyBinding::new("backspace", TaskListDelete, context),
        KeyBinding::new("ctrl-c", TaskListCopy, context),
        KeyBinding::new("space", TaskListToggleMark, context),
        KeyBinding::new("i", TaskListCheckIncoming, context),
        KeyBinding::new("ctrl-v", TaskListPaste, context),
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
    // Left/Right drive the tree here, so crossing to the right drawer uses Ctrl+arrows.
    bind_modified_pane_nav(cx, TASK_LIST_CONTEXT);
    key_context::bind_panel_escape(cx, TaskListDismissOverlay, TASK_LIST_CONTEXT);
}

/// What a node is waiting on the user for, as the host (W6's attention
/// module, once it exists) reports it: how many pending decisions, and
/// since when the oldest of them has been waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attention {
    pub count: usize,
    pub waiting_since: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub enum TaskListEvent {
    /// Ctrl+Right — move keyboard focus to the right drawer, if one is open.
    FocusDrawer,
    OpenInterview {
        task_id: String,
        node_id: uuid::Uuid,
        lifecycle: String,
        title: String,
    },
    OpenTaskEdit {
        task_id: String,
    },
    OpenObligations {
        task_id: String,
        title: String,
    },
    OpenPlan {
        task_id: String,
        title: String,
    },
    /// Escape from the tree — close the right drawer, whichever panel it shows.
    CloseDrawer,
    OpenLifecycle {
        task_id: String,
        /// Current lifecycle at emit time; the panel re-loads the task's
        /// live lifecycle when it opens, so this is informational only.
        #[allow(dead_code)]
        lifecycle: String,
    },
    /// A generator refresh finished and rewrote its managed subtree. The
    /// edit panel reloads from it, so its refresh status and error do not go
    /// stale when the refresh was driven from anywhere but that panel.
    GeneratorRefreshed {
        node_id: uuid::Uuid,
    },
    /// The tree selection changed (`None`: nothing selected). The right
    /// drawer follows it, whichever panel it is showing.
    SelectionChanged {
        task_id: Option<String>,
    },
    /// F / Action chip — open the Action panel for a node.
    OpenActionPanel {
        task_id: String,
    },
    /// A — open the node's most recent chat session, or start one.
    LaunchOrFocusAgent {
        task_id: String,
    },
    /// T — focus a shell, or open a new one when `shell_id` is `None`.
    OpenShell {
        task_id: String,
        shell_id: Option<String>,
    },
    /// C — open the node's resolved Files directory in a code editor.
    OpenCodeEditor {
        task_id: String,
        editor_id: String,
    },
    /// Right-click menu's "Open decisions (n)", or the attention badge on a
    /// row. The host decides what "open" means (the unified view's
    /// decisions panel); the existing Tasks view may ignore it.
    OpenDecisions {
        task_id: String,
    },
    /// Right-click menu's "Settings" entry: the unified view's settings
    /// panel (`PanelKind::Settings`, `TaskEditView` hosted). The existing
    /// Tasks view treats this the same as `OpenTaskEdit`, since its own
    /// edit drawer already hosts `TaskEditView`.
    OpenSettings {
        task_id: String,
    },
    /// Ctrl+E on a tree row: open its Details panel as a Ctrl+click would
    /// (`doc/ui/unified-view.md` "Keys"). The existing Tasks view treats
    /// this the same as `OpenTaskEdit`, since it has no columns.
    OpenTaskEditCtrl {
        task_id: String,
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
    /// Hosted as a column of a multi-column view (the workbench): the
    /// header takes the `column-focused` state while the tree has focus.
    marks_focused_column: bool,
    last_selected: Option<IndexPath>,
    pending_revert: Option<IndexPath>,
    action_sink: Rc<RefCell<Vec<RowAction>>>,
    compose_open: bool,
    compose_title_input: Entity<InputState>,
    credential_prompt_open: bool,
    credential_input: Entity<InputState>,
    pending_credential_request: Option<PendingCredentialRequest>,
    pending_credential_submit: bool,
    selection_before_compose: Option<String>,
    open_row_menu: Option<(RowMenuKind, String)>,
    row_menu: Option<Entity<PopupMenu>>,
    _row_menu_subscription: Option<Subscription>,
    sort_menu_open: bool,
    edit_open_for: Option<String>,
    /// Whether the shell's right drawer is showing a panel (see `set_drawer_open`).
    drawer_open: bool,
    /// Last selection sent as `TaskListEvent::SelectionChanged`.
    published_selection: Option<String>,
    /// Node created for inline edit that is not yet committed with Enter.
    draft: Option<edit::DraftRow>,
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
    /// Set when a row click focuses the nested list widget, so Enter would otherwise
    /// be swallowed as a list Confirm instead of reaching the task-list create action.
    pending_refocus_list: bool,
    ticket_import_generation: u64,
    pending_ticket_import: Option<PendingTicketImport>,
    status_line: String,
    config_dir: PathBuf,
    fleet: Arc<FleetStore>,
    active_list_id: Option<uuid::Uuid>,
    outline_lists: Vec<tod_store::outline::types::OutlineList>,
    app_nav: AppNavMenu,
    /// Generator node id whose sort/filter popover is open, if any.
    generator_filter_open: Option<String>,
    generator_filter_input: Entity<InputState>,
    /// Managed node id copied via Ctrl+C, pending a Ctrl+V paste-out.
    copied_managed_node_id: Option<uuid::Uuid>,
    /// Copied-out (linked) node ids that received a field update from the
    /// most recent refresh — session-only, cleared on restart or once the
    /// node is selected.
    recently_updated_copy_ids: std::collections::HashSet<String>,
    _list_subscription: Subscription,
    /// Row chips push their action onto `action_sink` from the list's own
    /// context, which notifies the list and not this view. Observing the list
    /// is what makes the next render — and so the drain — happen.
    _list_observation: Subscription,
    _compose_subscription: Subscription,
    _credential_subscription: Subscription,
    _inline_edit_subscription: Subscription,
    /// Rows marked for a multi-node action (Space / Ctrl+click): today,
    /// "Check incoming changes". Session-only.
    marked: std::collections::HashSet<String>,
    /// The shared incoming-changes check (`bind_incoming_check`).
    incoming_check: Option<Entity<IncomingCheck>>,
    _incoming_check_subscription: Option<Subscription>,
    /// Fed by the host through `set_attention` — what each node is waiting
    /// on the user for, and since when. Not persisted; re-supplied on every
    /// host-side change.
    attention: std::collections::HashMap<String, Attention>,
    /// `set_attention` changed `all_tasks` and needs a rebuild; applied on
    /// the next render, which is when a `Window` is available (mirrors
    /// `pending_live_refresh`).
    pending_attention_apply: bool,
    /// `set_status_overrides` changed `all_tasks` and needs a rebuild;
    /// applied the same way `pending_attention_apply` is.
    pending_status_override_apply: bool,
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

        let generator_filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter title or tags…"));

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

        let _list_observation = cx.observe(&list_state, |_, _, cx| cx.notify());

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
                // A click lands where it was aimed: only arrow keys wrap, so
                // only `Select` is checked for a wrap (a click from the first
                // row to the last used to be undone as one).
                this.last_selected = Some(*ix);
                if this.pending_revert.is_none() {
                    this.sync_selected_id(cx);
                }
                // A click confirms via the nested list widget and leaves it focused;
                // reclaim focus for the task-list surface so Enter still edits the node
                // instead of being swallowed as another list Confirm.
                this.pending_refocus_list = true;
                cx.notify();
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
            marks_focused_column: false,
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
            drawer_open: false,
            published_selection: initial_selection.1.clone(),
            draft: None,
            edit_original_title: None,
            inline_edit_input,
            pending_inline_commit: false,
            inline_enter_generation: 0,
            pending_abandon_edit: false,
            pending_compose_submit: false,
            pending_live_refresh: false,
            pending_new_list: false,
            pending_create_below: false,
            pending_refocus_list: false,
            ticket_import_generation: 0,
            pending_ticket_import: None,
            status_line: String::new(),
            config_dir,
            fleet: fleet.clone(),
            active_list_id,
            outline_lists,
            app_nav: AppNavMenu::default(),
            generator_filter_open: None,
            generator_filter_input,
            copied_managed_node_id: None,
            recently_updated_copy_ids: std::collections::HashSet::new(),
            _list_subscription,
            _list_observation,
            _compose_subscription,
            _credential_subscription,
            _inline_edit_subscription,
            marked: std::collections::HashSet::new(),
            incoming_check: None,
            _incoming_check_subscription: None,
            attention: std::collections::HashMap::new(),
            pending_attention_apply: false,
            pending_status_override_apply: false,
        };

        cx.defer_in(window, move |this, window, cx| {
            this.list_state.update(cx, |state, cx| {
                state.set_selected_index(this.last_selected, window, cx);
                if this.last_selected.is_some() {
                    state.scroll_to_selected_item(window, cx);
                }
                state.focus(window, cx);
            });
            this.focus_handle.focus(window, cx);
        });

        // Only this process writes the store, and every commit broadcasts a
        // change, so the view never reloads on a timer -- it reloads when (and
        // only when) something actually changed. The reload itself runs on the
        // background executor and its result is diffed against what is already
        // in memory, so an unrelated commit costs nothing and a changed row
        // patches just that row (see `apply_live_snapshot`).
        let poll_entity = cx.weak_entity();
        let fleet_for_poll = fleet.clone();
        cx.spawn(async move |_, cx| {
            let mut fleet_rx = fleet_for_poll.subscribe_changes();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                let mut changed = false;
                while fleet_rx.try_recv().is_ok() {
                    changed = true;
                }
                if !changed {
                    continue;
                }
                let Ok(list_id) = poll_entity.update(cx, |this, _| this.active_list_id) else {
                    break;
                };
                let fleet = fleet_for_poll.clone();
                let snapshot = cx
                    .background_spawn(async move {
                        LiveSnapshot {
                            lists: fleet.list_outline_lists().unwrap_or_default(),
                            tasks: load_tasks_from_store(&fleet, list_id),
                        }
                    })
                    .await;
                let Ok(()) = poll_entity.update(cx, |this, cx| {
                    this.apply_live_snapshot(list_id, snapshot, cx);
                }) else {
                    break;
                };
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

        let selection_moved_from = self.last_selected;
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
            state
                .delegate_mut()
                .set_recently_updated(self.recently_updated_copy_ids.clone());
            state.delegate_mut().set_marked(self.marked.clone());
            state.set_selected_index(selected_ix, window, cx);
            // Only chase the selection when it actually moved. A rebuild the
            // user did not ask for (a background change to the tree) must not
            // yank the viewport back from wherever they scrolled to.
            if selected_ix.is_some() && selected_ix != selection_moved_from {
                state.scroll_to_selected_item(window, cx);
            }
            cx.notify();
        });

        self.last_selected = selected_ix;
        self.pending_revert = None;
        self.persist_working_set();
        self.publish_selection(cx);
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
        self.recently_updated_copy_ids.remove(&new_id);
        self.working_set.selected_id = Some(new_id);
        self.persist_working_set();
        self.publish_selection(cx);
        cx.notify();
    }

    /// Tell the shell the selection moved so the right drawer follows it.
    /// Every path that changes `working_set.selected_id` calls this, and
    /// `render` does too, so a path that forgets still can't strand the drawer
    /// on a node that is no longer selected.
    fn publish_selection(&mut self, cx: &mut Context<Self>) {
        if self.published_selection == self.working_set.selected_id {
            return;
        }
        // A draft row has no node yet; the drawer stays on the last real one.
        if let Some(id) = self.working_set.selected_id.as_deref() {
            if self.is_draft_id(id) {
                return;
            }
        }
        self.published_selection = self.working_set.selected_id.clone();
        cx.emit(TaskListEvent::SelectionChanged {
            task_id: self.published_selection.clone(),
        });
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
            RowAction::ActionsControl { task_id } => {
                self.select_task_by_id(&task_id, window, cx);
                self.bump_interaction(&task_id, window, cx);
                // The Action chip opens the Action panel (same as F).
                self.handle_action_panel(&task_id, window, cx);
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
            RowAction::ToggleMark { task_id } => {
                self.toggle_mark(&task_id, cx);
            }
            RowAction::OpenObligations { task_id } => {
                self.dismiss_compose_for_row_action(window, cx);
                self.select_task_by_id(&task_id, window, cx);
                self.open_obligations_panel(&task_id, window, cx);
            }
            RowAction::DropObligation {
                task_id,
                obligation_id,
            } => {
                let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
                    return;
                };
                let _ = self.fleet.enqueue_outline(OutlineMutation::MoveObligation {
                    obligation_id,
                    target_node_id: node_id,
                });
                let _ = self.fleet.writer().flush();
                self.live_refresh(window, cx);
            }
            RowAction::RefreshGenerator { task_id } => {
                self.refresh_generator_for(&task_id, window, cx);
            }
            RowAction::OpenExternal { task_id } => {
                self.open_external_for(&task_id, cx);
            }
            RowAction::CycleGeneratorSort { task_id } => {
                self.cycle_generator_sort(&task_id, window, cx);
            }
            RowAction::ToggleGeneratorFilter { task_id } => {
                self.toggle_generator_filter(&task_id, window, cx);
            }
            RowAction::OpenContextMenu { task_id } => {
                self.open_context_menu(&task_id, window, cx);
            }
            RowAction::OpenDecisions { task_id } => {
                self.select_task_by_id(&task_id, window, cx);
                cx.emit(TaskListEvent::OpenDecisions {
                    task_id: task_id.clone(),
                });
                let presented = tod_journey::Presented {
                    actions: Vec::new(),
                    focused: Some("Open decisions".to_string()),
                    notices: Vec::new(),
                };
                if let Ok(node_id) = uuid::Uuid::parse_str(&task_id) {
                    crate::ui::journey::record_action(
                        cx,
                        tod_store::conversation::Focus::Node(node_id),
                        "Open decisions",
                        Source::Click,
                        "task_list_attention_badge",
                        presented,
                    );
                }
            }
            RowAction::AcceptTicket { task_id } => {
                let ready = self
                    .all_tasks
                    .iter()
                    .find(|t| t.id == task_id)
                    .is_some_and(|t| t.accept_ready);
                if ready {
                    self.accept_ticket(&task_id, Source::Click, window, cx);
                }
            }
        }
    }

    fn cycle_generator_sort(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(generator_id) = self.generator_ancestor_id(task_id) else {
            return;
        };
        let entry = self
            .working_set
            .generator_sorts
            .entry(generator_id)
            .or_default();
        if entry.sort_key == tod_core::task::model::SortKey::TicketId {
            entry.sort_key = tod_core::task::model::SortKey::TreeOrder;
            entry.sort_direction = WorkingSet::initial_direction_for_key(entry.sort_key);
        } else {
            let next = entry.sort_key.cycle();
            entry.sort_key = next;
            entry.sort_direction = WorkingSet::initial_direction_for_key(next);
        }
        self.persist_working_set();
        self.rebuild_visible_list(window, cx);
    }

    fn toggle_generator_filter(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(generator_id) = self.generator_ancestor_id(task_id) else {
            return;
        };
        let was_open = self.generator_filter_open.as_deref() == Some(generator_id.as_str());
        // One popup at a time.
        self.close_sort_menu(cx);
        self.close_row_menu(cx);
        self.generator_filter_open = None;
        if was_open {
            if !self.is_editing() {
                self.focus_handle.focus(window, cx);
            }
        } else {
            let current = self
                .working_set
                .generator_sorts
                .get(&generator_id)
                .map(|g| g.filter_query.clone())
                .unwrap_or_default();
            self.generator_filter_input.update(cx, |input, cx| {
                input.set_value(current, window, cx);
                input.focus(window, cx);
            });
            self.generator_filter_open = Some(generator_id);
        }
        self.sync_delegate_generator_filter(cx);
        cx.notify();
    }

    /// Mirror the open filter popup into the delegate, which draws it under the
    /// generator row's own chip.
    fn sync_delegate_generator_filter(&mut self, cx: &mut Context<Self>) {
        let open = self.generator_filter_open.clone();
        let input = self.generator_filter_input.clone();
        let view = cx.weak_entity();
        self.list_state.update(cx, |state, _| {
            state.delegate_mut().set_generator_filter(open, input, view);
        });
    }

    fn close_generator_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.generator_filter_open.take().is_none() {
            return;
        }
        self.sync_delegate_generator_filter(cx);
        if !self.is_editing() {
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    fn clear_generator_filter(
        &mut self,
        generator_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.generator_filter_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.working_set
            .generator_sorts
            .entry(generator_id.to_string())
            .or_default()
            .filter_query = String::new();
        self.persist_working_set();
        self.rebuild_visible_list(window, cx);
    }

    fn sync_generator_filter_from_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(generator_id) = self.generator_filter_open.clone() else {
            return;
        };
        let query = self.generator_filter_input.read(cx).text().to_string();
        let changed = self
            .working_set
            .generator_sorts
            .get(&generator_id)
            .map(|g| g.filter_query != query)
            .unwrap_or(!query.is_empty());
        if changed {
            self.working_set
                .generator_sorts
                .entry(generator_id)
                .or_default()
                .filter_query = query;
            self.persist_working_set();
            cx.defer_in(window, |this, window, cx| {
                this.rebuild_visible_list(window, cx);
            });
        }
    }

    fn open_external_for(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) else {
            return;
        };
        if task.source_type.as_deref() != Some("linear") {
            return;
        }
        let Some(external_id) = &task.external_id else {
            return;
        };

        let metadata = uuid::Uuid::parse_str(task_id).ok().and_then(|node_id| {
            self.fleet
                .get_extra_content(node_id, tod_store::outline::types::EXTRA_CONTENT_METADATA)
                .ok()
                .flatten()
                .and_then(|json_str| serde_json::from_str::<serde_json::Value>(&json_str).ok())
        });
        if let Some(url) =
            tod_integration::linear_issue_url(metadata.as_ref(), &self.config_dir, external_id)
        {
            cx.open_url(&url);
        }
    }

    /// Walk up from `task_id` to the nearest ancestor (or itself) that owns
    /// the Generator capability, so the refresh shortcut/chip works from
    /// anywhere in a generator's managed subtree.
    fn generator_ancestor_id(&self, task_id: &str) -> Option<String> {
        let mut current = self.all_tasks.iter().find(|t| t.id == task_id)?;
        loop {
            if current.managed_count.is_some() {
                return Some(current.id.clone());
            }
            let parent_id = current.parent_id.as_deref()?;
            current = self.all_tasks.iter().find(|t| t.id == parent_id)?;
        }
    }

    fn refresh_generator_for(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(generator_id) = self.generator_ancestor_id(task_id) else {
            return;
        };
        let Ok(node_id) = uuid::Uuid::parse_str(&generator_id) else {
            return;
        };
        self.start_generator_refresh(node_id, window, cx);
    }

    /// Refresh one generator node, whoever asked for it: the tree's own
    /// action, the credential prompt resuming a refresh it blocked, or the
    /// edit panel routing through the shell.
    pub(super) fn start_generator_refresh(
        &mut self,
        node_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The fetch reaches the network, so it runs on the background executor
        // and the user keeps working; the row shows "refreshing…" meanwhile,
        // off the generator's own in-progress status.
        let fleet = self.fleet.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { tod_core::generator::refresh_generator(&fleet, node_id) },
                )
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(updated_ids) => {
                        this.recently_updated_copy_ids
                            .extend(updated_ids.into_iter().map(|id| id.to_string()));
                        cx.emit(TaskListEvent::GeneratorRefreshed { node_id });
                    }
                    // Nothing was fetched and there is a key to collect, so
                    // ask for it and pick the refresh back up, rather than
                    // reporting a failure the user has no way to act on.
                    Err(err) if err.needs_linear_api_key() => {
                        this.open_linear_credential_prompt(
                            PendingCredentialRequest::GeneratorRefresh { node_id },
                            window,
                            cx,
                        );
                    }
                    Err(err) => this.show_error(format!("Refresh failed: {err}"), window, cx),
                }
                this.live_refresh(window, cx);
            });
        })
        .detach();
        self.live_refresh(window, cx);
    }

    /// Open the Linear API key prompt on behalf of another view (the edit
    /// panel, routed through the shell), resuming that generator's refresh
    /// once the key is saved.
    pub fn prompt_linear_credentials_for_generator(
        &mut self,
        node_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_linear_credential_prompt(
            PendingCredentialRequest::GeneratorRefresh { node_id },
            window,
            cx,
        );
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

    /// Create a node titled `title` at `position` relative to the selection.
    fn create_tree_node(
        &mut self,
        position: CreatePosition,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let draft = self.draft_placement(position, window, cx)?;
        self.create_node_at(&draft, title, window, cx)
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
            self.show_error("Failed to create list", window, cx);
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
            self.show_error("List created but not found", window, cx);
            return;
        };
        self.switch_active_list(new_id, window, cx);
        self.set_status_line(format!("Created {title}"), cx);
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
            self.show_error("No lists yet — press Enter to create one", window, cx);
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
        self.set_status_line(format!("Switched to {title}"), cx);
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

    /// A — open the node's most recent chat session, or start one.
    fn handle_agents_control(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id).cloned() else {
            return;
        };
        if !task.has_agent {
            self.show_error(
                "Enable the Agent capability on this node (or an ancestor) first.",
                window,
                cx,
            );
            return;
        }
        self.close_chrome_overlays(cx);
        cx.emit(TaskListEvent::LaunchOrFocusAgent {
            task_id: task_id.to_string(),
        });
        self.set_status_line("Opening agent chat…", cx);
    }

    /// T — focus the node's shell, pick among several, or open a new one.
    fn handle_shells_control(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id).cloned() else {
            return;
        };
        if !task.has_files {
            self.show_error(
                "Enable the Files capability on this node (or an ancestor) first.",
                window,
                cx,
            );
            return;
        }
        match task.shells.len() {
            0 => {
                cx.emit(TaskListEvent::OpenShell {
                    task_id: task_id.to_string(),
                    shell_id: None,
                });
            }
            1 => {
                cx.emit(TaskListEvent::OpenShell {
                    task_id: task_id.to_string(),
                    shell_id: Some(task.shells[0].id.clone()),
                });
            }
            _ => {
                if self.open_row_menu.as_ref() == Some(&(RowMenuKind::Shells, task_id.to_string()))
                {
                    let shell_id = task.shells[0].id.clone();
                    self.close_row_menu(cx);
                    cx.emit(TaskListEvent::OpenShell {
                        task_id: task_id.to_string(),
                        shell_id: Some(shell_id),
                    });
                } else {
                    self.toggle_shells_menu(task_id, window, cx);
                    self.set_status_line("Pick a shell (T again selects first)", cx);
                }
            }
        }
    }

    /// C — open the node's resolved Files directory in the first code editor.
    fn handle_open_code(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id).cloned() else {
            return;
        };
        if !task.has_files {
            self.show_error(
                "Enable the Files capability on this node (or an ancestor) first.",
                window,
                cx,
            );
            return;
        }
        let Some(editor) = code_editors().first() else {
            return;
        };
        self.close_chrome_overlays(cx);
        cx.emit(TaskListEvent::OpenCodeEditor {
            task_id: task_id.to_string(),
            editor_id: editor.id().to_string(),
        });
        self.set_status_line(format!("Opening {}…", editor.label()), cx);
    }

    /// F / Action chip — open the Action panel.
    fn handle_action_panel(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id).cloned() else {
            return;
        };
        if !task.has_actions {
            self.show_error(
                "Enable the Agent or Files capability on this node (or an ancestor) first.",
                window,
                cx,
            );
            return;
        }
        self.close_chrome_overlays(cx);
        cx.emit(TaskListEvent::OpenActionPanel {
            task_id: task_id.to_string(),
        });
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
        self.handle_lifecycle_control(task_id, &lifecycle, window, cx);
    }

    /// Proceed always opens the lifecycle transition panel first, whatever
    /// the current phase — including phases with an interview. The gate
    /// check (state agent) gets first crack at advancing the node on its
    /// own; the interview is a fallback the panel offers only if genuine
    /// questions remain, not a step every node is forced through. See
    /// `open_interview_for_task` for the on-demand path the panel uses.
    fn handle_lifecycle_control(
        &mut self,
        task_id: &str,
        lifecycle: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.emit(TaskListEvent::OpenLifecycle {
            task_id: task_id.to_string(),
            lifecycle: lifecycle.to_string(),
        });
        self.set_status_line(format!("Lifecycle panel: {lifecycle}"), cx);
    }

    /// Validate and open the implementation/design/requirements interview
    /// for `task_id` on demand — used by the lifecycle panel's "Open
    /// interview" affordance rather than being forced automatically.
    pub fn open_interview_for_task(
        &mut self,
        task_id: &str,
        lifecycle: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The shell opens `proposed` / `design` nodes in the conversation view
        // before it gets here; this path is the interview's (`planning`).
        let label = tod_core::process::spec_view_label(lifecycle).unwrap_or("Interview");
        if interview_phase_for_lifecycle(lifecycle).is_none() {
            self.show_error(
                format!("{label} unavailable for this lifecycle state."),
                window,
                cx,
            );
            return;
        }
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id).cloned() else {
            return;
        };
        if !task.is_work_node {
            self.show_error(
                format!("{label} unavailable — task is not a work node."),
                window,
                cx,
            );
            return;
        }
        let Ok(node_id) = uuid::Uuid::parse_str(&task.id) else {
            self.show_error(
                format!("{label} unavailable — invalid task id."),
                window,
                cx,
            );
            return;
        };
        if let Ok(Some(task_row)) = self.fleet.get_task(task_id) {
            if task_row.repo.as_ref().is_none_or(|r| r.trim().is_empty()) {
                self.show_error(
                    format!(
                        "Set repository on task before opening {}.",
                        label.to_lowercase()
                    ),
                    window,
                    cx,
                );
                return;
            }
            let repo = task_row.repo.as_deref().unwrap_or("");
            let branch = task_row.branch.as_deref().unwrap_or("");
            // A repository inside a dev container or sandbox can't be checked
            // from here.
            let in_container = self
                .fleet
                .resolve_files_for_node(task_id)
                .ok()
                .flatten()
                .is_some_and(|files| files.repo_is_remote());
            if !in_container
                && let Err(err) =
                    validate_interview_workspace(PathBuf::from(repo).as_path(), branch)
            {
                self.show_error(format!("{label} workspace: {err:#}"), window, cx);
                return;
            }
        }
        cx.emit(TaskListEvent::OpenInterview {
            task_id: task_id.to_string(),
            node_id,
            lifecycle: lifecycle.to_string(),
            title: task.title.clone(),
        });
        self.set_status_line(
            format!("Opening {} for {}", label.to_lowercase(), task.title),
            cx,
        );
    }

    /// Open the lifecycle transition panel for a task (bypasses interview
    /// routing), selecting it in the tree so the drawer shows the selection.
    pub fn open_lifecycle_panel(
        &mut self,
        task_id: &str,
        lifecycle: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_task_by_id(task_id, window, cx);
        cx.emit(TaskListEvent::OpenLifecycle {
            task_id: task_id.to_string(),
            lifecycle: lifecycle.to_string(),
        });
        self.set_status_line(format!("Lifecycle panel: {lifecycle}"), cx);
    }

    pub fn restore_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_list(window, cx);
    }

    pub(super) fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.list_state.update(cx, |state, cx| {
            state.focus(window, cx);
        });
        self.focus_handle.focus(window, cx);
    }

    /// Select `task_id` in the tree, if its row is visible (a row under a
    /// collapsed ancestor or filtered out by search is left unselected).
    pub fn reveal_node(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.select_task_by_id(task_id, window, cx);
    }

    fn select_task_by_id(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let visible = Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
        if let Some(row) = visible.iter().position(|t| t.id == task_id) {
            let ix = IndexPath::new(row);
            self.last_selected = Some(ix);
            self.recently_updated_copy_ids.remove(task_id);
            self.working_set.selected_id = Some(task_id.to_string());
            self.list_state.update(cx, |state, cx| {
                state.set_selected_index(Some(ix), window, cx);
                state.scroll_to_selected_item(window, cx);
            });
            self.persist_working_set();
            self.publish_selection(cx);
        }
    }

    fn emit_open_edit_for(&mut self, task_id: &str, cx: &mut Context<Self>) {
        if self.is_draft_id(task_id) {
            return;
        }
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id) else {
            return;
        };
        cx.emit(TaskListEvent::OpenTaskEdit {
            task_id: task_id.to_string(),
        });
        self.set_status_line(format!("Edit: {}", task.title), cx);
    }

    pub fn open_task_edit_panel(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_chrome_overlays(cx);
        self.emit_open_edit_for(task_id, cx);
        self.bump_interaction(task_id, window, cx);
        cx.notify();
    }

    pub fn open_obligations_panel(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id).cloned() else {
            return;
        };
        if !task.has_spec {
            self.show_error("Obligations require the Spec capability.", window, cx);
            return;
        }
        self.close_chrome_overlays(cx);
        cx.emit(TaskListEvent::OpenObligations {
            task_id: task_id.to_string(),
            title: task.title.clone(),
        });
        self.set_status_line(format!("Obligations: {}", task.title), cx);
        self.bump_interaction(task_id, window, cx);
        cx.notify();
    }

    /// Right-click menu's "Settings" entry.
    pub fn open_settings_panel(
        &mut self,
        task_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id).cloned() else {
            return;
        };
        self.close_chrome_overlays(cx);
        cx.emit(TaskListEvent::OpenSettings {
            task_id: task_id.to_string(),
        });
        self.set_status_line(format!("Settings: {}", task.title), cx);
        self.bump_interaction(task_id, window, cx);
        cx.notify();
    }

    pub fn open_plan_panel(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id).cloned() else {
            return;
        };
        self.close_chrome_overlays(cx);
        cx.emit(TaskListEvent::OpenPlan {
            task_id: task_id.to_string(),
            title: task.title.clone(),
        });
        self.set_status_line(format!("Plan steps: {}", task.title), cx);
        self.bump_interaction(task_id, window, cx);
        cx.notify();
    }

    /// Kept in sync by the shell, so Escape in the tree knows to close the drawer.
    pub fn set_drawer_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.drawer_open == open {
            return;
        }
        self.drawer_open = open;
        if !open {
            self.set_status_line("", cx);
        } else {
            cx.notify();
        }
    }

    pub fn request_live_refresh(&mut self, cx: &mut Context<Self>) {
        self.pending_live_refresh = true;
        cx.notify();
    }

    /// The selected node, when a saved (not draft) row is selected.
    /// [`Self::selected_node_id`] with the node's title.
    pub fn selected_node_with_title(&self) -> Option<(uuid::Uuid, String)> {
        let id = self.selected_node_id()?;
        let key = id.to_string();
        let title = self.all_tasks.iter().find(|t| t.id == key)?.title.clone();
        Some((id, title))
    }

    pub fn selected_node_id(&self) -> Option<uuid::Uuid> {
        let id = self.working_set.selected_id.as_deref()?;
        if self.is_draft_id(id) {
            return None;
        }
        uuid::Uuid::parse_str(id).ok()
    }

    /// Ctrl+J: the conversation about the selected node, or about the whole
    /// project when nothing is selected.
    fn on_open_agent_chat(
        &mut self,
        _: &OpenAgentChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = self
            .selected_node_id()
            .map_or(tod_store::conversation::Focus::Project, |id| {
                tod_store::conversation::Focus::Node(id)
            });
        cx.stop_propagation();
        window.dispatch_action(Box::new(OpenConversation::outline(focus)), cx);
    }

    /// Ctrl+Shift+R: report a problem against the selected node, or the
    /// whole project when nothing is selected.
    fn on_report_problem(
        &mut self,
        _: &ReportProblem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = self
            .selected_node_id()
            .map_or(tod_journey::JourneyKey::Project, tod_journey::JourneyKey::Node);
        cx.stop_propagation();
        window.dispatch_action(Box::new(OpenReportDialog { key, conversation: None }), cx);
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
            self.show_delete_error("invalid node id", window, cx);
            return;
        };
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::DeleteNode { node_id })
        {
            self.show_delete_error(err, window, cx);
            return;
        }
        self.finish_node_removal(task_id, window, cx);
    }

    fn show_delete_error(
        &mut self,
        err: impl std::fmt::Display,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_error(Self::format_delete_error(err), window, cx);
    }

    fn format_delete_error(err: impl std::fmt::Display) -> String {
        let detail = err.to_string();
        if detail.contains("associated agents") {
            return detail;
        }
        if detail.contains("referenced by other nodes") {
            return detail;
        }
        format!("Delete failed: {detail}")
    }

    fn finish_node_removal(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.fleet.writer().flush() {
            self.show_delete_error(err, window, cx);
            return;
        }
        let _ = self.fleet.reload_if_stale();

        let visible_before =
            Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
        let selected = self.working_set.selected_id.clone();
        if self.edit_open_for.as_deref() == Some(task_id) {
            self.edit_open_for = None;
        }
        self.reload_all_tasks();
        let visible_after =
            Self::visible_tasks(&self.all_tasks, &self.search_query, &self.working_set);
        self.working_set.selected_id = model::selection_after_delete(
            &visible_before,
            &visible_after,
            selected.as_deref(),
            task_id,
        );
        self.rebuild_visible_list(window, cx);
    }

    /// Load the active list's rows, with the draft row spliced in where it will be created.
    ///
    /// `load_tasks_from_store` reads fresh rows straight from the outline
    /// store, which knows nothing about `set_attention`'s "needs you" data —
    /// every freshly loaded row starts at zero. Re-apply the last attention
    /// map we were given so a reload (a live-refresh triggered by any store
    /// change, not just an attention change) never wipes the "needs you"
    /// badge out from under a node that is still waiting: it would otherwise
    /// flicker off here and only come back once the next attention poll
    /// lands.
    fn reload_all_tasks(&mut self) {
        let mut tasks = load_tasks_from_store(&self.fleet, self.active_list_id);
        if let Some(draft) = &self.draft {
            if Some(draft.list_id) == self.active_list_id {
                edit::insert_draft_row(&mut tasks, draft);
            }
        }
        Self::apply_attention_map(&mut tasks, &self.attention);
        self.all_tasks = tasks;
    }

    /// Stamp `needs_you_count` / `waiting_since` from `map` onto `tasks`,
    /// leaving nodes absent from `map` at zero. The shared core of
    /// `set_attention` and `reload_all_tasks`'s re-application of it.
    fn apply_attention_map(
        tasks: &mut [TaskItem],
        map: &std::collections::HashMap<String, Attention>,
    ) {
        for task in tasks.iter_mut() {
            let (count, waiting_since) = map
                .get(&task.id)
                .map(|a| (a.count, Some(a.waiting_since)))
                .unwrap_or((0, None));
            task.needs_you_count = count;
            task.waiting_since = waiting_since;
        }
    }

    /// Apply a reload that the change watcher read off the UI thread.
    ///
    /// The common case -- a commit that changed some rows' fields but not the
    /// shape of the tree -- replaces just those rows and leaves selection,
    /// scroll position and the persisted working set untouched. A commit that
    /// changed nothing this view shows costs a comparison and no redraw at
    /// all. Anything that adds, removes or moves a visible row falls back to
    /// `live_refresh`, which fixes up selection and needs a `Window`, so it
    /// runs on the next render.
    fn apply_live_snapshot(
        &mut self,
        loaded_for: Option<uuid::Uuid>,
        snapshot: LiveSnapshot,
        cx: &mut Context<Self>,
    ) {
        // A draft row lives only in memory, and the active list may have moved
        // on while the read was in flight; neither is this path's to reconcile.
        if self.draft.is_some()
            || self.active_list_id != loaded_for
            || snapshot.lists != self.outline_lists
        {
            self.request_live_refresh(cx);
            return;
        }
        let mut tasks = snapshot.tasks;
        Self::apply_attention_map(&mut tasks, &self.attention);
        if tasks == self.all_tasks {
            return;
        }
        let visible = Self::visible_tasks(&tasks, &self.search_query, &self.working_set);
        if !same_visible_rows(self.list_state.read(cx).delegate().items(), &visible) {
            self.request_live_refresh(cx);
            return;
        }
        self.all_tasks = tasks;
        self.list_state.update(cx, |state, cx| {
            state.delegate_mut().set_items(visible);
            cx.notify();
        });
        cx.notify();
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
        self.reload_all_tasks();
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
        self.set_status_line(message, cx);
    }

    fn set_status_line(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        let message = message.into();
        if self.status_line == message {
            return;
        }
        crate::ui::status::post(cx, crate::ui::status::StatusSource::Tasks, message.clone());
        self.status_line = message;
        cx.notify();
    }

    pub fn show_error(
        &mut self,
        message: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The toast goes away; the status bar keeps the message readable
        // (and copyable) until the next one replaces it.
        let message = message.into();
        crate::ui::toast::error_toast(window, cx, message.clone());
        self.set_status_line(message, cx);
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
            self.show_error(format!("Failed to move item: {err}"), window, cx);
            return;
        }
        if let Err(err) = self.fleet.writer().flush() {
            self.show_error(format!("Failed to move item: {err}"), window, cx);
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
        if self.drawer_open {
            cx.emit(TaskListEvent::CloseDrawer);
        } else if self.is_editing() {
            self.abandon_inline_edit(window, cx, true);
        } else if self.compose_open {
            self.close_compose(window, cx);
        } else if self.credential_prompt_open {
            self.cancel_credential_prompt(window, cx);
        } else if self.generator_filter_open.is_some() {
            self.close_generator_filter(window, cx);
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
        self.select_task_by_id(&task_id, window, cx);
        self.bump_interaction(&task_id, window, cx);
        self.handle_agents_control(&task_id, window, cx);
    }

    fn on_open_code(&mut self, _: &TaskListOpenCode, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self
            .working_set
            .selected_id
            .clone()
            .or_else(|| self.selected_task(cx).map(|t| t.id))
        else {
            return;
        };
        self.select_task_by_id(&task_id, window, cx);
        self.bump_interaction(&task_id, window, cx);
        self.handle_open_code(&task_id, window, cx);
    }

    fn on_open_action_panel(
        &mut self,
        _: &TaskListOpenActionPanel,
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
        self.select_task_by_id(&task_id, window, cx);
        self.bump_interaction(&task_id, window, cx);
        self.handle_action_panel(&task_id, window, cx);
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
            self.show_error(
                "Enable the Lifecycle capability on this task to start an interview",
                window,
                cx,
            );
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

    fn on_refresh_generator(
        &mut self,
        _: &TaskListRefreshGenerator,
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
        self.refresh_generator_for(&task_id, window, cx);
    }

    fn on_open_external(
        &mut self,
        _: &TaskListOpenExternal,
        _window: &mut Window,
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
        self.open_external_for(&task_id, cx);
    }

    fn on_copy(&mut self, _: &TaskListCopy, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            return;
        }
        let Some(task) = self.selected_task(cx) else {
            return;
        };
        if !task.managed {
            return;
        }
        let Ok(node_id) = uuid::Uuid::parse_str(&task.id) else {
            return;
        };
        self.copied_managed_node_id = Some(node_id);
        self.set_status_line(format!("Copied {}", task.title), cx);
        let _ = window;
    }

    fn on_paste(&mut self, _: &TaskListPaste, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            return;
        }
        let Some(source_node_id) = self.copied_managed_node_id else {
            return;
        };
        let Some(task) = self.selected_task(cx) else {
            crate::ui::toast::error_toast(window, cx, "Select a location to paste");
            return;
        };
        let Some(list_id) = self.active_list_id else {
            return;
        };
        if self.generator_ancestor_id(&task.id).is_some() {
            self.show_error("Cannot paste inside a generator subtree", window, cx);
            return;
        }
        let parent_id = uuid::Uuid::parse_str(&task.id).ok();
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::PasteManagedNodeCopy {
                source_node_id,
                list_id,
                parent_id,
                ordinal: 0,
            })
        {
            self.show_error(format!("Paste failed: {err}"), window, cx);
            return;
        }
        if let Err(err) = self.fleet.writer().flush() {
            self.show_error(format!("Paste failed: {err}"), window, cx);
            return;
        }
        self.live_refresh(window, cx);
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
        if self.is_editing() && !self.is_draft_edit() {
            cx.propagate();
            return;
        }
        if self.is_draft_edit() {
            self.reparent_draft(1, window, cx);
            return;
        }
        self.reparent_selected(1, window, cx);
    }

    fn on_outdent(&mut self, _: &TaskListOutdent, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() && !self.is_draft_edit() {
            cx.propagate();
            return;
        }
        if self.is_draft_edit() {
            self.reparent_draft(-1, window, cx);
            return;
        }
        self.reparent_selected(-1, window, cx);
    }

    fn on_focus_drawer(&mut self, _: &PaneFocusRight, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TaskListEvent::FocusDrawer);
        cx.stop_propagation();
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

    pub fn delete_selected_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_selected_node(window, cx);
    }

    fn delete_selected_node(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.selected_task(cx) else {
            crate::ui::toast::error_toast(window, cx, "Select a task to delete");
            return;
        };
        if task.managed {
            self.show_error(
                "Cannot delete a managed node — it is owned by its generator",
                window,
                cx,
            );
            return;
        }
        // If deleting a generator node with children, show confirmation.
        // A generator node has managed_count.is_some().
        let is_generator = task.managed_count.is_some();
        if is_generator && task.has_children {
            let task_id = task.id.clone();
            let view = cx.entity().downgrade();
            crate::ui::toast::confirm_toast(
                window,
                cx,
                "Delete Generator?",
                "Deleting this generator node will permanently delete all managed child nodes under it.",
                move |window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.remove_outline_node(&task_id, window, cx);
                    });
                },
                |_window, _cx| {},
            );
        } else {
            self.remove_outline_node(&task.id, window, cx);
        }
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
                let depth = rows[ix].depth;
                // Walk backward past any descendants of an earlier sibling to
                // find the previous sibling at this same depth, if any.
                let prev_sibling = rows[..ix]
                    .iter()
                    .rev()
                    .take_while(|r| r.depth >= depth)
                    .find(|r| r.depth == depth);
                let Some(prev) = prev_sibling else {
                    return;
                };
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
                let parent_id = entry.parent_id.unwrap();
                let Some(parent_entry) = outline.get_entry(parent_id).ok().flatten() else {
                    return;
                };
                let grandparent = parent_entry.parent_id;
                // Place immediately after the former parent among its own
                // siblings (children of `grandparent`); the mutation handler
                // shifts any later siblings to make room.
                let ord = parent_entry.ordinal + 1;
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

    fn on_open_edit_panel_ctrl(
        &mut self,
        _: &TaskListOpenEditPanelCtrl,
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
        if self.is_draft_id(&task_id) {
            return;
        }
        let Some(title) = self
            .all_tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.title.clone())
        else {
            return;
        };
        self.close_chrome_overlays(cx);
        cx.emit(TaskListEvent::OpenTaskEditCtrl {
            task_id: task_id.clone(),
        });
        self.set_status_line(format!("Edit: {title}"), cx);
        self.bump_interaction(&task_id, window, cx);
        cx.notify();
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

    /// Share the incoming-changes check with the lifecycle panel, so a
    /// check started in either shows in both.
    pub fn bind_incoming_check(&mut self, check: Entity<IncomingCheck>, cx: &mut Context<Self>) {
        self._incoming_check_subscription = Some(cx.observe(&check, |_, _, cx| cx.notify()));
        self.incoming_check = Some(check);
        cx.notify();
    }

    /// Host this view as a column; see `marks_focused_column`.
    pub fn set_marks_focused_column(&mut self, marks: bool) {
        self.marks_focused_column = marks;
    }

    /// The host (e.g. W6's attention feed) reports what each node is
    /// waiting on the user for, keyed by node id. Nodes not present in
    /// `map` are cleared back to zero. Rows with `count > 0` show a badge
    /// (clicking it emits `OpenDecisions`), the right-click menu's "Open
    /// decisions" entry uses the count, and the "Needs you" filter and the
    /// "Waiting longest" sort both read it.
    pub fn set_attention(
        &mut self,
        map: std::collections::HashMap<String, Attention>,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        for task in self.all_tasks.iter_mut() {
            let (count, waiting_since) = map
                .get(&task.id)
                .map(|a| (a.count, Some(a.waiting_since)))
                .unwrap_or((0, None));
            if task.needs_you_count != count || task.waiting_since != waiting_since {
                changed = true;
            }
        }
        self.attention = map;
        Self::apply_attention_map(&mut self.all_tasks, &self.attention);
        if changed {
            self.pending_attention_apply = true;
        }
        cx.notify();
    }

    /// The unified view (W11) reports each node's status label, keyed by
    /// node id, computed once from `AgentRuns` whenever it notifies —
    /// never read per row per frame, since building it touches
    /// `AgentRuns::runs_for_node` (a `fleet.read`). Nodes not present in
    /// `map` are cleared to `None`. The existing Tasks view never calls
    /// this, so `status_override` stays `None` there and no chip renders.
    pub fn set_status_overrides(
        &mut self,
        map: std::collections::HashMap<String, String>,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        for task in self.all_tasks.iter_mut() {
            let next = map.get(&task.id).cloned();
            if task.status_override != next {
                task.status_override = next;
                changed = true;
            }
        }
        if changed {
            self.pending_status_override_apply = true;
        }
        cx.notify();
    }

    /// Records toggling a "Needs you" / "Running" tree filter chip as a
    /// journey `UserAction`, on the project (the toggle isn't about one node).
    fn record_quick_filter_toggle(&self, chip: &str, on: bool, cx: &mut Context<Self>) {
        let presented = tod_journey::Presented {
            actions: vec![tod_journey::PresentedAction {
                id: chip.to_string(),
                label: chip.to_string(),
                primary: false,
                disabled: false,
            }],
            focused: Some(chip.to_string()),
            notices: Vec::new(),
        };
        crate::ui::journey::record_action(
            cx,
            tod_store::conversation::Focus::Project,
            if on {
                format!("{chip}-on")
            } else {
                format!("{chip}-off")
            },
            Source::Click,
            "task_list_quick_filters",
            presented,
        );
    }

    fn toggle_mark(&mut self, task_id: &str, cx: &mut Context<Self>) {
        if !self.marked.remove(task_id) {
            self.marked.insert(task_id.to_string());
        }
        self.push_marks(cx);
    }

    fn clear_marks(&mut self, cx: &mut Context<Self>) {
        self.marked.clear();
        self.push_marks(cx);
    }

    fn push_marks(&mut self, cx: &mut Context<Self>) {
        let marked = self.marked.clone();
        self.list_state.update(cx, |state, cx| {
            state.delegate_mut().set_marked(marked);
            cx.notify();
        });
        cx.notify();
    }

    fn on_toggle_mark(&mut self, _: &TaskListToggleMark, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self
            .working_set
            .selected_id
            .clone()
            .or_else(|| self.selected_task(cx).map(|t| t.id))
        else {
            return;
        };
        // On a ticket row whose generator has a quick-accept destination
        // configured, Space accepts it instead of toggling the batch mark.
        if let Some(task) = self.all_tasks.iter().find(|t| t.id == task_id)
            && task.managed
            && task.external_id.is_some()
        {
            if task.accept_ready {
                self.accept_ticket(&task_id, Source::Keyboard, window, cx);
            }
            return;
        }
        self.toggle_mark(&task_id, cx);
    }

    /// Quick-accept: copy a generator-managed ticket out to its generator's
    /// configured destination, enable the configured capabilities on the
    /// copy, and select it. A no-op (nothing enqueued) unless the row is
    /// still an accept-ready managed ticket — callers check `accept_ready`
    /// before calling this so the keystroke and chip are both silent
    /// no-ops otherwise, not an error toast.
    fn accept_ticket(
        &mut self,
        task_id: &str,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(source_node_id) = uuid::Uuid::parse_str(task_id) else {
            return;
        };
        // The ticket row offers exactly one action, so that is all that was presented.
        crate::ui::journey::record_action(
            cx,
            tod_store::conversation::Focus::Node(source_node_id),
            "accept-ticket",
            source,
            "task-list",
            tod_journey::Presented {
                actions: vec![tod_journey::PresentedAction {
                    id: "accept-ticket".into(),
                    label: "Accept".into(),
                    primary: true,
                    disabled: false,
                }],
                focused: Some("accept-ticket".into()),
                notices: Vec::new(),
            },
        );
        let new_node_id = uuid::Uuid::new_v4();
        if let Err(err) = self.fleet.enqueue_outline(OutlineMutation::AcceptGeneratedTicket {
            source_node_id,
            new_node_id,
        }) {
            self.show_error(format!("Accept failed: {err}"), window, cx);
            return;
        }
        if let Err(err) = self.fleet.writer().flush() {
            self.show_error(format!("Accept failed: {err}"), window, cx);
            return;
        }
        self.live_refresh(window, cx);
        self.select_created_task(&new_node_id.to_string(), window, cx);
    }

    /// The nodes "Check incoming changes" acts on: the marked rows, else
    /// the selected one.
    fn check_targets(&self, cx: &Context<Self>) -> Vec<uuid::Uuid> {
        if self.marked.is_empty() {
            return self
                .working_set
                .selected_id
                .clone()
                .or_else(|| self.selected_task(cx).map(|t| t.id))
                .and_then(|id| uuid::Uuid::parse_str(&id).ok())
                .into_iter()
                .collect();
        }
        // In tree order, so the summary reads top to bottom.
        self.all_tasks
            .iter()
            .filter(|t| self.marked.contains(&t.id))
            .filter_map(|t| uuid::Uuid::parse_str(&t.id).ok())
            .collect()
    }

    fn on_check_incoming(
        &mut self,
        _: &TaskListCheckIncoming,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.check_incoming(cx);
    }

    fn check_incoming(&mut self, cx: &mut Context<Self>) {
        let Some(check) = self.incoming_check.clone() else {
            return;
        };
        let nodes = self.check_targets(cx);
        if nodes.is_empty() || check.read(cx).is_running() {
            return;
        }
        check.update(cx, |check, cx| check.start(nodes, cx));
        self.clear_marks(cx);
    }

    fn on_open_plan(&mut self, _: &TaskListOpenPlan, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self
            .working_set
            .selected_id
            .clone()
            .or_else(|| self.selected_task(cx).map(|t| t.id))
        else {
            return;
        };
        self.open_plan_panel(&task_id, window, cx);
    }

    fn render_header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui::IntoElement as _;
        if self.marks_focused_column {
            self.render_column_header(window, cx).into_any_element()
        } else {
            self.render_toolbar(window, cx).into_any_element()
        }
    }

    /// The header as a column of a multi-column view: the toolbar's controls
    /// on one fixed-height `column-header` row, marked while the tree has
    /// focus. Only the search field keeps its (inline) shortcut badge; the
    /// others hang below their controls, which a fixed height has no room
    /// for. What does not fit the column's width is clipped at the right.
    fn render_column_header(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement {
        let focused = self.focus_handle.contains_focused(window, cx);
        let sort_label = format!(
            "Sort {} {}",
            self.working_set.sort_key.label(),
            self.working_set.sort_direction.arrow()
        );
        let mut search = Input::new(&self.search_input).cleanable(true).small().w_full();
        if let Some(pill) =
            render_shortcut_pill(window, &TaskListFocusSearch, TASK_LIST_CONTEXT, cx)
        {
            search = search.suffix(pill);
        }
        let list_count = self.outline_lists.len();

        let mut row = crate::ui::style::column_header(div(), focused)
            .overflow_hidden()
            .child(self.render_app_nav_without_badge(window, cx))
            .child(
                Button::new("prev-list")
                    .label("◀")
                    .small()
                    .disabled(list_count <= 1)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_prev_list(&TaskListPrevList, window, cx);
                    })),
            )
            .child(
                crate::ui::style::text(div())
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .child(self.active_list_title()),
            )
            .child(
                Button::new("next-list")
                    .label("▶")
                    .small()
                    .disabled(list_count <= 1)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_next_list(&TaskListNextList, window, cx);
                    })),
            )
            .child(
                Button::new("new-list")
                    .label("New list")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_new_list(&TaskListNewList, window, cx);
                    })),
            )
            .child(
                Button::new("new-task")
                    .label("New item")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_create_below(&TaskListCreateBelow, window, cx);
                    })),
            )
            .child(div().flex_1().min_w_0().child(search));
        if let Some(tag) = &self.working_set.tag_filter {
            row = row.child(
                Button::new("active-tag-filter")
                    .label(format!("Tag: {tag}"))
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_clear_tag_filter(&TaskListClearTagFilter, window, cx);
                    })),
            );
        }
        row.child(
            Button::new("sort-toggle")
                .label(sort_label)
                .small()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.cycle_sort_and_show_menu(window, cx);
                })),
        )
    }

    fn render_toolbar(
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
            .flex_wrap()
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
            .child(div().flex_1().min_w(px(120.)).child(search));

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

    /// Quick filter toggles above the tree, in the status-filter row style:
    /// "Pending changes" narrows it to nodes with pending incoming changes
    /// (and their ancestors). Shown while any node has one, or while on.
    fn render_quick_filters(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        use gpui::IntoElement as _;
        let pending_nodes = self
            .all_tasks
            .iter()
            .filter(|t| t.incoming_count > 0)
            .count();
        let needs_you_nodes = self
            .all_tasks
            .iter()
            .filter(|t| t.needs_you_count > 0)
            .count();
        let running_nodes = self
            .all_tasks
            .iter()
            .filter(|t| t.live_run_count > 0)
            .count();
        if pending_nodes == 0
            && needs_you_nodes == 0
            && running_nodes == 0
            && !self.working_set.pending_changes_only
            && !self.working_set.needs_you_only
            && !self.working_set.running_only
            && self.marked.is_empty()
        {
            return None;
        }
        let running = self
            .incoming_check
            .as_ref()
            .is_some_and(|c| c.read(cx).is_running());
        let check_label = if self.marked.is_empty() {
            "Check incoming changes".to_string()
        } else {
            format!("Check incoming changes ({} marked)", self.marked.len())
        };
        let check_button = chrome_control_with_shortcut(
            Button::new("check-incoming-changes")
                .label(check_label)
                .ghost()
                .small()
                .disabled(running || self.incoming_check.is_none())
                .on_click(cx.listener(|this, _, _, cx| this.check_incoming(cx))),
            window,
            &TaskListCheckIncoming,
            TASK_LIST_CONTEXT,
            cx,
        );
        let clear_marks = (!self.marked.is_empty()).then(|| {
            Button::new("clear-marks")
                .label("Clear marks")
                .ghost()
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.clear_marks(cx)))
        });
        Some(
            gpui_component::h_flex()
                .flex_wrap()
                .items_center()
                .gap(crate::ui::style::space::HAIRLINE)
                .px(crate::ui::style::space::RELATED)
                .py(crate::ui::style::space::HAIRLINE)
                .child(crate::ui::style::button_toggle(
                    Button::new("pending-changes-filter")
                        .label(format!("Pending changes {pending_nodes}"))
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.working_set.pending_changes_only =
                                !this.working_set.pending_changes_only;
                            this.rebuild_visible_list(window, cx);
                        })),
                    self.working_set.pending_changes_only,
                ))
                .child(crate::ui::style::button_toggle(
                    Button::new("needs-you-filter")
                        .label(format!("Needs you ({needs_you_nodes})"))
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.working_set.needs_you_only = !this.working_set.needs_you_only;
                            this.record_quick_filter_toggle(
                                "needs-you-filter",
                                this.working_set.needs_you_only,
                                cx,
                            );
                            this.rebuild_visible_list(window, cx);
                        })),
                    self.working_set.needs_you_only,
                ))
                .child(crate::ui::style::button_toggle(
                    Button::new("running-filter")
                        .label(format!("Running ({running_nodes})"))
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.working_set.running_only = !this.working_set.running_only;
                            this.record_quick_filter_toggle(
                                "running-filter",
                                this.working_set.running_only,
                                cx,
                            );
                            this.rebuild_visible_list(window, cx);
                        })),
                    self.working_set.running_only,
                ))
                .child(check_button)
                .children(clear_marks)
                .into_any_element(),
        )
    }

    /// The shared incoming-changes check's progress while it runs, and its
    /// summary afterwards: every node's outcome, and **Move back all** for
    /// the ones whose verdict sends them back (one confirmation).
    fn render_incoming_check(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        use crate::ui::selectable_text::selectable_text;
        use gpui::IntoElement as _;
        use gpui_component::scroll::ScrollableElement as _;
        use gpui_component::{h_flex, v_flex};
        let check = self.incoming_check.clone()?;
        let check = check.read(cx);
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let card = v_flex()
            .gap(crate::ui::style::space::HAIRLINE)
            .mx(crate::ui::style::space::RELATED)
            .my(crate::ui::style::space::HAIRLINE)
            .p(crate::ui::style::space::RELATED)
            .border_1()
            .border_color(crate::ui::style::color::incoming_text())
            .rounded_md();
        if let Some((done, total)) = check.progress() {
            return Some(
                card.child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .child(format!("Checking incoming changes: {done} of {total} done")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("One agent session per node. Keep working meanwhile."),
                )
                .into_any_element(),
            );
        }
        if !check.has_summary() {
            return None;
        }
        let affected = check.affected();
        let heading = match (check.results().len(), affected.len()) {
            (0, _) => "Incoming-changes check".to_string(),
            (n, 0) => format!("Checked {n} node(s): none needs to move back"),
            (n, k) => format!("Checked {n} node(s): {k} should move back"),
        };
        let error = check.error().map(str::to_string);
        let lines: Vec<String> = check.results().iter().map(outcome_line).collect();
        let moved = check.moved().map(str::to_string);
        let armed = check.move_back_armed();
        let mut card = card.child(div().text_xs().font_semibold().child(heading));
        if let Some(error) = error {
            card = card.child(div().text_xs().text_color(danger).child(selectable_text(
                "incoming-check-error",
                error,
                window,
                cx,
            )));
        }
        // Tree order (`IncomingCheck`), capped so a large check scrolls
        // instead of pushing the tree off screen.
        let mut list = v_flex()
            .id("incoming-check-results")
            .gap(crate::ui::style::space::HAIRLINE)
            .max_h(crate::ui::style::size::SUMMARY_LIST_MAX)
            .overflow_y_scrollbar();
        for (i, line) in lines.into_iter().enumerate() {
            list = list.child(div().text_xs().child(selectable_text(
                format!("incoming-check-result-{i}"),
                format!("* {line}"),
                window,
                cx,
            )));
        }
        card = card.child(list);
        let offer_move = !affected.is_empty() && moved.is_none();
        if let Some(moved) = moved {
            card = card.child(div().text_xs().text_color(muted).child(selectable_text(
                "incoming-check-moved",
                moved,
                window,
                cx,
            )));
        }
        let mut buttons = h_flex().gap_1();
        if offer_move {
            if armed {
                buttons = buttons
                    .child(
                        Button::new("incoming-check-move-back-confirm")
                            .label(format!("Confirm: move {} node(s) back", affected.len()))
                            .primary()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| this.move_back_all(cx))),
                    )
                    .child(
                        Button::new("incoming-check-move-back-cancel")
                            .label("Cancel")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(check) = &this.incoming_check {
                                    check.update(cx, |c, cx| c.cancel_move_back(cx));
                                }
                            })),
                    );
            } else {
                buttons = buttons.child(
                    Button::new("incoming-check-move-back-all")
                        .label("Move back all")
                        .primary()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.move_back_all(cx))),
                );
            }
        }
        buttons = buttons.child(
            Button::new("incoming-check-dismiss")
                .label("Dismiss")
                .ghost()
                .small()
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(check) = &this.incoming_check {
                        check.update(cx, |c, cx| c.dismiss(cx));
                    }
                })),
        );
        Some(card.child(buttons).into_any_element())
    }

    fn move_back_all(&mut self, cx: &mut Context<Self>) {
        if let Some(check) = &self.incoming_check {
            check.update(cx, |c, cx| c.move_back_all(cx));
        }
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

/// Whether two ordered row sets show the same rows in the same places -- only
/// their field values may differ. An added, removed or moved row is not a
/// match: fixing up the selection for one is `live_refresh`'s job.
fn same_visible_rows(shown: &[TaskItem], next: &[TaskItem]) -> bool {
    shown.len() == next.len() && shown.iter().zip(next).all(|(a, b)| a.id == b.id)
}

/// One reload of everything this view draws, read on the background
/// executor so the store queries never run on the UI thread.
struct LiveSnapshot {
    lists: Vec<tod_store::outline::types::OutlineList>,
    tasks: Vec<TaskItem>,
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
        // Hosted as a column only in the workbench.
        if self.marks_focused_column {
            Some(AppDestination::Workbench)
        } else {
            Some(AppDestination::Tasks)
        }
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
        self.sync_generator_filter_from_input(window, cx);
        self.publish_selection(cx);
        if self.pending_live_refresh {
            self.pending_live_refresh = false;
            self.live_refresh(window, cx);
        }
        if self.pending_attention_apply {
            self.pending_attention_apply = false;
            self.rebuild_visible_list(window, cx);
        }
        if self.pending_status_override_apply {
            self.pending_status_override_apply = false;
            self.rebuild_visible_list(window, cx);
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
        if self.pending_refocus_list {
            self.pending_refocus_list = false;
            if !self.is_editing() {
                self.focus_handle.focus(window, cx);
            }
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
            .on_action(cx.listener(Self::on_focus_drawer))
            .on_action(cx.listener(Self::on_open_agent_chat))
            .on_action(cx.listener(Self::on_report_problem))
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
            .on_action(cx.listener(Self::on_open_code))
            .on_action(cx.listener(Self::on_open_action_panel))
            .on_action(cx.listener(Self::on_row_lifecycle))
            .on_action(cx.listener(Self::on_refresh_generator))
            .on_action(cx.listener(Self::on_open_external))
            .on_action(cx.listener(Self::on_row_edit))
            .on_action(cx.listener(Self::on_open_edit_panel))
            .on_action(cx.listener(Self::on_open_edit_panel_ctrl))
            .on_action(cx.listener(Self::on_open_obligations))
            .on_action(cx.listener(Self::on_open_plan))
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
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_toggle_mark))
            .on_action(cx.listener(Self::on_check_incoming))
            .on_action(cx.listener(on_app_nav_toggle::<Self>))
            .child(self.render_header(window, cx))
            .when_some(self.render_quick_filters(window, cx), |el, bar| {
                el.child(bar)
            })
            .when_some(self.render_incoming_check(window, cx), |el, card| {
                el.child(card)
            })
            .child(body)
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
    use super::row_menu::RowMenuKind;
    use super::{Attention, TaskListEvent, TaskListView, delegate};
    use crate::views::rows::fixture::Fixture;
    use chrono::Utc;
    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
    use gpui_component::Root;

    type Events = std::rc::Rc<std::cell::RefCell<Vec<TaskListEvent>>>;

    fn open_view<'a>(
        fixture: &Fixture,
        cx: &'a mut TestAppContext,
    ) -> (Entity<TaskListView>, Events, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        // `TaskListView::new` resolves `TodPaths::discover()` for its
        // working-set file; pin it to a scratch dir for the test.
        let data_root = std::env::temp_dir().join(format!("tod-task-list-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_root).unwrap();
        tod_store::set_data_root(data_root);
        let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let events: Events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let store = fixture.store.clone();
        let (slot_in, events_in) = (slot.clone(), events.clone());
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| TaskListView::new(window, cx, store));
            cx.subscribe(&view, move |_, _, event: &TaskListEvent, _| {
                events_in.borrow_mut().push(event.clone());
            })
            .detach();
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        draw(cx);
        (view, events, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn right_click_opens_the_context_menu_for_the_row(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let node_id = fixture.node_id.to_string();
        let (view, _events, cx) = open_view(&fixture, cx);
        view.update_in(cx, |view, window, cx| {
            view.open_context_menu(&node_id, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.open_row_menu,
                Some((RowMenuKind::Context, node_id.clone()))
            );
            assert!(view.row_menu.is_some(), "menu entity was built");
            assert_eq!(view.working_set.selected_id.as_deref(), Some(node_id.as_str()));
        });
    }

    #[gpui::test]
    fn context_menu_rename_entry_starts_inline_edit(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let node_id = fixture.node_id.to_string();
        let (view, _events, cx) = open_view(&fixture, cx);
        // Right-click, then run the same thing the menu's "Rename (F2)" entry
        // does — the entries dispatch through `TaskListView`'s own methods
        // (see `context_menu::build`), so driving `start_inline_edit`
        // directly exercises the same path the click would.
        view.update_in(cx, |view, window, cx| {
            view.open_context_menu(&node_id, window, cx);
            view.start_inline_edit(&node_id, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.edit_open_for.as_deref(), Some(node_id.as_str()));
        });
    }

    #[gpui::test]
    fn attention_badge_click_emits_open_decisions(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let node_id = fixture.node_id.to_string();
        let (view, events, cx) = open_view(&fixture, cx);
        view.update_in(cx, |view, window, cx| {
            view.handle_row_action(
                delegate::RowAction::OpenDecisions {
                    task_id: node_id.clone(),
                },
                window,
                cx,
            );
        });
        draw(cx);
        assert!(events.borrow().iter().any(
            |e| matches!(e, TaskListEvent::OpenDecisions { task_id } if task_id == &node_id)
        ));
    }

    #[gpui::test]
    fn set_attention_badges_rows_and_needs_you_filter_keeps_ancestors(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let node_id = fixture.node_id.to_string();
        let (view, _events, cx) = open_view(&fixture, cx);
        let mut map = std::collections::HashMap::new();
        map.insert(
            node_id.clone(),
            Attention {
                count: 2,
                waiting_since: Utc::now(),
            },
        );
        view.update(cx, |view, cx| {
            view.set_attention(map, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            let task = view.all_tasks.iter().find(|t| t.id == node_id).unwrap();
            assert_eq!(task.needs_you_count, 2);
            assert!(task.waiting_since.is_some());
        });
        // The "Needs you" filter keeps the node visible (it matches).
        view.update_in(cx, |view, window, cx| {
            view.working_set.needs_you_only = true;
            view.rebuild_visible_list(window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, cx| {
            let visible = view.list_state.read(cx).delegate().items();
            assert!(visible.iter().any(|t| t.id == node_id));
        });
    }

    /// Only arrow keys wrap from the first row to the last; a click there
    /// must land. Regression test for clicks on the last row being undone as
    /// if they were a wrap.
    #[gpui::test]
    fn clicking_the_last_row_from_the_first_selects_it(cx: &mut TestAppContext) {
        use gpui_component::IndexPath;
        use gpui_component::list::ListEvent;
        use tod_store::outline::{CreatePosition, OutlineMutation};

        let fixture = Fixture::new();
        let list_id = fixture.store.list_outline_lists().unwrap()[0].id;
        for title in ["Second node", "Third node"] {
            fixture
                .store
                .enqueue_outline(OutlineMutation::CreateNode {
                    node_id: Some(uuid::Uuid::new_v4()),
                    list_id,
                    parent_id: None,
                    anchor_id: None,
                    position: CreatePosition::Below,
                    title: title.into(),
                })
                .unwrap();
        }
        fixture.store.writer().flush().unwrap();
        let (view, _events, cx) = open_view(&fixture, cx);
        view.update_in(cx, |view, window, cx| {
            view.refresh(window, cx);
        });
        draw(cx);

        let last = view.read_with(cx, |view, cx| {
            view.list_state.read(cx).delegate().items_count() - 1
        });
        assert!(last >= 2, "the fixture should show at least three rows");
        let list_state = view.read_with(cx, |view, _| view.list_state.clone());
        list_state.update_in(cx, |state, window, cx| {
            state.set_selected_index(Some(IndexPath::new(0)), window, cx);
        });
        view.update(cx, |view, _| view.last_selected = Some(IndexPath::new(0)));
        list_state.update_in(cx, |state, window, cx| {
            state.set_selected_index(Some(IndexPath::new(last)), window, cx);
            cx.emit(ListEvent::Confirm(IndexPath::new(last)));
        });
        draw(cx);

        view.read_with(cx, |view, cx| {
            assert_eq!(view.last_selected.map(|ix| ix.row), Some(last));
            let selected = view.list_state.read(cx).selected_index().map(|ix| ix.row);
            assert_eq!(selected, Some(last));
        });
    }

    /// A store-change-triggered reload (`refresh` -> `live_refresh` ->
    /// `reload_all_tasks`) reads rows straight from the outline store, which
    /// knows nothing about `set_attention`'s "needs you" data. Regression
    /// test for a bug where any such reload — triggered by any store change,
    /// not just an attention change — silently wiped the badge back to zero
    /// until the next attention poll happened to land, showing as a flicker
    /// in the unified view's tree.
    #[gpui::test]
    fn refresh_after_set_attention_keeps_the_badge(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let node_id = fixture.node_id.to_string();
        let (view, _events, cx) = open_view(&fixture, cx);
        let mut map = std::collections::HashMap::new();
        map.insert(
            node_id.clone(),
            Attention {
                count: 1,
                waiting_since: Utc::now(),
            },
        );
        view.update(cx, |view, cx| {
            view.set_attention(map, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            let task = view.all_tasks.iter().find(|t| t.id == node_id).unwrap();
            assert_eq!(task.needs_you_count, 1);
        });

        // Any store change reruns `live_refresh`/`reload_all_tasks`, unrelated
        // to attention — the badge must survive it.
        view.update_in(cx, |view, window, cx| {
            view.refresh(window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            let task = view.all_tasks.iter().find(|t| t.id == node_id).unwrap();
            assert_eq!(
                task.needs_you_count, 1,
                "a reload triggered by an unrelated store change must not clear the \"needs you\" badge"
            );
            assert!(task.waiting_since.is_some());
        });
    }

    #[test]
    fn same_visible_rows_ignores_field_changes_but_not_shape_changes() {
        let rows = large_fixture_set(3);
        let mut retitled = rows.clone();
        retitled[1].title = "Renamed".into();
        assert!(super::same_visible_rows(&rows, &retitled));

        let removed: Vec<_> = rows.iter().skip(1).cloned().collect();
        assert!(!super::same_visible_rows(&rows, &removed));

        let mut reordered = rows.clone();
        reordered.swap(0, 1);
        assert!(!super::same_visible_rows(&rows, &reordered));
    }

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
    fn format_delete_error_shows_agent_message_without_prefix() {
        let message = super::TaskListView::format_delete_error(
            "node \"Agent task\" has associated agents — remove them before deleting",
        );
        assert!(message.contains("associated agents"));
        assert!(message.contains("Agent task"));
        assert!(!message.starts_with("Delete failed:"));
    }

    #[test]
    fn format_delete_error_prefixes_unknown_errors() {
        let message = super::TaskListView::format_delete_error("database locked");
        assert_eq!(message, "Delete failed: database locked");
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
    fn title_sort_preserves_tree_hierarchy() {
        let mut parent = large_fixture_set(1)[0].clone();
        parent.id = "parent-id".into();
        parent.depth = 0;
        parent.title = "Parent".into();
        parent.parent_id = None;
        let mut child_a = parent.clone();
        child_a.id = "child-a".into();
        child_a.depth = 1;
        child_a.title = "Alpha child".into();
        child_a.parent_id = Some("parent-id".into());
        child_a.tree_ordinal = 1;
        let mut child_b = child_a.clone();
        child_b.id = "child-b".into();
        child_b.title = "Beta child".into();
        child_b.tree_ordinal = 2;
        let mut root_other = parent.clone();
        root_other.id = "root-other".into();
        root_other.title = "Zeta root".into();
        root_other.parent_id = None;
        let ws = ListWorkingSet {
            sort_key: SortKey::Title,
            sort_direction: SortDirection::Asc,
            ..ListWorkingSet::default_sort()
        };
        let visible = filter_and_sort_tasks(&[parent, child_b, child_a, root_other], "", &ws);
        assert_eq!(visible.len(), 4);
        assert_eq!(visible[0].title, "Parent");
        assert_eq!(visible[1].title, "Alpha child");
        assert_eq!(visible[2].title, "Beta child");
        assert_eq!(visible[3].title, "Zeta root");
        assert_eq!(visible[1].depth, 1);
    }
}
