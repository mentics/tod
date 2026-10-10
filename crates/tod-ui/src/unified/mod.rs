//! The unified view: one node tree in column 1, and any number of panels in
//! columns 2 onward, placed by the rule in `doc/ui/unified-view.md`.
//!
//! `columns` is the pure layout model (no GPUI); `panel` is the
//! `ColumnPanel` contract every panel implements, with the events panels
//! send the root; the panels themselves are in `panels`. This module wires
//! them into a GPUI view root, hosts `TaskListView` in column 1, and
//! registers the view's keys.

mod attention_feed;
mod chat_drawer;
mod columns;
mod panel;
pub mod panels;
pub mod requests;
mod resize;
pub mod runners;
pub mod status_label;

pub use columns::{Column, ColumnModel, PanelKind};
pub use panel::ColumnPanel;
use chat_drawer::{ChatDrawer, ChatDrawerEvent};
use panel::{PanelFocusSelected, PanelOpenChat, PanelOpenRequest};
use panels::DetailsPanel;
use resize::{
    ChatDrawerEdge, ColumnDivider, DIVIDER_WIDTH, DividerDrag, PANEL_MIN_WIDTH, ResizeStart,
    TREE_MIN_WIDTH, chat_height_at, starting_tree_width,
};

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext, Bounds, Context, DragMoveEvent, Entity, EntityId, FocusHandle,
    Focusable, InteractiveElement, IntoElement, KeyBinding, MouseDownEvent, ParentElement, Pixels,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, actions,
    canvas, div, prelude::FluentBuilder, px,
};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme, IconName, Selectable, Sizable};
use tod_core::attention::NodeAttention;
use tod_core::task::model::RunSource;
use tod_core::workbench_layout::{self, WorkbenchLayout};
use tod_store::conversation::Focus;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::interview::TodPaths;
use crate::interview::agent::SharedAgent;
use crate::ui::agent_chat::OpenAgentChat;
use crate::ui::agent_runs::AgentRuns;
use crate::ui::app_nav::{AppNavToggle, HasAppNav};
use crate::ui::code_links::{OpenCodeRef, open_code_ref};
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, PaneFocusRight, bind_pane_nav};
use crate::ui::style;
use crate::views::lifecycle_control::LifecycleController;
use crate::views::task_list::{TaskListEvent, TaskListView};

/// One column-2+ panel entity. Most host an existing view embedded, as
/// `conversation/context_panel.rs` already does.
enum HostedPanel {
    Details(Entity<DetailsPanel>),
    Task(Entity<panels::task::TaskPanel>),
    Obligations(Entity<panels::obligations::ObligationsPanel>),
    Plan(Entity<panels::plan::PlanPanel>),
    Findings(Entity<panels::findings::FindingsPanel>),
    Settings(Entity<panels::settings::SettingsPanel>),
    Transcript(Entity<panels::transcript::TranscriptPanel>),
    Changes(Entity<panels::changes::ChangesPanel>),
}

impl HostedPanel {
    fn title(&self, cx: &App) -> SharedString {
        match self {
            Self::Details(e) => e.read(cx).title(cx),
            Self::Task(e) => e.read(cx).title(cx),
            Self::Obligations(e) => e.read(cx).title(cx),
            Self::Plan(e) => e.read(cx).title(cx),
            Self::Findings(e) => e.read(cx).title(cx),
            Self::Settings(e) => e.read(cx).title(cx),
            Self::Transcript(e) => e.read(cx).title(cx),
            Self::Changes(e) => e.read(cx).title(cx),
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Self::Details(e) => e.read(cx).focus_handle(cx),
            Self::Task(e) => e.read(cx).focus_handle(cx),
            Self::Obligations(e) => e.read(cx).focus_handle(cx),
            Self::Plan(e) => e.read(cx).focus_handle(cx),
            Self::Findings(e) => e.read(cx).focus_handle(cx),
            Self::Settings(e) => e.read(cx).focus_handle(cx),
            Self::Transcript(e) => e.read(cx).focus_handle(cx),
            Self::Changes(e) => e.read(cx).focus_handle(cx),
        }
    }

    fn entity_id(&self) -> EntityId {
        match self {
            Self::Details(e) => e.entity_id(),
            Self::Task(e) => e.entity_id(),
            Self::Obligations(e) => e.entity_id(),
            Self::Plan(e) => e.entity_id(),
            Self::Findings(e) => e.entity_id(),
            Self::Settings(e) => e.entity_id(),
            Self::Transcript(e) => e.entity_id(),
            Self::Changes(e) => e.entity_id(),
        }
    }

    fn render(&self) -> AnyElement {
        match self {
            Self::Details(e) => e.clone().into_any_element(),
            Self::Task(e) => e.clone().into_any_element(),
            Self::Obligations(e) => e.clone().into_any_element(),
            Self::Plan(e) => e.clone().into_any_element(),
            Self::Findings(e) => e.clone().into_any_element(),
            Self::Settings(e) => e.clone().into_any_element(),
            Self::Transcript(e) => e.clone().into_any_element(),
            Self::Changes(e) => e.clone().into_any_element(),
        }
    }
}
pub use chat_drawer::register_chat_drawer_keyboard_bindings;

actions!(
    unified,
    [
        UnifiedTogglePinFocused,
        UnifiedNextWaiting,
        UnifiedPrevWaiting,
        UnifiedCloseFocusedColumn
    ]
);

pub const UNIFIED_CONTEXT: &str = "Unified";

/// Register the unified view's own keys: Alt+W (pin the focused column) and
/// Ctrl+Left/Right (`ui/pane_nav.rs`) between columns. Call once at startup
/// alongside every other `register_*_keyboard_bindings`.
pub fn register_unified_keyboard_bindings(cx: &mut App) {
    bind_pane_nav(cx, UNIFIED_CONTEXT);
    panels::details::register_details_panel_keyboard_bindings(cx);
    requests::register_request_keyboard_bindings(cx);
    let context = Some(key_context::excluding_input(UNIFIED_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("alt-w", UnifiedTogglePinFocused, context),
        KeyBinding::new("alt-q", UnifiedNextWaiting, context),
        KeyBinding::new("alt-shift-q", UnifiedPrevWaiting, context),
        KeyBinding::new("ctrl-w", UnifiedCloseFocusedColumn, context),
    ]);
    register_chat_drawer_keyboard_bindings(cx);
    panels::transcript::register_transcript_panel_bindings(cx);
}

/// A column-2+ slot: the model's bookkeeping plus the panel entity backing
/// it and the subscription (where the panel kind emits one) that carries its
/// open requests up to the root.
struct HostedColumn {
    panel: HostedPanel,
    _subscriptions: Vec<Subscription>,
}

/// A column-2+ slot's width, kept beside `UnifiedView::hosted` (same index)
/// so it survives the slot's panel being replaced.
#[derive(Default)]
struct ColumnWidth {
    /// The width the user dragged it to; `None` shares what is left evenly.
    dragged: Option<Pixels>,
    /// Where it was at the last layout, which a drag starts from.
    laid_out: Rc<Cell<Bounds<Pixels>>>,
}

/// Where the view is, as the app's Back and Forward record it
/// (`ui::nav_history`): the tree's selection and what each column shows.
/// Pins come back with it but are not part of what makes two places
/// different, so pinning a column is not a step Back retraces.
#[derive(Debug, Clone)]
pub struct WorkbenchPlace {
    pub node: Option<Uuid>,
    pub columns: Vec<Column>,
}

impl PartialEq for WorkbenchPlace {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
            && self.columns.len() == other.columns.len()
            && self
                .columns
                .iter()
                .zip(&other.columns)
                .all(|(a, b)| a.panel == b.panel)
    }
}

impl Eq for WorkbenchPlace {}

pub struct UnifiedView {
    fleet: Arc<FleetStore>,
    paths: TodPaths,
    agent_runs: Entity<AgentRuns>,
    /// The one lifecycle controller the shell shares with the conversation
    /// view and the lifecycle panel (`.claude/CLAUDE.md`): a gate check
    /// started or waived in either shows in the task panel too.
    lifecycle: Entity<LifecycleController>,
    /// Every task's runner on this machine, shared with the task panels.
    runners: Entity<runners::NodeRunners>,
    task_list: Entity<TaskListView>,
    columns: ColumnModel,
    hosted: Vec<HostedColumn>,
    column_widths: Vec<ColumnWidth>,
    /// Column 1's width: about 80 characters (`resize::starting_tree_width`)
    /// until the user drags its divider. `None` until the first render,
    /// which has the window to measure the font with.
    tree_width: Option<Pixels>,
    /// The divider drag in progress, from its first move.
    resize: Option<ResizeStart>,
    /// The widths saved from the last drag, in this or an earlier run
    /// (`tod_core::workbench_layout`); a column opening at a position takes
    /// the width saved for it.
    layout: WorkbenchLayout,
    /// The bottom-of-window chat drawer (W8): a freeform conversation about
    /// whichever node is currently in focus. Never shown for the tree
    /// itself; see `chat_drawer`.
    chat_drawer: Entity<ChatDrawer>,
    /// The most recent selection that can hold an agent session — a node
    /// (tree, or Details when retargeted), an obligation, or a plan step —
    /// whichever panel it happened in. Drives the chat drawer
    /// (`doc/ui/unified-view.md` "The chat drawer"). Selecting something
    /// that cannot have one (a finding, a decision) leaves this alone.
    last_chat_focus: Focus,
    focus_handle: FocusHandle,
    /// What every node is waiting on the user for, recomputed off the UI
    /// thread on every store change (`attention_feed`) and fed to the tree
    /// via `TaskListView::set_attention`; Alt+Q walks the same data
    /// (`doc/ui/unified-view-plan.md` W12).
    attention: HashMap<Uuid, NodeAttention>,
    /// The node [`Self::default_panel`] last judged, and whether it was a
    /// task node then. A store change that turns the selected node into a
    /// task node (or back) swaps its default panel in place
    /// ([`Self::follow_default_panel`]).
    last_default: Cell<Option<(Uuid, bool)>>,
    /// The node [`Self::restore_place`] selected in the tree: its
    /// `SelectionChanged`, delivered later, must not open the node's default
    /// panel over the columns that were restored with it.
    restoring_selection: Option<Uuid>,
    /// Who was working on each node when the tree was last told: the
    /// lifecycle processor, and only the chat panel. A node leaving both
    /// has stopped, and lands in [`Self::finished`].
    last_lifecycle_running: std::collections::HashSet<Uuid>,
    last_chat_running: std::collections::HashSet<Uuid>,
    /// Nodes whose run stopped since the user last selected them, oldest
    /// first, and which kind stopped. The tree marks them (orange, ringed
    /// icon) and Alt+Q visits them after the nodes waiting on the user;
    /// selecting one (by click or by Alt+Q) clears it.
    finished: Vec<(Uuid, RunSource)>,
    _task_list_subscription: Subscription,
    _agent_runs_subscription: Subscription,
    _runners_subscription: Subscription,
    _chat_drawer_subscription: Subscription,
    _attention_poll: gpui::Task<()>,
}

impl UnifiedView {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        fleet: Arc<FleetStore>,
        paths: TodPaths,
        agent: SharedAgent,
        agent_runs: Entity<AgentRuns>,
        lifecycle: Entity<LifecycleController>,
    ) -> Self {
        let task_list = cx.new(|cx| {
            let mut task_list = TaskListView::new(window, cx, fleet.clone());
            task_list.set_marks_focused_column(true);
            task_list
        });
        let _task_list_subscription =
            cx.subscribe_in(&task_list, window, |this, _, event: &TaskListEvent, window, cx| {
                this.on_task_list_event(event, window, cx);
            });
        let layout = workbench_layout::load(paths.config_dir());
        let chat_height = layout.chat_height.map(px);
        let chat_drawer = cx.new(|cx| {
            ChatDrawer::new(window, cx, fleet.clone(), agent.clone(), agent_runs.clone(), chat_height)
        });
        // Collapsing the drawer hands focus back to the focused column (or
        // the tree), so the keyboard is never left on the collapsed tab.
        let _chat_drawer_subscription =
            cx.subscribe_in(&chat_drawer, window, |this, _, event, window, cx| match event {
                ChatDrawerEvent::Collapsed => this.sync_window_focus(window, cx),
            });
        let _agent_runs_subscription = cx.observe(&agent_runs, |this, _, cx| {
            this.apply_status_overrides(cx);
        });
        let runners = {
            let (fleet, agent, agent_runs) = (fleet.clone(), agent.clone(), agent_runs.clone());
            cx.new(|cx| runners::NodeRunners::new(fleet, agent, agent_runs, cx))
        };
        let _runners_subscription = cx.observe(&runners, |this, _, cx| this.apply_status_overrides(cx));
        let _attention_poll = Self::spawn_attention_poll(fleet.clone(), window, cx);
        let mut this = Self {
            fleet,
            paths,
            agent_runs,
            lifecycle,
            runners,
            task_list,
            columns: ColumnModel::new(),
            hosted: Vec::new(),
            column_widths: Vec::new(),
            tree_width: None,
            resize: None,
            layout,
            chat_drawer,
            last_chat_focus: Focus::Project,
            focus_handle: cx.focus_handle(),
            attention: HashMap::new(),
            last_default: Cell::new(None),
            restoring_selection: None,
            last_lifecycle_running: std::collections::HashSet::new(),
            last_chat_running: std::collections::HashSet::new(),
            finished: Vec::new(),
            _task_list_subscription,
            _agent_runs_subscription,
            _runners_subscription,
            _chat_drawer_subscription,
            _attention_poll,
        };
        this.apply_status_overrides(cx);
        // The tree comes up with the node selected last time already
        // selected, which sends no `SelectionChanged`: show its default
        // panel as selecting it would.
        if let Some(node_id) = this.task_list.read(cx).selected_node_id() {
            this.open_panel(this.default_panel(node_id), 0, false, window, cx);
        }
        this
    }

    /// Recomputes `attention_feed::compute` off the UI thread whenever the
    /// store changes (`FleetStore::subscribe_changes`), once immediately at
    /// startup, then feeds it to the tree and keeps it for Alt+Q
    /// (`doc/ui/unified-view-plan.md` W12 "Feed attention into the tree").
    ///
    /// The same pass re-judges whether the selected node is a task node, so
    /// its default panel follows a capability change.
    fn spawn_attention_poll(
        fleet: Arc<FleetStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            let mut rx = fleet.subscribe_changes();
            loop {
                let Ok(selected) = this.update(cx, |this, cx| this.task_list.read(cx).selected_node_id()) else {
                    break;
                };
                let fleet_for_read = fleet.clone();
                let (computed, is_task) = cx
                    .background_executor()
                    .spawn(async move {
                        let is_task = selected.map(|id| {
                            let is_task = fleet_for_read
                                .read(|conn| tod_store::fleet::node_actions::is_task_node(conn, id))
                                .unwrap_or(false);
                            (id, is_task)
                        });
                        (attention_feed::compute(&fleet_for_read), is_task)
                    })
                    .await;
                let Ok(()) = this.update_in(cx, |this, window, cx| {
                    this.apply_attention(computed, cx);
                    if let Some((node_id, is_task)) = is_task {
                        this.follow_default_panel(node_id, is_task, window, cx);
                    }
                }) else {
                    break;
                };
                if rx.recv().await.is_err() {
                    break;
                }
                // Coalesce any further changes that arrived while computing.
                while rx.try_recv().is_ok() {}
            }
        })
    }

    /// `node_id` is (or is no longer) a task node: when that changed since
    /// [`Self::default_panel`] last judged it, an unpinned column showing
    /// the old default for it (Details or Task) shows the new one, in place
    /// and without moving focus. A Details the user opened on a node that
    /// stays a task node is left alone.
    fn follow_default_panel(
        &mut self,
        node_id: Uuid,
        is_task: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((judged, was_task)) = self.last_default.get() else {
            return;
        };
        if judged != node_id || was_task == is_task {
            return;
        }
        self.last_default.set(Some((node_id, is_task)));
        let (old, new) = if is_task {
            (PanelKind::Details(node_id), PanelKind::Task(node_id))
        } else {
            (PanelKind::Task(node_id), PanelKind::Details(node_id))
        };
        let Some(ix) = self
            .columns
            .columns()
            .iter()
            .position(|column| !column.pinned && column.panel == old)
        else {
            return;
        };
        // Already shown elsewhere: nothing to swap.
        if self.columns.columns().iter().any(|column| column.panel == new) {
            return;
        }
        self.columns.replace(ix, new);
        self.hosted[ix] = self.construct_hosted(new, window, cx);
        if self.columns.focused_index() == Some(ix) {
            self.sync_window_focus(window, cx);
        }
        cx.notify();
    }

    /// Store the freshly computed attention map and hand its
    /// `TaskListView::set_attention` shape to the tree.
    fn apply_attention(
        &mut self,
        map: HashMap<Uuid, NodeAttention>,
        cx: &mut Context<Self>,
    ) {
        let for_tree = attention_feed::to_task_list_map(&map);
        self.attention = map;
        self.task_list.update(cx, |task_list, cx| {
            task_list.set_attention(for_tree, cx);
        });
    }

    /// Tells the tree which nodes the lifecycle processor is working on (a
    /// runner with a run in progress, or a lifecycle conversation mid-turn),
    /// which only a chat is, and which stopped since the user last selected
    /// them. A node that stops while the user already has it selected is not
    /// marked: they are looking at it.
    fn apply_running_nodes(&mut self, cx: &mut Context<Self>) {
        let mut lifecycle = self.agent_runs.read(cx).running_lifecycle_nodes();
        lifecycle.extend(self.runners.read(cx).running_nodes());
        let chat = self.agent_runs.read(cx).running_chat_nodes();
        let selected = self.task_list.read(cx).selected_node_id();
        let was_active: Vec<Uuid> = self
            .last_lifecycle_running
            .iter()
            .chain(&self.last_chat_running)
            .copied()
            .collect();
        for node in was_active {
            if lifecycle.contains(&node) || chat.contains(&node) || selected == Some(node) {
                continue;
            }
            let source = if self.last_lifecycle_running.contains(&node) {
                RunSource::Lifecycle
            } else {
                RunSource::Chat
            };
            self.finished.retain(|(id, _)| *id != node);
            self.finished.push((node, source));
        }
        // A node working again is no longer "finished".
        self.finished
            .retain(|(id, _)| !lifecycle.contains(id) && !chat.contains(id));
        self.last_lifecycle_running = lifecycle;
        self.last_chat_running = chat;
        self.feed_running_nodes(cx);
    }

    fn feed_running_nodes(&mut self, cx: &mut Context<Self>) {
        let ids = |set: &std::collections::HashSet<Uuid>| -> std::collections::HashSet<String> {
            set.iter().map(|n| n.to_string()).collect()
        };
        let lifecycle = ids(&self.last_lifecycle_running);
        let chat = ids(&self.last_chat_running);
        let finished = self
            .finished
            .iter()
            .map(|(id, source)| (id.to_string(), *source))
            .collect();
        self.task_list
            .update(cx, |task_list, cx| task_list.set_running_nodes(lifecycle, chat, finished, cx));
    }

    /// The user has seen `node`: drop its "finished" marker.
    fn clear_finished(&mut self, node: Uuid, cx: &mut Context<Self>) {
        let before = self.finished.len();
        self.finished.retain(|(id, _)| *id != node);
        if self.finished.len() != before {
            self.feed_running_nodes(cx);
        }
    }

    /// Recomputes every running node's status label (W11) from
    /// [`AgentRuns`] and hands the map to `TaskListView` in one call — never
    /// per row per frame, since `AgentRuns::running_status_labels` reads
    /// each running slot's conversation row.
    fn apply_status_overrides(&mut self, cx: &mut Context<Self>) {
        self.apply_running_nodes(cx);
        let fleet = self.fleet.clone();
        let map = self.agent_runs.read(cx).running_status_labels(|node| {
            fleet
                .get_node(&node.to_string())
                .ok()
                .flatten()
                .map(|task| task.lifecycle)
        });
        let awaiting = self.runners.read(cx).awaiting_nodes();
        let ids: std::collections::HashSet<String> = awaiting.iter().map(|n| n.to_string()).collect();
        self.task_list.update(cx, |task_list, cx| task_list.set_awaiting(ids, cx));
        let mut map: std::collections::HashMap<String, String> =
            map.into_iter().map(|(id, label)| (id.to_string(), label)).collect();
        // A node whose run ended to wait (for a review, say) is out of the
        // way: nobody owes it anything, time has to pass.
        for node in awaiting {
            let id = node.to_string();
            if map.contains_key(&id) {
                continue;
            }
            if let Some(task) = self.fleet.get_node(&id).ok().flatten() {
                map.insert(id, format!("{} · waiting", task.lifecycle));
            }
        }
        self.task_list.update(cx, |task_list, cx| {
            task_list.set_status_overrides(map, cx);
        });
    }

    fn on_task_list_event(
        &mut self,
        event: &TaskListEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TaskListEvent::SelectionChanged { task_id } => {
                let node_id = task_id.as_deref().and_then(|id| Uuid::parse_str(id).ok());
                let restored = self.restoring_selection.take();
                if let Some(id) = node_id {
                    self.clear_finished(id, cx);
                    // A selection Back or Forward made brings its own columns.
                    if restored != Some(id) {
                        // The node tree (column 1) always counts as pinned, so a
                        // selection opens the node's default panel in the first
                        // unpinned column starting at column 2 (index 0), as a
                        // plain (non-ctrl) open.
                        self.open_panel(self.default_panel(id), 0, false, window, cx);
                    }
                    self.set_chat_focus(Focus::Node(id), cx);
                }
            }
            // The tree's right-click menu (W4) and the attention badge on a
            // row emit these; map each to the column it opens
            // (`doc/ui/unified-view-plan.md` W12 "map the tree's
            // right-click menu to columns").
            TaskListEvent::OpenTaskEdit { task_id } | TaskListEvent::OpenActionPanel { task_id } => {
                if let Ok(id) = Uuid::parse_str(task_id) {
                    self.open_panel(self.default_panel(id), 0, false, window, cx);
                }
            }
            TaskListEvent::OpenTaskEditCtrl { task_id } => {
                if let Ok(id) = Uuid::parse_str(task_id) {
                    self.open_panel(self.default_panel(id), 0, true, window, cx);
                }
            }
            TaskListEvent::OpenObligations { task_id, .. } => {
                if let Ok(id) = Uuid::parse_str(task_id) {
                    self.open_panel(PanelKind::Obligations(id), 0, false, window, cx);
                }
            }
            TaskListEvent::OpenPlan { task_id, .. } => {
                if let Ok(id) = Uuid::parse_str(task_id) {
                    self.open_panel(PanelKind::Plan(id), 0, false, window, cx);
                }
            }
            TaskListEvent::OpenTaskPanel { task_id } => {
                if let Ok(id) = Uuid::parse_str(task_id) {
                    self.open_panel(PanelKind::Task(id), 0, false, window, cx);
                }
            }
            TaskListEvent::OpenSettings { task_id } => {
                if let Ok(id) = Uuid::parse_str(task_id) {
                    self.open_panel(PanelKind::Settings(id), 0, false, window, cx);
                }
            }
            // The tree keeps Ctrl+Right for itself (it would open the Tasks
            // view's drawer); here it moves to the next column.
            TaskListEvent::FocusDrawer => {
                self.columns.focus_tree();
                self.columns.focus_right();
                self.sync_window_focus(window, cx);
            }
            _ => {}
        }
    }

    /// The panel a node opens by default: the task panel for a task node
    /// (`tod_store::fleet::node_actions::is_task_node`), else Details.
    /// Generator and managed nodes get their own arms later.
    ///
    /// One short `fleet.read` (capability lookups and an ancestor walk), the
    /// same kind of read `DetailsPanel`'s load does on this thread.
    fn default_panel(&self, node_id: Uuid) -> PanelKind {
        let is_task = self
            .fleet
            .read(|conn| tod_store::fleet::node_actions::is_task_node(conn, node_id))
            .unwrap_or(false);
        self.last_default.set(Some((node_id, is_task)));
        if is_task {
            PanelKind::Task(node_id)
        } else {
            PanelKind::Details(node_id)
        }
    }

    /// Build the panel entity for `target`, and its subscription when its
    /// kind can emit [`PanelOpenRequest`] (`Findings` today; every other
    /// real panel has no links of its own yet).
    fn construct_hosted(
        &self,
        target: PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> HostedColumn {
        match target {
            PanelKind::Details(node_id) => {
                let panel = cx.new(|cx| {
                    DetailsPanel::new(node_id, self.fleet.clone(), self.agent_runs.clone(), window, cx)
                });
                let panel_id = panel.entity_id();
                let subscription =
                    cx.subscribe_in(&panel, window, move |this, _, event: &PanelOpenRequest, window, cx| {
                        this.route_open_request(panel_id, event, window, cx);
                    });
                HostedColumn {
                    panel: HostedPanel::Details(panel),
                    _subscriptions: vec![subscription],
                }
            }
            PanelKind::Task(node_id) => {
                let panel =
                    cx.new(|cx| panels::task::TaskPanel::new(node_id, self.fleet.clone(), self.agent_runs.clone(), self.lifecycle.clone(), self.runners.clone(), window, cx));
                let panel_id = panel.entity_id();
                let subscription =
                    cx.subscribe_in(&panel, window, move |this, _, event: &PanelOpenRequest, window, cx| {
                        this.route_open_request(panel_id, event, window, cx);
                    });
                HostedColumn {
                    panel: HostedPanel::Task(panel),
                    _subscriptions: vec![subscription],
                }
            }
            PanelKind::Obligations(id) => {
                let panel = cx.new(|cx| {
                    panels::obligations::ObligationsPanel::new(id, self.fleet.clone(), window, cx)
                });
                let panel_id = panel.entity_id();
                let open_sub =
                    cx.subscribe_in(&panel, window, move |this, _, event: &PanelOpenRequest, window, cx| {
                        this.route_open_request(panel_id, event, window, cx);
                    });
                let focus_sub = cx.subscribe_in(
                    &panel,
                    window,
                    |this, _, event: &PanelFocusSelected, _window, cx| {
                        this.set_chat_focus(event.0, cx);
                    },
                );
                let chat_sub = cx.subscribe_in(
                    &panel,
                    window,
                    |this, _, event: &PanelOpenChat, window, cx| {
                        this.open_chat_on(event.0, window, cx);
                    },
                );
                HostedColumn {
                    panel: HostedPanel::Obligations(panel),
                    _subscriptions: vec![open_sub, focus_sub, chat_sub],
                }
            }
            PanelKind::Plan(id) => {
                let panel =
                    cx.new(|cx| panels::plan::PlanPanel::new(id, self.fleet.clone(), window, cx));
                let panel_id = panel.entity_id();
                let open_sub =
                    cx.subscribe_in(&panel, window, move |this, _, event: &PanelOpenRequest, window, cx| {
                        this.route_open_request(panel_id, event, window, cx);
                    });
                let focus_sub = cx.subscribe_in(
                    &panel,
                    window,
                    |this, _, event: &PanelFocusSelected, _window, cx| {
                        this.set_chat_focus(event.0, cx);
                    },
                );
                let chat_sub = cx.subscribe_in(
                    &panel,
                    window,
                    |this, _, event: &PanelOpenChat, window, cx| {
                        this.open_chat_on(event.0, window, cx);
                    },
                );
                HostedColumn {
                    panel: HostedPanel::Plan(panel),
                    _subscriptions: vec![open_sub, focus_sub, chat_sub],
                }
            }
            PanelKind::Findings(id) => {
                let panel = cx
                    .new(|cx| panels::findings::FindingsPanel::new(id, self.fleet.clone(), window, cx));
                let panel_id = panel.entity_id();
                let subscription =
                    cx.subscribe_in(&panel, window, move |this, _, event: &PanelOpenRequest, window, cx| {
                        this.route_open_request(panel_id, event, window, cx);
                    });
                HostedColumn {
                    panel: HostedPanel::Findings(panel),
                    _subscriptions: vec![subscription],
                }
            }
            PanelKind::Changes(id) => {
                let panel = cx
                    .new(|cx| panels::changes::ChangesPanel::new(id, self.fleet.clone(), window, cx));
                HostedColumn {
                    panel: HostedPanel::Changes(panel),
                    _subscriptions: Vec::new(),
                }
            }
            PanelKind::Settings(id) => {
                let panel = cx.new(|cx| {
                    panels::settings::SettingsPanel::new(
                        id,
                        self.fleet.clone(),
                        self.paths.clone(),
                        window,
                        cx,
                    )
                });
                HostedColumn {
                    panel: HostedPanel::Settings(panel),
                    _subscriptions: Vec::new(),
                }
            }
            PanelKind::Watch(id) => {
                let panel = cx.new(|cx| {
                    panels::transcript::TranscriptPanel::watching(
                        id,
                        self.fleet.clone(),
                        self.agent_runs.clone(),
                        window,
                        cx,
                    )
                });
                HostedColumn {
                    panel: HostedPanel::Transcript(panel),
                    _subscriptions: Vec::new(),
                }
            }
            PanelKind::Transcript(id) => {
                let panel = cx
                    .new(|cx| panels::transcript::TranscriptPanel::new(id, self.fleet.clone(), window, cx));
                HostedColumn {
                    panel: HostedPanel::Transcript(panel),
                    _subscriptions: Vec::new(),
                }
            }
        }
    }

    /// A hosted panel's own `PanelOpenRequest`: open it from the column that
    /// emitted it, by entity id (a column's index can move under it as
    /// others close).
    fn route_open_request(
        &mut self,
        panel_id: EntityId,
        event: &PanelOpenRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(col) = self
            .hosted
            .iter()
            .position(|h| h.panel.entity_id() == panel_id)
        else {
            return;
        };
        self.open_panel(event.target, col, event.ctrl, window, cx);
    }

    /// Point the chat drawer at `focus`, the most recent selection that can
    /// hold an agent session, wherever it happened
    /// (`doc/ui/unified-view.md` "The chat drawer"). `ChatDrawer::set_focus`
    /// itself is a no-op when `focus` already matches.
    fn set_chat_focus(&mut self, focus: Focus, cx: &mut Context<Self>) {
        self.last_chat_focus = focus;
        self.chat_drawer.update(cx, |drawer, cx| drawer.set_focus(focus, cx));
    }

    /// Ctrl+J toggles the chat drawer: captured before the tree's own
    /// `OpenAgentChat` handler can consume it.
    fn on_open_agent_chat(&mut self, _: &OpenAgentChat, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_chat(window, cx);
        cx.stop_propagation();
    }

    /// Show or hide the chat drawer.
    pub fn toggle_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_drawer.update(cx, |drawer, cx| drawer.toggle(window, cx));
    }

    /// Apply the column-placement rule and keep `hosted` in sync with the
    /// resulting `columns` model.
    fn open_panel(
        &mut self,
        target: PanelKind,
        from_column: usize,
        ctrl: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let PanelKind::Details(node_id) | PanelKind::Task(node_id) = target {
            self.set_chat_focus(Focus::Node(node_id), cx);
        }
        let before = self.columns.len();
        let ix = self.columns.open(target, from_column, ctrl);
        if ix < before {
            // Details keeps its entity (and any unsaved edit state) when it
            // is only retargeted to another node. So does the task panel:
            // rebuilding it would drop a half-typed freeform answer on
            // every Alt+Q.
            if let (HostedPanel::Details(panel), PanelKind::Details(node_id)) =
                (&self.hosted[ix].panel, target)
            {
                let panel = panel.clone();
                panel.update(cx, |panel, cx| panel.set_node(node_id, window, cx));
            } else if let (HostedPanel::Task(panel), PanelKind::Task(node_id)) =
                (&self.hosted[ix].panel, target)
            {
                // A task column retargeted to another task keeps its entity.
                let panel = panel.clone();
                panel.update(cx, |panel, cx| panel.set_node(node_id, cx));
            } else {
                self.hosted[ix] = self.construct_hosted(target, window, cx);
            }
        } else {
            let hosted = self.construct_hosted(target, window, cx);
            self.hosted.push(hosted);
            let position = self.column_widths.len();
            self.column_widths.push(ColumnWidth {
                dragged: self.layout.column_width(position).map(px),
                ..Default::default()
            });
        }
        cx.notify();
    }

    /// E on an item with no conversation yet: point the chat drawer at it
    /// and expand it, where its first conversation starts.
    fn open_chat_on(&mut self, focus: Focus, window: &mut Window, cx: &mut Context<Self>) {
        self.set_chat_focus(focus, cx);
        self.chat_drawer.update(cx, |drawer, cx| drawer.expand(window, cx));
    }

    fn close_column(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.hosted.len() {
            return;
        }
        self.hosted.remove(index);
        self.column_widths.remove(index);
        self.columns.close(index);
        cx.notify();
    }

    /// Ctrl+W: close the focused column (never column 1, the node tree,
    /// which has no `focused_index` of its own). Focus then follows
    /// `ColumnModel::close`'s neighbor rule (`doc/ui/unified-view.md`
    /// "Keys").
    fn close_focused_column(
        &mut self,
        _: &UnifiedCloseFocusedColumn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.columns.focused_index() else {
            return;
        };
        self.close_column(ix, cx);
        self.sync_window_focus(window, cx);
    }

    /// The app menu is the tree's (it sits in the tree's header), but `` ` ``
    /// must open it from anywhere in the view, not only with focus in the
    /// tree: with focus there the tree handles it first.
    fn toggle_app_nav(&mut self, _: &AppNavToggle, window: &mut Window, cx: &mut Context<Self>) {
        self.task_list
            .update(cx, |list, cx| list.toggle_app_nav(window, cx));
    }

    /// Close the app menu, e.g. when the shell switches views.
    pub fn close_app_nav(&mut self, cx: &mut Context<Self>) {
        self.task_list
            .update(cx, |list, _| list.app_nav_mut().close());
    }

    fn toggle_pin_focused(&mut self, _: &UnifiedTogglePinFocused, _: &mut Window, cx: &mut Context<Self>) {
        self.columns.toggle_pin_focused();
        cx.notify();
    }

    /// The Alt+Q order (longest-waiting first, matching the tree's
    /// `SortKey::WaitingLongest`) and the node adjacent to the current
    /// selection in it, wrapping around
    /// (`doc/ui/unified-view.md` "Alt+Q").
    /// Where the view is now, for the app's Back and Forward.
    pub fn place(&self, cx: &App) -> WorkbenchPlace {
        WorkbenchPlace {
            node: self.task_list.read(cx).selected_node_id(),
            columns: self.columns.columns().to_vec(),
        }
    }

    /// Put the view back at `place`: select its node in the tree and show
    /// its columns, keeping the panel entity (and any unsaved edit in it) of
    /// a column that already shows the same panel, and each position's
    /// width. Keyboard focus goes to the tree. A node no longer in the tree
    /// is left unselected; [`Self::place`] then tells where the view ended up.
    pub fn restore_place(&mut self, place: &WorkbenchPlace, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(node) = place.node {
            if self.task_list.read(cx).selected_node_id() != Some(node) {
                let task_id = node.to_string();
                self.task_list
                    .update(cx, |task_list, cx| task_list.reveal_node(&task_id, window, cx));
                self.restoring_selection = (self.task_list.read(cx).selected_node_id()
                    == Some(node))
                .then_some(node);
            }
            self.set_chat_focus(Focus::Node(node), cx);
        }
        let old_panels: Vec<PanelKind> =
            self.columns.columns().iter().map(|column| column.panel).collect();
        let mut old_hosted: Vec<Option<HostedColumn>> =
            std::mem::take(&mut self.hosted).into_iter().map(Some).collect();
        self.hosted = place
            .columns
            .iter()
            .map(|column| {
                old_panels
                    .iter()
                    .zip(old_hosted.iter_mut())
                    .find(|(panel, hosted)| **panel == column.panel && hosted.is_some())
                    .and_then(|(_, hosted)| hosted.take())
                    .unwrap_or_else(|| self.construct_hosted(column.panel, window, cx))
            })
            .collect();
        for position in self.column_widths.len()..place.columns.len() {
            self.column_widths.push(ColumnWidth {
                dragged: self.layout.column_width(position).map(px),
                ..Default::default()
            });
        }
        self.column_widths.truncate(place.columns.len());
        self.columns = ColumnModel::from_columns(place.columns.clone());
        self.focus_tree(window, cx);
    }

    /// The node selected in this view's tree, with its title.
    pub fn selected_node_with_title(&self, cx: &App) -> Option<(Uuid, String)> {
        self.task_list.read(cx).selected_node_with_title()
    }

    fn next_waiting_node(&self, forward: bool, cx: &Context<Self>) -> Option<Uuid> {
        // Nodes waiting on the user come first; then those whose run stopped
        // unseen, oldest first.
        let mut order = attention_feed::waiting_order(&self.attention);
        let unseen: Vec<Uuid> = self
            .finished
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| !order.contains(id))
            .collect();
        order.extend(unseen);
        if order.is_empty() {
            return None;
        }
        let current = self.task_list.read(cx).selected_node_id();
        let index = current.and_then(|id| order.iter().position(|n| *n == id));
        let next_index = match (index, forward) {
            (Some(ix), true) => (ix + 1) % order.len(),
            (Some(ix), false) => (ix + order.len() - 1) % order.len(),
            (None, true) => 0,
            (None, false) => order.len() - 1,
        };
        Some(order[next_index])
    }

    /// Alt+Q / Alt+Shift+Q: select the next (or previous) node waiting on
    /// the user in the tree and show it in its default panel (the task
    /// panel for a task node), with keyboard focus on it so the number keys
    /// answer its top request at once. The panel opens by the ordinary
    /// column rule (`ColumnModel::open`): an already-open panel for the node
    /// is focused, else it replaces the first unpinned column, else a new
    /// column is appended; a pinned column is never replaced or unpinned
    /// (`doc/ui/unified-view.md` "Where a panel opens").
    fn advance_waiting(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.next_waiting_node(forward, cx) else {
            return;
        };
        let task_id = target.to_string();
        self.clear_finished(target, cx);
        self.task_list
            .update(cx, |task_list, cx| task_list.reveal_node(&task_id, window, cx));
        // Selecting in the tree opens the default panel too (through
        // `SelectionChanged`), but that event is delivered later; open it
        // now so focus can move to it in this same keypress.
        self.open_panel(self.default_panel(target), 0, false, window, cx);
        // `ColumnModel::open` points `focused_index` at the column; move
        // keyboard focus there as well, or the number keys do nothing until
        // the user clicks the panel.
        self.sync_window_focus(window, cx);
        cx.notify();
    }

    fn next_waiting(&mut self, _: &UnifiedNextWaiting, window: &mut Window, cx: &mut Context<Self>) {
        self.advance_waiting(true, window, cx);
    }

    fn prev_waiting(&mut self, _: &UnifiedPrevWaiting, window: &mut Window, cx: &mut Context<Self>) {
        self.advance_waiting(false, window, cx);
    }

    fn focus_left(&mut self, _: &PaneFocusLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_focused_column(window, cx);
        self.columns.focus_left();
        self.sync_window_focus(window, cx);
    }

    fn focus_right(&mut self, _: &PaneFocusRight, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_focused_column(window, cx);
        self.columns.focus_right();
        self.sync_window_focus(window, cx);
    }

    /// Point `columns`' focused index at the column that actually holds
    /// keyboard focus, so Ctrl+Left/Right step from where the user really is
    /// and the focused header marks where keys go. A click, or an open that
    /// leaves keys where they were (selecting a tree row opens Details, but
    /// the tree keeps focus), otherwise leaves the model somewhere else.
    /// Focus outside every column (the chat drawer) leaves it alone.
    fn sync_focused_column(&mut self, window: &Window, cx: &App) {
        if let Some(ix) = self
            .hosted
            .iter()
            .position(|h| h.panel.focus_handle(cx).contains_focused(window, cx))
        {
            self.columns.focus(ix);
        } else if self.task_list.read(cx).focus_handle(cx).contains_focused(window, cx) {
            self.columns.focus_tree();
        }
    }

    /// Put keyboard focus on the node tree: where keys go when the view is
    /// shown, at startup or on switching to it.
    pub fn focus_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.columns.focus_tree();
        self.sync_window_focus(window, cx);
    }

    /// Give focus back to the focused column (or the tree) after whatever
    /// held it went away, so key bindings still reach the shell.
    pub fn restore_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_window_focus(window, cx);
    }

    fn sync_window_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.columns.focused_index() {
            Some(ix) => {
                if let Some(hosted) = self.hosted.get(ix) {
                    let handle = hosted.panel.focus_handle(cx);
                    window.focus(&handle, cx);
                }
            }
            None => {
                let handle = self.task_list.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
        }
        cx.notify();
    }

    /// `chat_focused`: the chat drawer has focus, so its header is the one
    /// in the `column-focused` state and no column's is.
    fn render_column_header(
        &self,
        index: usize,
        chat_focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hosted = &self.hosted[index];
        let title = hosted.panel.title(cx);
        let pinned = self.columns.is_pinned(index);
        let focused = !chat_focused && self.columns.focused_index() == Some(index);
        style::column_header(div(), focused)
            .justify_between()
            .child(style::text_title(div()).child(title))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new(("unified-col-pin", index))
                            .label(if pinned { "Pinned" } else { "Pin" })
                            .small()
                            .selected(pinned)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.columns.toggle_pin(index);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("unified-col-close", index))
                            .icon(IconName::Close)
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_column(index, cx);
                            })),
                    ),
            )
    }

    /// A draggable divider; `divider` as in [`ColumnDivider`].
    fn render_divider(&self, divider: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (line, drag_border) = (theme.border, theme.drag_border);
        let view = cx.entity().downgrade();
        div()
            .id(("unified-divider", divider))
            .flex_shrink_0()
            .w(px(DIVIDER_WIDTH))
            .h_full()
            .flex()
            .justify_center()
            .cursor_col_resize()
            .hover(move |el| el.bg(drag_border))
            .child(div().w(px(1.)).h_full().bg(line))
            .on_drag(ColumnDivider(divider), move |_, _, _, cx| {
                // A new drag measures from where the columns are now.
                let _ = view.update(cx, |this, _| this.resize = None);
                cx.new(|_| DividerDrag)
            })
            .into_any_element()
    }

    /// Follow a divider drag: the first move records where the column left
    /// of it starts and the pair's width, and every move puts the divider
    /// under the pointer (`resize`). `view_left` is this view's left edge,
    /// where the tree starts.
    fn drag_divider(
        &mut self,
        divider: usize,
        x: Pixels,
        view_left: Pixels,
        cx: &mut Context<Self>,
    ) {
        let last = self.hosted.len().saturating_sub(1);
        if self.hosted.is_empty() || divider > last {
            return;
        }
        let start = match self.resize {
            Some(start) if start.divider == divider => start,
            _ => {
                let (left_edge, left) = match divider {
                    0 => (view_left, self.tree_width.unwrap_or(px(TREE_MIN_WIDTH))),
                    n => {
                        let bounds = self.column_widths[n - 1].laid_out.get();
                        (bounds.left(), bounds.size.width)
                    }
                };
                let pair = (divider < last)
                    .then(|| left + self.column_widths[divider].laid_out.get().size.width);
                let start = ResizeStart {
                    divider,
                    left_edge,
                    pair,
                };
                self.resize = Some(start);
                start
            }
        };
        let min_left = px(if divider == 0 { TREE_MIN_WIDTH } else { PANEL_MIN_WIDTH });
        let (left, right) = start.widths_at(x, min_left, px(PANEL_MIN_WIDTH));
        match divider {
            0 => self.tree_width = Some(left),
            n => self.column_widths[n - 1].dragged = Some(left),
        }
        if let Some(right) = right {
            self.column_widths[divider].dragged = Some(right);
        }
        cx.notify();
    }

    /// A divider or chat-edge drag ended: save every width and the chat
    /// drawer's height, for the next launch too. Positions not open now
    /// keep what was saved for them.
    fn save_layout(&mut self, cx: &mut Context<Self>) {
        self.resize = None;
        self.layout.tree_width = self.tree_width.map(f32::from);
        self.layout.chat_height = self.chat_drawer.read(cx).height().map(f32::from);
        for (position, column) in self.column_widths.iter().enumerate() {
            self.layout
                .set_column_width(position, column.dragged.map(f32::from));
        }
        let (config_dir, layout) = (self.paths.config_dir().to_path_buf(), self.layout.clone());
        cx.background_executor()
            .spawn(async move {
                if let Err(err) = workbench_layout::save(&config_dir, &layout) {
                    tracing::warn!("workbench: failed to save the layout: {err:#}");
                }
            })
            .detach();
    }

    /// Column `index + 2`, with a divider on its left. One the user has not
    /// sized takes an equal share of what is left; a new column squeezes the
    /// others rather than scrolling or folding them away.
    fn render_column(
        &self,
        index: usize,
        chat_focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let focus_handle = self.hosted[index].panel.focus_handle(cx);
        let header = self
            .render_column_header(index, chat_focused, cx)
            .into_any_element();
        let panel = self.hosted[index].panel.render();
        // The last column has no width of its own: it takes what is left.
        let last = index + 1 == self.hosted.len();
        let dragged = self.column_widths[index].dragged.filter(|_| !last);
        let laid_out = self.column_widths[index].laid_out.clone();
        div()
            .id(("unified-col", index))
            .relative()
            .flex()
            .flex_col()
            .map(|el| match dragged {
                // Shrinks with the window like the rest, but never grows.
                Some(width) => el.w(width),
                None => el.flex_1(),
            })
            .min_w_0()
            .h_full()
            .debug_selector(move || format!("unified-col-{index}"))
            .capture_any_mouse_down(focus_on_click(focus_handle))
            .child(
                canvas(
                    move |bounds, _, _| laid_out.set(bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(header)
            .child(div().flex_1().overflow_hidden().child(panel))
            .into_any_element()
    }
}

impl Focusable for UnifiedView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// A mouse-down anywhere in a column moves keyboard focus into it (and so
/// the focused column, which `sync_focused_column` reads from focus). Capture
/// phase, so a control inside that stops the click still counts; and only
/// when focus is not in the column already, so a click on something in it
/// that takes focus itself (a field) keeps it.
fn focus_on_click(handle: FocusHandle) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    move |_, window, cx| {
        if !handle.contains_focused(window, cx) {
            window.focus(&handle, cx);
        }
    }
}

impl Render for UnifiedView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_focused_column(window, cx);
        let border = cx.theme().border;
        let tree_width = *self
            .tree_width
            .get_or_insert_with(|| starting_tree_width(window, self.layout.tree_width.map(px)));
        let total = self.columns.len();
        let chat_focused = self
            .chat_drawer
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        // Divider `ix` sits left of column `ix`.
        let mut column_elements = Vec::new();
        for ix in 0..total {
            column_elements.push(self.render_divider(ix, cx));
            column_elements.push(
                self.render_column(ix, chat_focused, cx)
                    .into_any_element(),
            );
        }
        // Column 1: node tree on top (shrinks and scrolls as the drawer
        // below it expands), the chat drawer under it — never over it
        // (`doc/ui/unified-view.md` "The chat drawer"). Columns 2+ take the
        // full height.
        let task_list_focus = self.task_list.read(cx).focus_handle(cx);
        let tree_column = div()
            .flex_shrink_0()
            .w(tree_width)
            .h_full()
            .overflow_hidden()
            .when(total == 0, |el| el.border_r_1().border_color(border))
            .flex()
            .flex_col()
            .child(
                // On the tree, not the whole column: a click in the chat
                // drawer is not a click in the tree.
                div()
                    .id("unified-tree")
                    .debug_selector(|| "unified-tree".into())
                    .capture_any_mouse_down(focus_on_click(task_list_focus))
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.task_list.clone()),
            )
            .child(self.chat_drawer.clone());
        div()
            .id("unified-view")
            .key_context(UNIFIED_CONTEXT)
            .track_focus(&self.focus_handle)
            .capture_action(cx.listener(Self::on_open_agent_chat))
            .on_action(cx.listener(Self::toggle_app_nav))
            .on_action(cx.listener(|this, action: &OpenCodeRef, window, cx| {
                // The chat drawer follows the focused column's node, else
                // the tree selection: what the clicked text is about.
                let node = this.chat_drawer.read(cx).focus().node_id();
                open_code_ref(this.fleet.clone(), node, &action.target, window, cx);
            }))
            .on_action(cx.listener(Self::toggle_pin_focused))
            .on_action(cx.listener(Self::next_waiting))
            .on_action(cx.listener(Self::prev_waiting))
            .on_action(cx.listener(Self::focus_left))
            .on_action(cx.listener(Self::focus_right))
            .on_action(cx.listener(Self::close_focused_column))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ColumnDivider>, _, cx| {
                let divider = event.drag(cx).0;
                this.drag_divider(divider, event.event.position.x, event.bounds.left(), cx);
            }))
            .on_drop(cx.listener(|this, _: &ColumnDivider, _, cx| this.save_layout(cx)))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ChatDrawerEdge>, _, cx| {
                let bounds = event.bounds;
                let height = chat_height_at(event.event.position.y, bounds.top(), bounds.bottom());
                this.chat_drawer.update(cx, |drawer, cx| drawer.set_height(height, cx));
            }))
            .on_drop(cx.listener(|this, _: &ChatDrawerEdge, _, cx| this.save_layout(cx)))
            .size_full()
            .flex()
            .child(tree_column)
            .children(column_elements)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::rows::fixture::Fixture;
    use gpui::{Modifiers, TestAppContext, VisualTestContext, point};
    use gpui_component::Root;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn open_view<'a>(
        fixture: &Fixture,
        cx: &'a mut TestAppContext,
    ) -> (Entity<UnifiedView>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        // `TaskListView::new` (column 1) resolves `TodPaths` for its working
        // set. Tests get their own root, separate from the fixture's store.
        let config_root =
            std::env::temp_dir().join(format!("tod-unified-config-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&config_root).unwrap();
        open_view_in(fixture, &config_root, cx)
    }

    /// `open_view` on a config root that already exists, as a later launch.
    fn open_view_in<'a>(
        fixture: &Fixture,
        config_root: &std::path::Path,
        cx: &'a mut TestAppContext,
    ) -> (Entity<UnifiedView>, &'a mut VisualTestContext) {
        crate::interview::set_data_root(config_root.to_path_buf());
        let paths = crate::interview::TodPaths::discover().unwrap();
        let slot = Rc::new(RefCell::new(None));
        let store = fixture.store.clone();
        let agent: crate::interview::agent::SharedAgent = std::sync::Arc::new(std::sync::Mutex::new(
            Box::new(tod_agent::MockAgentProvider::new()),
        ));
        let agent_runs_for_test = cx.new(|_| AgentRuns::new(store.clone(), agent.clone()));
        let lifecycle_for_test = cx.new(|_| LifecycleController::new(store.clone()));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                UnifiedView::new(
                    window,
                    cx,
                    store,
                    paths,
                    agent,
                    agent_runs_for_test,
                    lifecycle_for_test,
                )
            });
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        draw(cx);
        (view, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// Widths saved when a drag ends come back on the next launch: the tree
    /// takes its width, and a column opened at a position takes that
    /// position's.
    #[gpui::test]
    fn dragged_sizes_are_saved_and_taken_by_the_next_launch(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;
        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
            view.tree_width = Some(px(412.));
            view.column_widths[0].dragged = Some(px(300.));
            view.chat_drawer
                .update(cx, |drawer, cx| drawer.set_height(px(450.), cx));
            view.save_layout(cx);
        });
        cx.run_until_parked();

        let config_dir = view.read_with(cx, |view, _| view.paths.config_dir().to_path_buf());
        let saved = workbench_layout::load(&config_dir);
        assert_eq!(saved.tree_width, Some(412.));
        assert_eq!(saved.column_width(0), Some(300.));
        assert_eq!(saved.chat_height, Some(450.));

        // A later launch: a fresh view reads the file.
        let (next, cx) = open_view_in(&fixture, &config_dir, cx);
        next.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
        });
        draw(cx);
        next.read_with(cx, |view, _| {
            assert_eq!(view.tree_width, Some(px(412.)));
            assert_eq!(view.column_widths[0].dragged, Some(px(300.)));
        });
        next.read_with(cx, |view, cx| {
            assert_eq!(view.chat_drawer.read(cx).height(), Some(px(450.)));
        });
    }

    #[gpui::test]
    fn selecting_a_node_opens_details_in_column_two(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), 1);
            assert_eq!(view.columns.columns()[0].panel, PanelKind::Details(node_id));
            // Keys stay in the tree, so its column is still the focused one.
            assert_eq!(view.columns.focused_index(), None);
        });
    }

    #[gpui::test]
    fn a_task_node_opens_the_task_panel(cx: &mut TestAppContext) {
        use tod_store::outline::{Capability, OutlineMutation};
        let fixture = Fixture::new();
        let plain = fixture.node_id;
        // A second node, made a task before the view opens (store writes
        // while the view runs wake it from another thread).
        let task_id = Uuid::new_v4();
        let list_id = fixture.store.list_outline_lists().unwrap()[0].id;
        fixture
            .store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(task_id),
                list_id,
                parent_id: None,
                anchor_id: Some(plain),
                position: tod_store::outline::CreatePosition::Below,
                title: "A task".into(),
            })
            .unwrap();
        fixture
            .store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: task_id,
                capabilities: vec![Capability::Lifecycle, Capability::Agent],
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();
        fixture.store.reload_if_stale().ok();

        let (view, cx) = open_view(&fixture, cx);
        // The fixture node has only Spec: not a task.
        assert_eq!(
            view.read_with(cx, |view, _| view.default_panel(plain)),
            PanelKind::Details(plain)
        );
        let target = view.read_with(cx, |view, _| view.default_panel(task_id));
        assert_eq!(target, PanelKind::Task(task_id));
        view.update_in(cx, |view, window, cx| {
            view.open_panel(target, 0, false, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.columns.columns()[0].panel, PanelKind::Task(task_id));
            let HostedPanel::Task(panel) = &view.hosted[0].panel else {
                panic!("expected a task panel");
            };
            assert_eq!(panel.read(cx).node_id(), task_id);
        });
    }

    #[gpui::test]
    fn the_default_panel_follows_the_node_becoming_a_task(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let node_id = fixture.node_id;
        let (view, cx) = open_view(&fixture, cx);
        view.update_in(cx, |view, window, cx| {
            let target = view.default_panel(node_id);
            assert_eq!(target, PanelKind::Details(node_id));
            view.open_panel(target, 0, false, window, cx);
            // Still not a task: a Details stays.
            view.follow_default_panel(node_id, false, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.columns()[0].panel, PanelKind::Details(node_id));
        });

        view.update_in(cx, |view, window, cx| {
            view.follow_default_panel(node_id, true, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.columns.len(), 1);
            assert_eq!(view.columns.columns()[0].panel, PanelKind::Task(node_id));
            let HostedPanel::Task(panel) = &view.hosted[0].panel else {
                panic!("expected a task panel");
            };
            assert_eq!(panel.read(cx).node_id(), node_id);
        });

        // The user opens Details on the task node: it stays while the node
        // stays a task.
        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
            view.follow_default_panel(node_id, true, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.columns()[0].panel, PanelKind::Details(node_id));
        });
    }

    #[gpui::test]
    fn opening_obligations_from_details_opens_a_second_column(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
            // Opening from column 2 (index 0), non-ctrl: since column 0 is
            // unpinned it is *replaced* — mirrors a click in that column.
            view.open_panel(PanelKind::Obligations(node_id), 0, true, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), 2);
            assert_eq!(
                view.columns.columns()[0].panel,
                PanelKind::Details(node_id)
            );
            assert_eq!(
                view.columns.columns()[1].panel,
                PanelKind::Obligations(node_id)
            );
        });
    }

    #[gpui::test]
    fn the_node_selected_at_startup_shows_its_default_panel(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        draw(cx);

        view.read_with(cx, |view, cx| {
            let selected = view.task_list.read(cx).selected_node_id();
            assert!(selected.is_some());
            assert_eq!(view.columns.len(), 1);
            assert_eq!(
                view.columns.columns()[0].panel,
                view.default_panel(selected.unwrap())
            );
            // Keys start in the tree.
            assert_eq!(view.columns.focused_index(), None);
        });
    }

    #[gpui::test]
    fn restoring_a_place_brings_back_its_columns_and_keeps_shown_panels(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        let earlier = view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
            view.open_panel(PanelKind::Obligations(node_id), 0, true, window, cx);
            view.place(cx)
        });
        // Plan replaces Details in column 2, and column 3 closes.
        view.update_in(cx, |view, window, cx| {
            view.close_column(1, cx);
            view.open_panel(PanelKind::Plan(node_id), 0, false, window, cx);
        });
        draw(cx);
        assert_ne!(view.read_with(cx, |view, cx| view.place(cx)), earlier);

        view.update_in(cx, |view, window, cx| view.restore_place(&earlier, window, cx));
        draw(cx);
        let obligations = view.read_with(cx, |view, cx| {
            assert_eq!(view.place(cx), earlier);
            assert_eq!(view.hosted.len(), 2);
            assert_eq!(view.column_widths.len(), 2);
            assert_eq!(view.columns.focused_index(), None);
            view.hosted[1].panel.entity_id()
        });

        // Obligations moves to column 2, keeping its entity.
        view.update_in(cx, |view, window, cx| {
            view.close_column(0, cx);
            view.restore_place(
                &WorkbenchPlace {
                    node: earlier.node,
                    columns: vec![earlier.columns[1].clone()],
                },
                window,
                cx,
            );
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.hosted.len(), 1);
            assert_eq!(view.hosted[0].panel.entity_id(), obligations);
        });
    }

    #[gpui::test]
    fn pinning_the_focused_column_keeps_it_when_replacing(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
            view.toggle_pin_focused(&UnifiedTogglePinFocused, window, cx);
            // Clicking column 2 again (now pinned) opens the next panel in a
            // new column instead of replacing it.
            view.open_panel(PanelKind::Obligations(node_id), 0, false, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), 2);
            assert!(view.columns.is_pinned(0));
            assert_eq!(
                view.columns.columns()[0].panel,
                PanelKind::Details(node_id)
            );
            assert_eq!(
                view.columns.columns()[1].panel,
                PanelKind::Obligations(node_id)
            );
        });
    }

    #[gpui::test]
    fn ctrl_j_toggles_the_chat_drawer_without_reaching_the_tree(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);

        view.read_with(cx, |view, cx| {
            assert!(!view.chat_drawer.read(cx).expanded());
        });
        cx.dispatch_action(crate::ui::agent_chat::OpenAgentChat);
        draw(cx);
        view.read_with(cx, |view, cx| {
            assert!(view.chat_drawer.read(cx).expanded());
        });
        cx.dispatch_action(crate::ui::agent_chat::OpenAgentChat);
        draw(cx);
        view.read_with(cx, |view, cx| {
            assert!(!view.chat_drawer.read(cx).expanded());
        });
    }

    #[gpui::test]
    fn selecting_a_node_points_the_chat_drawer_at_it(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, cx| {
            assert_eq!(view.chat_drawer.read(cx).focus(), Focus::Node(node_id));
        });
    }

    #[gpui::test]
    fn selecting_an_obligation_points_the_chat_drawer_at_it(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;
        let obligation_id = fixture.design_obligation;
        let before = view.read_with(cx, |view, cx| view.chat_drawer.read(cx).focus());

        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Obligations(node_id), 0, false, window, cx);
        });
        draw(cx);
        // Opening the list puts its cursor on the first row; that is not a
        // selection, so the drawer stays where it was.
        view.read_with(cx, |view, cx| {
            assert_eq!(view.chat_drawer.read(cx).focus(), before);
        });

        view.update_in(cx, |view, window, cx| {
            let Some(hosted) = view
                .hosted
                .iter_mut()
                .find(|h| matches!(h.panel, HostedPanel::Obligations(_)))
            else {
                panic!("obligations column opened");
            };
            let HostedPanel::Obligations(panel) = &hosted.panel else {
                unreachable!()
            };
            let panel = panel.clone();
            panel.update(cx, |panel, cx| {
                panel.select_obligation(obligation_id, window, cx);
            });
        });
        // The panel reports its selection to the drawer on its next render.
        draw(cx);

        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.chat_drawer.read(cx).focus(),
                Focus::Obligation {
                    node: node_id,
                    id: obligation_id,
                }
            );
        });
    }

    #[gpui::test]
    fn selecting_a_finding_leaves_the_chat_drawer_where_it_was(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        // Point the drawer at the node first, as selecting it in the tree
        // would.
        view.update_in(cx, |view, _window, cx| {
            view.set_chat_focus(Focus::Node(node_id), cx);
        });
        draw(cx);

        // Opening (and rendering) the findings panel — which never reports a
        // selection to the drawer, since a finding cannot hold an agent
        // session — must not move the drawer's focus.
        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Findings(node_id), 0, false, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, cx| {
            assert_eq!(view.chat_drawer.read(cx).focus(), Focus::Node(node_id));
        });
    }

    /// Adds a second node to the fixture (which only creates one) and a
    /// pending decision on it, so Alt+Q tests have two waiting nodes to
    /// order between.
    fn add_waiting_node(fixture: &Fixture, question: &str, waiting_before: Uuid) -> Uuid {
        use tod_store::interview::{ACTOR_USER, InterviewCommand};
        use tod_store::outline::{CreatePosition, OutlineMutation};

        let list_id = fixture.store.list_outline_lists().unwrap()[0].id;
        let node_id = Uuid::new_v4();
        fixture
            .store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node_id),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Second node".into(),
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();
        // `waiting_before` already has (or will get) a decision asked with an
        // earlier `since`; this one is asked after, so it waits less long.
        let _ = waiting_before;
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AskDecision {
                    node_id,
                    conversation_id: None,
                    protocol: None,
                    decision: tod_store::decisions::NewDecision {
                        question: question.to_string(),
                        options: vec!["a".to_string(), "b".to_string()],
                        evidence: Vec::new(),
                        ..Default::default()
                    },
                },
            )
            .unwrap();
        node_id
    }

    fn ask_decision(fixture: &Fixture, node_id: Uuid, question: &str) {
        use tod_store::interview::{ACTOR_USER, InterviewCommand};
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AskDecision {
                    node_id,
                    conversation_id: None,
                    protocol: None,
                    decision: tod_store::decisions::NewDecision {
                        question: question.to_string(),
                        options: vec!["a".to_string(), "b".to_string()],
                        evidence: Vec::new(),
                        ..Default::default()
                    },
                },
            )
            .unwrap();
    }

    /// Loads `attention_feed::compute` synchronously and applies it, rather
    /// than waiting on the background poll — the poll itself is exercised
    /// end to end by the mock smoke test.
    fn load_attention(view: &Entity<UnifiedView>, cx: &mut VisualTestContext) {
        view.update(cx, |view, cx| {
            let map = attention_feed::compute(&view.fleet);
            view.apply_attention(map, cx);
        });
    }

    #[gpui::test]
    fn alt_q_visits_the_longest_waiting_node_first_and_wraps(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let first = fixture.node_id;
        ask_decision(&fixture, first, "On the first node?");
        // Give the store's millisecond clock room to separate the two.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let second = add_waiting_node(&fixture, "On the second node?", first);
        let (view, cx) = open_view(&fixture, cx);
        load_attention(&view, cx);
        let order = view.read_with(cx, |view, _| attention_feed::waiting_order(&view.attention));
        // `add_waiting_node` asked its decision after `first`'s, so `first`
        // waits longer and sorts first — same rule as `SortKey::WaitingLongest`.
        assert_eq!(order, vec![first, second]);

        // The tree may already have a row selected (its own default), so
        // don't assume where the first press lands — only that it is one of
        // the two waiting nodes, and that every further press cycles
        // through `order`, wrapping around.
        view.update_in(cx, |view, window, cx| {
            view.next_waiting(&UnifiedNextWaiting, window, cx);
        });
        draw(cx);
        let after_first = view
            .read_with(cx, |view, cx| view.task_list.read(cx).selected_node_id())
            .expect("alt+q selects a waiting node");
        let start_ix = order
            .iter()
            .position(|id| *id == after_first)
            .expect("selection is one of the waiting nodes");

        view.update_in(cx, |view, window, cx| {
            view.next_waiting(&UnifiedNextWaiting, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.task_list.read(cx).selected_node_id(),
                Some(order[(start_ix + 1) % order.len()]),
            );
        });

        // Wraps back around to where the first press landed.
        view.update_in(cx, |view, window, cx| {
            view.next_waiting(&UnifiedNextWaiting, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.task_list.read(cx).selected_node_id(), Some(after_first));
        });

        // Alt+Shift+Q walks backward, wrapping the other way.
        view.update_in(cx, |view, window, cx| {
            view.prev_waiting(&UnifiedPrevWaiting, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.task_list.read(cx).selected_node_id(),
                Some(order[(start_ix + order.len() - 1) % order.len()]),
            );
        });
    }

    #[gpui::test]
    fn alt_q_opens_the_default_panel_without_touching_an_already_pinned_column(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let node_id = fixture.node_id;
        ask_decision(&fixture, node_id, "Only decision?");
        let (view, cx) = open_view(&fixture, cx);
        load_attention(&view, cx);

        // Pin an unrelated column first (Settings) so Alt+Q has to route
        // around it rather than replace or unpin it.
        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Settings(node_id), 0, false, window, cx);
            view.columns.set_pinned(0, true);
        });
        draw(cx);

        view.update_in(cx, |view, window, cx| {
            view.next_waiting(&UnifiedNextWaiting, window, cx);
        });
        draw(cx);

        let expected = view.read_with(cx, |view, _| view.default_panel(node_id));
        view.read_with(cx, |view, _| {
            // The pinned column is untouched...
            assert!(view.columns.is_pinned(0));
            assert_eq!(view.columns.columns()[0].panel, PanelKind::Settings(node_id));
            // ...and the node's default panel opened in a new column,
            // focused, and not pinned (the ordinary column rule).
            let ix = view.columns.focused_index().expect("a column is focused");
            assert_eq!(view.columns.columns()[ix].panel, expected);
            assert!(!view.columns.is_pinned(ix));
        });

        // A second Alt+Q press on the same lone waiting node focuses the
        // already open panel rather than opening another one.
        let before = view.read_with(cx, |view, _| view.columns.len());
        view.update_in(cx, |view, window, cx| {
            view.next_waiting(&UnifiedNextWaiting, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), before);
        });
    }

    #[gpui::test]
    fn ctrl_w_closes_the_focused_column_and_not_the_tree(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
            view.open_panel(PanelKind::Obligations(node_id), 1, true, window, cx);
            // The second open() appended and focused a new column; move keys
            // there as Ctrl+Right would.
            view.sync_window_focus(window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), 2);
            assert_eq!(view.columns.focused_index(), Some(1));
        });

        view.update_in(cx, |view, window, cx| {
            view.close_focused_column(&UnifiedCloseFocusedColumn, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), 1);
            assert_eq!(
                view.columns.columns()[0].panel,
                PanelKind::Details(node_id)
            );
            assert_eq!(view.columns.focused_index(), Some(0));
        });

        // Focus the node tree (column 1) directly, then Ctrl+W must be a
        // no-op: there is no focused column-2+ index to close.
        view.update_in(cx, |view, window, cx| {
            view.columns.focus_tree();
            view.sync_window_focus(window, cx);
            view.close_focused_column(&UnifiedCloseFocusedColumn, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), 1);
            assert_eq!(view.columns.focused_index(), None);
        });
    }

    #[gpui::test]
    fn ctrl_e_from_a_panel_opens_in_the_next_column_leaving_the_current_one(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;
        let conversation_id = Uuid::new_v4();

        view.update_in(cx, |view, window, cx| {
            // Column 2 (index 0) holds Details, focused.
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
            view.sync_window_focus(window, cx);
            // A Ctrl+E from that same column (as `route_open_request` would
            // dispatch, using the panel's own column as `from_column`) opens
            // strictly after it rather than replacing it.
            view.open_panel(PanelKind::Transcript(conversation_id), 0, true, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), 2);
            assert_eq!(
                view.columns.columns()[0].panel,
                PanelKind::Details(node_id)
            );
            assert_eq!(
                view.columns.columns()[1].panel,
                PanelKind::Transcript(conversation_id)
            );
            // Keys stay in Details, where Ctrl+E was pressed.
            assert_eq!(view.columns.focused_index(), Some(0));
        });
    }

    #[gpui::test]
    fn tree_menu_obligations_entry_opens_an_obligations_panel(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        // The right-click menu's "Obligations" entry calls
        // `TaskListView::open_obligations_panel` (`context_menu.rs`), which
        // the unified root maps to `PanelKind::Obligations`
        // (`on_task_list_event`).
        view.update_in(cx, |view, window, cx| {
            view.task_list.update(cx, |task_list, cx| {
                task_list.open_obligations_panel(&node_id.to_string(), window, cx);
            });
        });
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), 1);
            assert_eq!(
                view.columns.columns()[0].panel,
                PanelKind::Obligations(node_id)
            );
        });
    }

    /// Ctrl+Left/Right move keyboard focus between columns from wherever it
    /// really is: the tree (which keeps Ctrl+Right for itself), a column
    /// focused by a click rather than by the model, and a Settings column
    /// (whose `TaskEditView` handles Ctrl+Left on its own when standalone).
    #[gpui::test]
    fn ctrl_arrows_move_focus_between_columns(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
            view.open_panel(PanelKind::Settings(node_id), 0, true, window, cx);
            // Keyboard focus on the tree, as after clicking a row.
            let handle = view.task_list.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        });
        draw(cx);
        let (tree, details, settings) = view.read_with(cx, |view, cx| {
            (
                view.task_list.read(cx).focus_handle(cx),
                view.hosted[0].panel.focus_handle(cx),
                view.hosted[1].panel.focus_handle(cx),
            )
        });

        cx.dispatch_action(PaneFocusRight);
        draw(cx);
        cx.update(|window, cx| assert!(details.contains_focused(window, cx), "tree -> details"));

        cx.dispatch_action(PaneFocusRight);
        draw(cx);
        cx.update(|window, cx| assert!(settings.contains_focused(window, cx), "details -> settings"));

        cx.dispatch_action(PaneFocusLeft);
        draw(cx);
        cx.update(|window, cx| assert!(details.contains_focused(window, cx), "settings -> details"));

        // A click into the tree moves focus without telling the model.
        cx.update(|window, cx| window.focus(&tree, cx));
        cx.dispatch_action(PaneFocusRight);
        draw(cx);
        cx.update(|window, cx| assert!(details.contains_focused(window, cx), "clicked tree -> details"));

        cx.dispatch_action(PaneFocusLeft);
        draw(cx);
        cx.update(|window, cx| assert!(tree.contains_focused(window, cx), "details -> tree"));
    }

    /// A click in a column moves keyboard focus into it, and the focused
    /// column (the one whose header is marked) follows.
    #[gpui::test]
    fn clicking_a_column_focuses_it(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_view(&fixture, cx);
        let node_id = fixture.node_id;

        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
            view.open_panel(PanelKind::Findings(node_id), 0, true, window, cx);
            view.sync_window_focus(window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| assert_eq!(view.columns.focused_index(), Some(1)));

        let click = |cx: &mut VisualTestContext, selector: &'static str| {
            let bounds = cx.debug_bounds(selector).expect(selector);
            cx.simulate_click(bounds.bottom_right() - point(px(4.), px(4.)), Modifiers::default());
            draw(cx);
        };

        click(cx, "unified-col-0");
        let details = view.read_with(cx, |view, cx| view.hosted[0].panel.focus_handle(cx));
        cx.update(|window, cx| assert!(details.contains_focused(window, cx)));
        view.read_with(cx, |view, _| assert_eq!(view.columns.focused_index(), Some(0)));

        click(cx, "unified-tree");
        view.read_with(cx, |view, _| assert_eq!(view.columns.focused_index(), None));

        click(cx, "unified-col-1");
        view.read_with(cx, |view, _| assert_eq!(view.columns.focused_index(), Some(1)));
    }

    /// Alt+Q must move keyboard focus into the panel it opens — not just
    /// point `ColumnModel::focused_index` at it — so the number keys that
    /// answer the top request work on the very next keypress with no click
    /// in between.
    #[gpui::test]
    fn alt_q_focuses_the_panel_it_opens(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let node_id = fixture.node_id;
        ask_decision(&fixture, node_id, "Focus me?");
        let (view, cx) = open_view(&fixture, cx);
        load_attention(&view, cx);

        for _ in 0..2 {
            // The second press comes from inside the panel itself.
            view.update_in(cx, |view, window, cx| {
                view.next_waiting(&UnifiedNextWaiting, window, cx);
            });
            draw(cx);
            let focus = view.read_with(cx, |view, cx| {
                let ix = view.columns.focused_index().expect("a column is focused");
                assert_eq!(view.columns.columns()[ix].panel.node(), Some(node_id));
                view.hosted[ix].panel.focus_handle(cx)
            });
            cx.update(|window, _| {
                assert!(
                    focus.is_focused(window),
                    "Alt+Q should focus the panel so the next keypress can answer it"
                );
            });
        }
    }
}
