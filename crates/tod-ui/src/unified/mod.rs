//! The unified view: one node tree in column 1, and any number of panels in
//! columns 2 onward, placed by the rule in `doc/ui/unified-view.md`.
//!
//! `columns` is the pure layout model (no GPUI); `panel` is the
//! `ColumnPanel` contract and the `PlaceholderPanel` every panel kind uses
//! until later work items (W5, W7, W9) supply the real ones. This module
//! wires both into a GPUI view root, hosts `TaskListView` in column 1, and
//! registers the view's keys.

mod attention_feed;
mod chat_drawer;
mod columns;
mod panel;
pub mod panels;
pub mod status_label;

pub use columns::{ColumnModel, DEFAULT_VISIBLE_COLUMNS, PanelKind};
pub use panel::ColumnPanel;
use chat_drawer::ChatDrawer;
use panel::{PanelActivateFocusedLink, PanelCtrlActivateFocusedLink, PanelOpenRequest, PlaceholderPanel};
use panels::DetailsPanel;

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, EntityId, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, MouseDownEvent, ParentElement,
    Render, SharedString, Styled, Subscription, Window, actions, div, prelude::FluentBuilder, px,
};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme, IconName, Selectable, Sizable};
use tod_core::attention::NodeAttention;
use tod_store::conversation::Focus;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::interview::TodPaths;
use crate::interview::agent::SharedAgent;
use crate::ui::agent_chat::OpenAgentChat;
use crate::ui::agent_runs::AgentRuns;
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav};
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, PaneFocusRight, bind_pane_nav};
use crate::views::task_list::{TaskListEvent, TaskListView};

/// One column-2+ panel entity. Every kind but `Details` and `Decisions`
/// (still `PlaceholderPanel`, W5's and W9's respectively) is the real panel
/// W7 adds; each hosts an existing view embedded, as
/// `conversation/context_panel.rs` already does.
enum HostedPanel {
    Placeholder(Entity<PlaceholderPanel>),
    Details(Entity<DetailsPanel>),
    Decisions(Entity<panels::decisions::DecisionsPanel>),
    Obligations(Entity<panels::obligations::ObligationsPanel>),
    Plan(Entity<panels::plan::PlanPanel>),
    Findings(Entity<panels::findings::FindingsPanel>),
    Settings(Entity<panels::settings::SettingsPanel>),
    Transcript(Entity<panels::transcript::TranscriptPanel>),
}

impl HostedPanel {
    fn title(&self, cx: &App) -> SharedString {
        match self {
            Self::Placeholder(e) => e.read(cx).title(cx),
            Self::Details(e) => e.read(cx).title(cx),
            Self::Decisions(e) => e.read(cx).title(cx),
            Self::Obligations(e) => e.read(cx).title(cx),
            Self::Plan(e) => e.read(cx).title(cx),
            Self::Findings(e) => e.read(cx).title(cx),
            Self::Settings(e) => e.read(cx).title(cx),
            Self::Transcript(e) => e.read(cx).title(cx),
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Self::Placeholder(e) => e.read(cx).focus_handle(cx),
            Self::Details(e) => e.read(cx).focus_handle(cx),
            Self::Decisions(e) => e.read(cx).focus_handle(cx),
            Self::Obligations(e) => e.read(cx).focus_handle(cx),
            Self::Plan(e) => e.read(cx).focus_handle(cx),
            Self::Findings(e) => e.read(cx).focus_handle(cx),
            Self::Settings(e) => e.read(cx).focus_handle(cx),
            Self::Transcript(e) => e.read(cx).focus_handle(cx),
        }
    }

    fn entity_id(&self) -> EntityId {
        match self {
            Self::Placeholder(e) => e.entity_id(),
            Self::Details(e) => e.entity_id(),
            Self::Decisions(e) => e.entity_id(),
            Self::Obligations(e) => e.entity_id(),
            Self::Plan(e) => e.entity_id(),
            Self::Findings(e) => e.entity_id(),
            Self::Settings(e) => e.entity_id(),
            Self::Transcript(e) => e.entity_id(),
        }
    }

    fn render(&self) -> AnyElement {
        match self {
            Self::Placeholder(e) => e.clone().into_any_element(),
            Self::Details(e) => e.clone().into_any_element(),
            Self::Decisions(e) => e.clone().into_any_element(),
            Self::Obligations(e) => e.clone().into_any_element(),
            Self::Plan(e) => e.clone().into_any_element(),
            Self::Findings(e) => e.clone().into_any_element(),
            Self::Settings(e) => e.clone().into_any_element(),
            Self::Transcript(e) => e.clone().into_any_element(),
        }
    }
}
pub use chat_drawer::register_chat_drawer_keyboard_bindings;

actions!(unified, [UnifiedTogglePinFocused, UnifiedNextWaiting, UnifiedPrevWaiting]);

pub const UNIFIED_CONTEXT: &str = "Unified";

/// Register the unified view's own keys: Alt+W (pin the focused column) and
/// Ctrl+Left/Right (`ui/pane_nav.rs`) between columns. Call once at startup
/// alongside every other `register_*_keyboard_bindings`.
pub fn register_unified_keyboard_bindings(cx: &mut App) {
    bind_pane_nav(cx, UNIFIED_CONTEXT);
    panels::details::register_details_panel_keyboard_bindings(cx);
    panels::decisions::register_decisions_panel_keyboard_bindings(cx);
    let context = Some(key_context::excluding_input(UNIFIED_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("alt-w", UnifiedTogglePinFocused, context),
        KeyBinding::new("alt-q", UnifiedNextWaiting, context),
        KeyBinding::new("alt-shift-q", UnifiedPrevWaiting, context),
    ]);
    let panel_context = Some(key_context::excluding_input(panel::UNIFIED_PANEL_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("enter", PanelActivateFocusedLink, panel_context),
        KeyBinding::new("ctrl-enter", PanelCtrlActivateFocusedLink, panel_context),
    ]);
    register_chat_drawer_keyboard_bindings(cx);
}

/// A column-2+ slot: the model's bookkeeping plus the panel entity backing
/// it and the subscription (where the panel kind emits one) that carries its
/// open requests up to the root.
struct HostedColumn {
    panel: HostedPanel,
    _subscription: Option<Subscription>,
}

pub struct UnifiedView {
    fleet: Arc<FleetStore>,
    paths: TodPaths,
    agent_runs: Entity<AgentRuns>,
    task_list: Entity<TaskListView>,
    columns: ColumnModel,
    hosted: Vec<HostedColumn>,
    /// The bottom-of-window chat drawer (W8): a freeform conversation about
    /// whichever node is currently in focus. Never shown for the tree
    /// itself; see `chat_drawer`.
    chat_drawer: Entity<ChatDrawer>,
    focus_handle: FocusHandle,
    app_nav: AppNavMenu,
    /// What every node is waiting on the user for, recomputed off the UI
    /// thread on every store change (`attention_feed`) and fed to the tree
    /// via `TaskListView::set_attention`; Alt+Q walks the same data
    /// (`doc/ui/unified-view-plan.md` W12).
    attention: HashMap<Uuid, NodeAttention>,
    _task_list_subscription: Subscription,
    _agent_runs_subscription: Subscription,
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
    ) -> Self {
        let task_list = cx.new(|cx| TaskListView::new(window, cx, fleet.clone()));
        let _task_list_subscription =
            cx.subscribe_in(&task_list, window, |this, _, event: &TaskListEvent, window, cx| {
                this.on_task_list_event(event, window, cx);
            });
        let chat_drawer = cx.new(|cx| {
            ChatDrawer::new(window, cx, fleet.clone(), agent, agent_runs.clone())
        });
        let _agent_runs_subscription = cx.observe(&agent_runs, |this, _, cx| {
            this.apply_status_overrides(cx);
        });
        let _attention_poll = Self::spawn_attention_poll(fleet.clone(), cx);
        let mut this = Self {
            fleet,
            paths,
            agent_runs,
            task_list,
            columns: ColumnModel::new(),
            hosted: Vec::new(),
            chat_drawer,
            focus_handle: cx.focus_handle(),
            app_nav: AppNavMenu::default(),
            attention: HashMap::new(),
            _task_list_subscription,
            _agent_runs_subscription,
            _attention_poll,
        };
        this.apply_status_overrides(cx);
        this
    }

    /// Recomputes `attention_feed::compute` off the UI thread whenever the
    /// store changes (`FleetStore::subscribe_changes`), once immediately at
    /// startup, then feeds it to the tree and keeps it for Alt+Q
    /// (`doc/ui/unified-view-plan.md` W12 "Feed attention into the tree").
    fn spawn_attention_poll(fleet: Arc<FleetStore>, cx: &mut Context<Self>) -> gpui::Task<()> {
        cx.spawn(async move |this, cx| {
            let mut rx = fleet.subscribe_changes();
            loop {
                let fleet_for_read = fleet.clone();
                let computed = cx
                    .background_executor()
                    .spawn(async move { attention_feed::compute(&fleet_for_read) })
                    .await;
                let Ok(()) = this.update(cx, |this, cx| this.apply_attention(computed, cx)) else {
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

    /// Recomputes every running node's status label (W11) from
    /// [`AgentRuns`] and hands the map to `TaskListView` in one call — never
    /// per row per frame, since `AgentRuns::running_status_labels` reads
    /// each running slot's conversation row.
    fn apply_status_overrides(&mut self, cx: &mut Context<Self>) {
        let fleet = self.fleet.clone();
        let map = self.agent_runs.read(cx).running_status_labels(|node| {
            fleet
                .get_node(&node.to_string())
                .ok()
                .flatten()
                .map(|task| task.lifecycle)
        });
        let map: std::collections::HashMap<String, String> =
            map.into_iter().map(|(id, label)| (id.to_string(), label)).collect();
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
                if let Some(id) = node_id {
                    // The node tree (column 1) always counts as pinned, so a
                    // selection opens Details in the first unpinned column
                    // starting at column 2 (index 0), as a plain (non-ctrl) open.
                    self.open_panel(PanelKind::Details(id), 0, false, window, cx);
                }
                // The decisions panel is a singleton with no target of its own:
                // it always follows whichever node is current
                // (`doc/ui/unified-view.md` "Decisions"), so any open column
                // retargets in place rather than through the placement rule.
                self.sync_decisions_node(node_id, window, cx);
            }
            // The tree's right-click menu (W4) and the attention badge on a
            // row emit these; map each to the column it opens
            // (`doc/ui/unified-view-plan.md` W12 "map the tree's
            // right-click menu to columns").
            TaskListEvent::OpenTaskEdit { task_id } | TaskListEvent::OpenActionPanel { task_id } => {
                if let Ok(id) = Uuid::parse_str(task_id) {
                    self.open_panel(PanelKind::Details(id), 0, false, window, cx);
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
            TaskListEvent::OpenDecisions { task_id } => {
                if let Ok(id) = Uuid::parse_str(task_id) {
                    self.sync_decisions_node(Some(id), window, cx);
                    self.open_panel(PanelKind::Decisions, 0, false, window, cx);
                }
            }
            _ => {}
        }
    }

    fn sync_decisions_node(&mut self, node_id: Option<Uuid>, window: &mut Window, cx: &mut Context<Self>) {
        for hosted in &self.hosted {
            if let HostedPanel::Decisions(panel) = &hosted.panel {
                let panel = panel.clone();
                panel.update(cx, |panel, cx| panel.set_node(node_id, window, cx));
            }
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
                    _subscription: Some(subscription),
                }
            }
            PanelKind::Decisions => {
                let node_id = self.task_list.read(cx).selected_node_id();
                let panel = cx.new(|cx| {
                    panels::decisions::DecisionsPanel::new(
                        node_id,
                        self.fleet.clone(),
                        self.agent_runs.clone(),
                        window,
                        cx,
                    )
                });
                let panel_id = panel.entity_id();
                let subscription =
                    cx.subscribe_in(&panel, window, move |this, _, event: &PanelOpenRequest, window, cx| {
                        this.route_open_request(panel_id, event, window, cx);
                    });
                HostedColumn {
                    panel: HostedPanel::Decisions(panel),
                    _subscription: Some(subscription),
                }
            }
            PanelKind::Obligations(id) => {
                let panel = cx.new(|cx| {
                    panels::obligations::ObligationsPanel::new(id, self.fleet.clone(), window, cx)
                });
                let panel_id = panel.entity_id();
                let subscription =
                    cx.subscribe_in(&panel, window, move |this, _, event: &PanelOpenRequest, window, cx| {
                        this.route_open_request(panel_id, event, window, cx);
                    });
                HostedColumn {
                    panel: HostedPanel::Obligations(panel),
                    _subscription: Some(subscription),
                }
            }
            PanelKind::Plan(id) => {
                let panel =
                    cx.new(|cx| panels::plan::PlanPanel::new(id, self.fleet.clone(), window, cx));
                let panel_id = panel.entity_id();
                let subscription =
                    cx.subscribe_in(&panel, window, move |this, _, event: &PanelOpenRequest, window, cx| {
                        this.route_open_request(panel_id, event, window, cx);
                    });
                HostedColumn {
                    panel: HostedPanel::Plan(panel),
                    _subscription: Some(subscription),
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
                    _subscription: Some(subscription),
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
                    _subscription: None,
                }
            }
            PanelKind::Transcript(id) => {
                let panel = cx
                    .new(|cx| panels::transcript::TranscriptPanel::new(id, self.fleet.clone(), window, cx));
                HostedColumn {
                    panel: HostedPanel::Transcript(panel),
                    _subscription: None,
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

    /// The unified view's current focus for the chat drawer (W8): the
    /// focused column's target node, else the node tree's selection, else
    /// the whole project.
    fn chat_focus(&self, cx: &App) -> Focus {
        if let Some(ix) = self.columns.focused_index()
            && let Some(column) = self.columns.columns().get(ix)
            && let Some(node) = column.panel.node()
        {
            return Focus::Node(node);
        }
        match self.task_list.read(cx).selected_node_id() {
            Some(id) => Focus::Node(id),
            None => Focus::Project,
        }
    }

    /// Ctrl+J toggles the chat drawer here instead of opening the old
    /// conversation view: captured before the tree's own `OpenAgentChat`
    /// handler can consume it.
    fn on_open_agent_chat(&mut self, _: &OpenAgentChat, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_drawer.update(cx, |drawer, cx| drawer.toggle(window, cx));
        cx.stop_propagation();
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
        let before = self.columns.len();
        let ix = self.columns.open(target, from_column, ctrl);
        if ix < before {
            // Details keeps its entity (and any unsaved edit state) when it
            // is only retargeted to another node.
            if let (HostedPanel::Details(panel), PanelKind::Details(node_id)) =
                (&self.hosted[ix].panel, target)
            {
                let panel = panel.clone();
                panel.update(cx, |panel, cx| panel.set_node(node_id, window, cx));
            } else {
                self.hosted[ix] = self.construct_hosted(target, window, cx);
            }
        } else {
            let hosted = self.construct_hosted(target, window, cx);
            self.hosted.push(hosted);
        }
        cx.notify();
    }

    fn close_column(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.hosted.len() {
            return;
        }
        self.hosted.remove(index);
        self.columns.close(index);
        cx.notify();
    }

    fn toggle_pin_focused(&mut self, _: &UnifiedTogglePinFocused, _: &mut Window, cx: &mut Context<Self>) {
        self.columns.toggle_pin_focused();
        cx.notify();
    }

    /// The Alt+Q order (longest-waiting first, matching the tree's
    /// `SortKey::WaitingLongest`) and the node adjacent to the current
    /// selection in it, wrapping around
    /// (`doc/ui/unified-view.md` "Alt+Q").
    fn next_waiting_node(&self, forward: bool, cx: &Context<Self>) -> Option<Uuid> {
        let order = attention_feed::waiting_order(&self.attention);
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
    /// the user, and show it in the singleton decisions panel, opening and
    /// pinning its column if it is not shown yet. A column the user pinned
    /// is never unpinned or replaced: the decisions panel is a singleton
    /// (`ColumnModel::open`), so it either retargets in place wherever it
    /// already is, or opens in the first unpinned column (appending one if
    /// every column is pinned) — the same rule every other panel follows
    /// (`doc/ui/unified-view.md` "Where a panel opens", "Singleton panels").
    fn advance_waiting(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.next_waiting_node(forward, cx) else {
            return;
        };
        let already_shown = self
            .hosted
            .iter()
            .any(|h| matches!(h.panel, HostedPanel::Decisions(_)));
        let task_id = target.to_string();
        self.task_list
            .update(cx, |task_list, cx| task_list.reveal_node(&task_id, window, cx));
        self.sync_decisions_node(Some(target), window, cx);
        self.open_panel(PanelKind::Decisions, 0, false, window, cx);
        if !already_shown {
            if let Some(ix) = self
                .hosted
                .iter()
                .position(|h| matches!(h.panel, HostedPanel::Decisions(_)))
            {
                self.columns.set_pinned(ix, true);
            }
        }
        cx.notify();
    }

    fn next_waiting(&mut self, _: &UnifiedNextWaiting, window: &mut Window, cx: &mut Context<Self>) {
        self.advance_waiting(true, window, cx);
    }

    fn prev_waiting(&mut self, _: &UnifiedPrevWaiting, window: &mut Window, cx: &mut Context<Self>) {
        self.advance_waiting(false, window, cx);
    }

    fn focus_left(&mut self, _: &PaneFocusLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.columns.focus_left();
        self.sync_window_focus(window, cx);
    }

    fn focus_right(&mut self, _: &PaneFocusRight, window: &mut Window, cx: &mut Context<Self>) {
        self.columns.focus_right();
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

    fn render_column_header(
        &self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let accent = cx.theme().accent;
        let muted = cx.theme().muted_foreground;
        let hosted = &self.hosted[index];
        let title = hosted.panel.title(cx);
        let pinned = self.columns.is_pinned(index);
        let focused = self.columns.focused_index() == Some(index);
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(border)
            .when(focused, |el| el.bg(accent.opacity(0.08)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("{}", index + 2)),
                    )
                    .child(div().text_sm().child(title)),
            )
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

    fn render_column(&self, index: usize, folded: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let accent = cx.theme().accent;
        let focused = self.columns.focused_index() == Some(index);
        if folded {
            return div()
                .id(("unified-col-strip", index))
                .w(px(28.))
                .h_full()
                .flex_shrink_0()
                .border_l_1()
                .border_color(border)
                .flex()
                .items_start()
                .justify_center()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.columns.focus(index);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{}", index + 2)),
                )
                .into_any_element();
        }
        let header = self.render_column_header(index, cx);
        let panel = self.hosted[index].panel.render();
        div()
            .id(("unified-col", index))
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(220.))
            .h_full()
            .border_l_1()
            .border_color(border)
            .when(focused, |el| el.border_color(accent))
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

impl HasAppNav for UnifiedView {
    fn app_nav_mut(&mut self) -> &mut AppNavMenu {
        &mut self.app_nav
    }

    fn app_nav_current(&self) -> Option<AppDestination> {
        Some(AppDestination::Workbench)
    }

    fn app_nav_fallback_focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// The node tree column's fixed width, shared by the top row and the bottom
/// row's spacer so the chat drawer lines up under columns 2+ only.
const TREE_COLUMN_WIDTH: f32 = 280.;

impl Render for UnifiedView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.chat_focus(cx);
        self.chat_drawer.update(cx, |drawer, cx| drawer.set_focus(focus, cx));

        let border = cx.theme().border;
        let visible_slots = DEFAULT_VISIBLE_COLUMNS;
        let folded = self.columns.folded(visible_slots);
        let total = self.columns.len();
        let column_elements: Vec<_> = (0..total)
            .map(|ix| self.render_column(ix, folded.contains(&ix), cx).into_any_element())
            .collect();
        div()
            .id("unified-view")
            .key_context(UNIFIED_CONTEXT)
            .track_focus(&self.focus_handle)
            .capture_action(cx.listener(Self::on_open_agent_chat))
            .on_action(cx.listener(Self::toggle_pin_focused))
            .on_action(cx.listener(Self::next_waiting))
            .on_action(cx.listener(Self::prev_waiting))
            .on_action(cx.listener(Self::focus_left))
            .on_action(cx.listener(Self::focus_right))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(TREE_COLUMN_WIDTH))
                            .h_full()
                            .border_r_1()
                            .border_color(border)
                            .child(self.task_list.clone()),
                    )
                    .children(column_elements),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .w_full()
                    .flex()
                    .child(div().flex_shrink_0().w(px(TREE_COLUMN_WIDTH)))
                    .child(div().flex_1().min_w(px(220.)).child(self.chat_drawer.clone())),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::rows::fixture::Fixture;
    use gpui::{TestAppContext, VisualTestContext};
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
        crate::interview::set_data_root(config_root);
        let paths = crate::interview::TodPaths::discover().unwrap();
        let slot = Rc::new(RefCell::new(None));
        let store = fixture.store.clone();
        let agent: crate::interview::agent::SharedAgent = std::sync::Arc::new(std::sync::Mutex::new(
            Box::new(tod_agent::MockAgentProvider::new()),
        ));
        let agent_runs_for_test = cx.new(|_| AgentRuns::new(store.clone(), agent.clone()));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| UnifiedView::new(window, cx, store, paths, agent, agent_runs_for_test));
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
            assert_eq!(view.columns.focused_index(), Some(0));
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
    fn alt_q_opens_and_pins_decisions_without_touching_an_already_pinned_column(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let node_id = fixture.node_id;
        ask_decision(&fixture, node_id, "Only decision?");
        let (view, cx) = open_view(&fixture, cx);
        load_attention(&view, cx);

        // Pin an unrelated column first (Details) so Alt+Q has to route
        // around it rather than replace or unpin it.
        view.update_in(cx, |view, window, cx| {
            view.open_panel(PanelKind::Details(node_id), 0, false, window, cx);
            view.columns.set_pinned(0, true);
        });
        draw(cx);

        view.update_in(cx, |view, window, cx| {
            view.next_waiting(&UnifiedNextWaiting, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, _| {
            // The pinned Details column is untouched...
            assert!(view.columns.is_pinned(0));
            assert_eq!(view.columns.columns()[0].panel, PanelKind::Details(node_id));
            // ...and Decisions opened in a new column and was pinned, since
            // it was not shown anywhere yet.
            let decisions_ix = view
                .columns
                .columns()
                .iter()
                .position(|c| c.panel == PanelKind::Decisions)
                .expect("decisions column opened");
            assert!(view.columns.is_pinned(decisions_ix));
        });

        // A second Alt+Q press on the same lone waiting node re-targets the
        // existing (already pinned) Decisions column rather than opening
        // another one.
        let before = view.read_with(cx, |view, _| view.columns.len());
        view.update_in(cx, |view, window, cx| {
            view.next_waiting(&UnifiedNextWaiting, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.columns.len(), before);
        });
    }
}
