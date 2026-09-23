use super::always_on_top;
use super::data_root_setup::DataRootSetupView;
use super::fleet_blocked::FleetBlockedView;
use super::no_focus;
use super::right_drawer::{DrawerKind, DrawerRequest, RightDrawer};
#[cfg(feature = "agent-socket")]
use crate::agent_socket;
#[cfg(feature = "agent-socket")]
use crate::agent_socket::commands::AgentPlatformSocketCommand;
use crate::app::history_window::HistoryWindowControl;
use crate::app::interactive_agent_window::{
    InteractiveAgentOpenParams, InteractiveAgentWindowControl,
};
use crate::app::transcript_window::TranscriptWindowControl;
use crate::cli::LaunchOptions;
use crate::conversation::{ConversationView, ConversationViewEvent};
use crate::interview::agent::{AgentBackend, AgentPlatform, SharedAgent};
use crate::interview::settings::{persist_window_geometry, resolve_open_window_bounds};
use crate::interview::views::{SessionsEvent, SessionsView, SettingsEvent, SettingsView};
use crate::interview::{TaskListProceedContext, TodPaths, TodSettings};
use crate::ui::actionable::render_shortcut_pill_in_context;
use crate::ui::agent_chat::{OpenAgentChat, OpenConversation};
use crate::ui::app_nav::{
    HasAppNav, ShellGoConversation, ShellGoDatabase, ShellGoSettings, ShellGoTasks,
    register_app_nav_keyboard_bindings,
};
use crate::ui::key_context::NOT_INPUT;
use crate::ui::panel_split::{PanelSplitState, h_panel_split};
use crate::ui::selectable_text::selectable_text;
use crate::ui::status::{self, StatusSource};
use crate::ui::toast::{error_toast, notification_overlay, warning_toast};
use crate::views::action_panel::{ActionPanelEvent, ActionPanelView};
use crate::views::database::DatabaseView;
use crate::views::incoming_check::{IncomingCheck, IncomingCheckEvent};
use crate::views::lifecycle_control::LifecycleController;
use crate::views::lifecycle_panel::{LifecyclePanelEvent, LifecyclePanelView};
use crate::views::obligations::{ObligationsEvent, ObligationsView};
use crate::views::plan_steps::{PlanStepsEvent, PlanStepsView};
use crate::views::task_edit::{TaskEditEvent, TaskEditView};
use crate::views::task_list::{TaskListEvent, TaskListView};
use crate::views::visual_design_panel::{
    EmbeddedChatParams, VisualDesignPanelEvent, VisualDesignPanelView,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, IconName, Root, Selectable, StyledExt, TitleBar, h_flex};
use std::path::PathBuf;
use std::sync::Arc;
use tod_agent::EngagementState;
use tod_core::conversation::RunNotice;
use tod_core::process::{interview_phase_for_lifecycle, interview_phase_label};
use tod_core::run_transcript;
use tod_store::agent_traffic::{
    AgentStatusGroups, SharedAgentTrafficLog, format_status_bar, shared_log,
};
use tod_store::conversation::{Focus, ProtocolKind};
use tod_store::fleet::terminal::{focus_shell_session, open_shell_for_node};
use tod_store::fleet::{FleetLaunchError, FleetStore, code_editor, open_code_editor_for_node};
use uuid::Uuid;

actions!(
    shell,
    [ShellOpenAgentTranscripts, ShellOpenHistory, ShellUndo]
);

const TASKS_TREE_MIN: f32 = 240.0;
const TASKS_DRAWER_MIN: f32 = 280.0;
/// The status bar's fixed height: a compact button plus its padding.
const STATUS_BAR_HEIGHT: Pixels = px(36.);
/// The most characters of status text the bar shows before cutting it short.
const STATUS_BAR_MAX_CHARS: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellView {
    Tasks,
    Interview,
    /// The conversation view (Ctrl+J).
    Conversation,
    Settings,
    Database,
}

struct PendingOpenInterview {
    task_id: String,
    node_id: Uuid,
    lifecycle: String,
    title: String,
}

struct PendingOpenLifecycle {
    task_id: String,
    lifecycle: String,
}

pub struct Shell {
    active_view: ShellView,
    task_list: Entity<TaskListView>,
    /// Every panel shown to the right of the task tree — see `right_drawer`.
    drawer: RightDrawer,
    sessions: Entity<SessionsView>,
    conversation: Entity<ConversationView>,
    /// Where the conversation view's Back goes once its own history is empty.
    view_before_conversation: ShellView,
    settings: Entity<SettingsView>,
    database: Entity<DatabaseView>,
    fleet: Arc<FleetStore>,
    _mutation_socket: Option<tod_store::fleet::mutation_socket::PortFileGuard>,
    agent: SharedAgent,
    traffic_log: SharedAgentTrafficLog,
    transcript_window: TranscriptWindowControl,
    _interactive_agent_window: InteractiveAgentWindowControl,
    history_window: HistoryWindowControl,
    agent_status_text: SharedString,
    paths: TodPaths,
    migration_notice_dismissed: bool,
    pending_open_interview: Option<PendingOpenInterview>,
    /// (task_id, lifecycle) — from the lifecycle panel's on-demand
    /// "Open interview" affordance, validated and routed through
    /// `TaskListView::open_interview_for_task` once `window` is available.
    pending_open_interview_for_task: Option<(String, String)>,
    /// A conversation to open once `window` is available (panel events have none).
    pending_open_conversation: Option<Focus>,
    /// A node that just entered a state with on-entry work for its agent.
    pending_on_entry: Option<Uuid>,
    /// The conversation view asked to return to where the user came from.
    pending_leave_conversation: bool,
    /// The conversation's context panel asked to show a node (and maybe an
    /// obligation) in the Tasks view.
    pending_go_to_tasks: Option<(Uuid, Option<Uuid>)>,
    /// A gate check that waited on the node's incoming changes may run now.
    pending_gate_check: Option<Uuid>,
    pending_open_lifecycle: Option<PendingOpenLifecycle>,
    pending_return_to_tasks: bool,
    /// Drawer changes queued by event handlers, applied in order on render.
    pending_drawer: Vec<DrawerRequest>,
    pending_delete_selected_task: bool,
    pending_refocus_task_list: bool,
    /// A generator whose refresh stopped for want of the Linear API key.
    /// Queued by the edit panel's event, applied on render, where there is a
    /// window to open the task list's credential prompt with.
    pending_linear_credentials_for: Option<Uuid>,
    pending_error_toast: Option<String>,
    pending_warning_toast: Option<String>,
    always_on_top: bool,
    tasks_split_state: Entity<PanelSplitState>,
    _task_list_subscription: Subscription,
    _task_edit_subscription: Subscription,
    _obligations_subscription: Subscription,
    _plan_subscription: Subscription,
    _lifecycle_panel_subscription: Subscription,
    _visual_design_panel_subscription: Subscription,
    _action_panel_subscription: Subscription,
    _sessions_subscription: Subscription,
    _conversation_subscription: Subscription,
    _settings_subscription: Subscription,
    _incoming_check_subscription: Subscription,
}

/// Where "open" goes for a node in `lifecycle`: `proposed` and `design` nodes
/// are talked through in the conversation view, focused on the node; `None`
/// leaves the node to the interview (`planning`).
pub(crate) fn spec_conversation_focus(lifecycle: &str, node_id: Uuid) -> Option<Focus> {
    matches!(lifecycle, "proposed" | "design").then_some(Focus::Node(node_id))
}

/// Human-readable summary of background work that would be lost if the
/// window closed right now: agents mid-run and gate checks in flight.
fn collect_running_work(
    fleet: &FleetStore,
    _lifecycle_panel: &Entity<LifecyclePanelView>,
    sessions: &Entity<SessionsView>,
    conversation: &Entity<ConversationView>,
    cx: &App,
) -> Vec<SharedString> {
    let mut items = Vec::new();
    if let Ok(runs) = fleet.list_unended_runs() {
        for run in runs {
            if run.is_live() {
                let title = fleet
                    .get_task(&run.node_id)
                    .ok()
                    .flatten()
                    .map(|t| t.title)
                    .unwrap_or_else(|| run.node_id.clone());
                let platform_label = match run.platform.as_deref() {
                    Some("claude") => "Claude",
                    Some("cursor") => "Cursor",
                    Some("mock") => "Mock",
                    Some(other) if !other.is_empty() => other,
                    _ => "Coding",
                };
                items.push(SharedString::from(format!(
                    "{platform_label} agent running: {title}"
                )));
            }
        }
    }
    for item in sessions.read(cx).running_interview_work() {
        items.push(SharedString::from(item));
    }
    for item in conversation.read(cx).running_work() {
        items.push(SharedString::from(item));
    }
    items
}

impl Shell {
    fn select_view(&mut self, view: ShellView, window: &mut Window, cx: &mut Context<Self>) {
        self.task_list
            .update(cx, |list, _| list.app_nav_mut().close());
        self.sessions
            .update(cx, |sessions, cx| sessions.close_app_nav(cx));
        self.conversation
            .update(cx, |conversation, _| conversation.close_app_nav());
        self.settings
            .update(cx, |settings, _| settings.app_nav_mut().close());
        self.database
            .update(cx, |database, _| database.app_nav_mut().close());
        if self.active_view == view {
            if view == ShellView::Tasks {
                self.task_list.update(cx, |list, cx| {
                    list.refresh(window, cx);
                });
            }
            return;
        }
        if view == ShellView::Conversation {
            self.view_before_conversation = self.active_view;
        }
        self.active_view = view;
        crate::ui::journey::record_nav(
            cx,
            tod_journey::NavEvent::ViewSelected {
                view: format!("{view:?}"),
            },
        );
        match view {
            ShellView::Tasks => {
                self.task_list.update(cx, |list, cx| {
                    list.refresh(window, cx);
                });
                let focus = self.task_list.read(cx).focus_handle(cx);
                focus.focus(window, cx);
            }
            ShellView::Interview => {
                self.sessions.update(cx, |sessions, cx| {
                    sessions.focus(window, cx);
                });
            }
            ShellView::Conversation => {
                let focus = self.conversation.read(cx).focus_handle(cx);
                focus.focus(window, cx);
            }
            ShellView::Settings => {
                let focus = self.settings.read(cx).focus_handle(cx);
                focus.focus(window, cx);
            }
            ShellView::Database => {
                let focus = self.database.read(cx).focus_handle(cx);
                focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    fn queue_open_interview(
        &mut self,
        task_id: String,
        node_id: Uuid,
        lifecycle: String,
        title: String,
        cx: &mut Context<Self>,
    ) {
        // `proposed` and `design` nodes are talked through in the conversation
        // view (D12); `planning` keeps its interview.
        if let Some(focus) = spec_conversation_focus(&lifecycle, node_id) {
            self.queue_open_conversation(focus, cx);
            return;
        }
        crate::ui::journey::record_nav(
            cx,
            tod_journey::NavEvent::DrawerOpened {
                drawer: "interview".into(),
            },
        );
        self.active_view = ShellView::Interview;
        self.pending_open_interview = Some(PendingOpenInterview {
            task_id,
            node_id,
            lifecycle,
            title,
        });
        cx.notify();
    }

    fn drain_pending_return_to_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_return_to_tasks {
            return;
        }
        self.pending_return_to_tasks = false;
        self.select_view(ShellView::Tasks, window, cx);
    }

    fn drain_pending_open_interview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_open_interview.take() else {
            return;
        };
        let phase = interview_phase_for_lifecycle(&pending.lifecycle)
            .unwrap_or("task-requirements-interview");
        let phase_label = interview_phase_label(phase);
        self.sessions.update(cx, |sessions, cx| {
            sessions.open_or_kickoff_for_entity(
                pending.node_id,
                phase,
                &pending.title,
                phase_label,
                Some(TaskListProceedContext {
                    task_id: pending.task_id,
                    lifecycle: pending.lifecycle,
                }),
                window,
                cx,
            );
        });
    }

    fn drain_pending_open_lifecycle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_open_lifecycle.take() else {
            return;
        };
        self.task_list.update(cx, |list, cx| {
            list.open_lifecycle_panel(&pending.task_id, &pending.lifecycle, window, cx);
        });
    }

    fn drain_pending_open_interview_for_task(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((task_id, lifecycle)) = self.pending_open_interview_for_task.take() else {
            return;
        };
        // The lifecycle panel's "open" for `proposed` / `design` needs no
        // interview workspace: it opens the node's conversation (D12).
        if let Some(focus) = Uuid::parse_str(&task_id)
            .ok()
            .and_then(|node_id| spec_conversation_focus(&lifecycle, node_id))
        {
            self.open_conversation(focus, window, cx);
            return;
        }
        self.task_list.update(cx, |list, cx| {
            list.open_interview_for_task(&task_id, &lifecycle, window, cx);
        });
    }

    fn dismiss_migration_notice(&mut self, cx: &mut Context<Self>) {
        self.migration_notice_dismissed = true;
        cx.notify();
    }

    fn toggle_always_on_top(&mut self, cx: &mut Context<Self>) {
        let next = !self.always_on_top;
        if always_on_top::set(next) {
            self.always_on_top = next;
            self.persist_always_on_top(next);
            cx.notify();
        }
    }

    fn persist_always_on_top(&self, enabled: bool) {
        match TodSettings::load(&self.paths) {
            Ok(mut settings) => {
                settings.always_on_top = enabled;
                if let Err(err) = settings.save(&self.paths) {
                    tracing::error!("failed to save always_on_top setting: {err:#}");
                }
            }
            Err(err) => {
                tracing::error!("failed to load settings for always_on_top save: {err:#}");
            }
        }
    }

    fn compute_status_groups(&self) -> AgentStatusGroups {
        let mut groups = AgentStatusGroups::default();
        if let Ok(registry) = self._interactive_agent_window.engagement().lock() {
            groups.fleet.total = registry.len() as u32;
            groups.fleet.processing = registry
                .values()
                .filter(|state| matches!(state, EngagementState::WaitingOnAgent))
                .count() as u32;
            groups.fleet.blocked = registry
                .values()
                .filter(|state| {
                    matches!(
                        state,
                        EngagementState::WaitingOnUser | EngagementState::WaitingOnOther(_)
                    )
                })
                .count() as u32;
        }
        if let Ok(provider) = self.agent.lock() {
            groups.interview = provider.interview_status_counts();
        }
        if let Ok(log) = self.traffic_log.lock() {
            groups.traffic_entries = log.entries().len();
        }
        groups
    }

    fn refresh_agent_status(&mut self, cx: &mut Context<Self>) {
        let text = format_status_bar(&self.compute_status_groups());
        if self.agent_status_text.as_ref() != text {
            self.agent_status_text = text.into();
            cx.notify();
        }
    }

    fn replace_agent_platform(&mut self, platform: AgentPlatform, cx: &mut Context<Self>) {
        // RoutingAgentProvider already hosts both Cursor and Claude; settings persist the
        // preferred interview platform without swapping the shared provider mutex.
        tracing::info!(
            event = "agent",
            action = "platform_settings_updated",
            platform = platform.label(),
            "interview agent platform setting updated (routing provider unchanged)"
        );
        self.refresh_agent_status(cx);
    }

    #[cfg(feature = "agent-socket")]
    pub fn handle_agent_platform_socket(
        &mut self,
        action: AgentPlatformSocketCommand,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        match action {
            AgentPlatformSocketCommand::Get => {
                let platform = self.settings.read(cx).agent_platform();
                Ok(format!("ok {}", platform_label(platform)))
            }
            AgentPlatformSocketCommand::Cycle => {
                self.settings.update(cx, |settings, cx| {
                    settings.cycle_agent_platform(1, cx);
                });
                let platform = self.settings.read(cx).agent_platform();
                Ok(format!("ok {}", platform_label(platform)))
            }
            AgentPlatformSocketCommand::Set(raw) => {
                let platform = parse_agent_platform(&raw)?;
                self.settings.update(cx, |settings, cx| {
                    settings.set_agent_platform(platform, cx);
                });
                Ok(format!("ok {}", platform_label(platform)))
            }
        }
    }

    fn queue_drawer(&mut self, request: DrawerRequest, cx: &mut Context<Self>) {
        self.pending_drawer.push(request);
        cx.notify();
    }

    /// A drawer panel closed. If that left the drawer empty — rather than the
    /// panel being swapped out for another — hand focus back to the tree.
    fn on_drawer_panel_closed(&mut self, cx: &mut Context<Self>) {
        if self.drawer.is_open(cx) {
            return;
        }
        self.pending_refocus_task_list = true;
        cx.notify();
    }

    fn drain_pending_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for request in std::mem::take(&mut self.pending_drawer) {
            self.apply_drawer_request(request, window, cx);
        }
        let open = self.drawer.is_open(cx);
        self.task_list
            .update(cx, |list, cx| list.set_drawer_open(open, cx));
    }

    /// Explicit opens close the other panels and take keyboard focus;
    /// following the selection does neither (see `RightDrawer::follow`).
    fn apply_drawer_request(
        &mut self,
        request: DrawerRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &request {
            DrawerRequest::Close | DrawerRequest::Follow { task_id: None } => {
                crate::ui::journey::record_nav(
                    cx,
                    tod_journey::NavEvent::DrawerClosed {
                        drawer: "drawer".into(),
                    },
                );
            }
            DrawerRequest::Follow { task_id: Some(_) } | DrawerRequest::Focus => {}
            other => {
                crate::ui::journey::record_nav(
                    cx,
                    tod_journey::NavEvent::DrawerOpened {
                        drawer: format!("{other:?}")
                            .split(|c: char| c == ' ' || c == '{')
                            .next()
                            .unwrap_or("drawer")
                            .to_string(),
                    },
                );
            }
        }
        match request {
            DrawerRequest::OpenTaskEdit { task_id } => {
                self.drawer
                    .close_except(Some(DrawerKind::TaskEdit), window, cx);
                self.drawer
                    .task_edit
                    .update(cx, |edit, cx| edit.open(&task_id, window, cx));
                if !self.drawer.task_edit.read(cx).is_open() {
                    self.task_list.update(cx, |list, cx| {
                        list.show_error("Could not open node for editing", window, cx);
                    });
                }
            }
            DrawerRequest::OpenObligations { task_id, title } => {
                if let Ok(node_id) = Uuid::parse_str(&task_id) {
                    self.drawer
                        .close_except(Some(DrawerKind::Obligations), window, cx);
                    self.drawer.obligations.update(cx, |panel, cx| {
                        panel.open(node_id, &title, None, window, cx);
                    });
                }
            }
            DrawerRequest::OpenPlan { task_id, title } => {
                if let Ok(node_id) = Uuid::parse_str(&task_id) {
                    self.drawer.close_except(Some(DrawerKind::Plan), window, cx);
                    self.drawer.plan.update(cx, |panel, cx| {
                        panel.open(node_id, &title, window, cx);
                    });
                }
            }
            DrawerRequest::OpenLifecycle { task_id } => {
                self.drawer
                    .close_except(Some(DrawerKind::Lifecycle), window, cx);
                self.drawer.lifecycle.update(cx, |panel, cx| {
                    if panel.is_open() {
                        panel.retarget(&task_id, cx);
                    } else {
                        panel.open(&task_id, window, cx);
                    }
                });
                if self.drawer.lifecycle.read(cx).is_open() {
                    self.drawer.focus(window, cx);
                } else {
                    self.task_list.update(cx, |list, cx| {
                        list.show_error("Could not open lifecycle panel", window, cx);
                    });
                }
            }
            DrawerRequest::OpenVisualDesign {
                node_id,
                obligation_id,
            } => {
                self.open_visual_design_panel(node_id, obligation_id, window, cx);
            }
            DrawerRequest::OpenActionPanel { task_id } => {
                self.drawer
                    .close_except(Some(DrawerKind::Action), window, cx);
                self.drawer
                    .action
                    .update(cx, |panel, cx| panel.open(&task_id, window, cx));
                if !self.drawer.action.read(cx).is_open() {
                    self.task_list.update(cx, |list, cx| {
                        list.show_error("Could not open the Action panel", window, cx);
                    });
                }
            }
            DrawerRequest::Follow {
                task_id: Some(task_id),
            } => {
                self.drawer.follow(&task_id, &self.fleet, window, cx);
            }
            DrawerRequest::Follow { task_id: None } | DrawerRequest::Close => {
                self.drawer.close_except(None, window, cx);
            }
            DrawerRequest::Focus => {
                self.drawer.focus(window, cx);
            }
        }
        cx.notify();
    }

    fn open_transcript_window(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = self.transcript_window.open_or_focus(cx) {
            tracing::error!("failed to open agent transcript window: {err}");
        }
    }

    fn open_history_window(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = self.history_window.open_or_focus(cx) {
            tracing::error!("failed to open history window: {err}");
        }
    }

    /// Show the conversation view on `focus`: its latest conversation, or a
    /// new, unsaved one.
    fn open_conversation(&mut self, focus: Focus, window: &mut Window, cx: &mut Context<Self>) {
        self.open_conversation_with(focus, ProtocolKind::Outline, false, window, cx);
    }

    fn open_conversation_with(
        &mut self,
        focus: Focus,
        protocol: ProtocolKind,
        start: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_view(ShellView::Conversation, window, cx);
        self.conversation.update(cx, |conversation, cx| {
            if start {
                conversation.run(focus, protocol, window, cx);
            } else {
                conversation.open_with(focus, protocol, true, window, cx);
            }
        });
        if let Some(conversation_id) = self.conversation.read(cx).conversation_id() {
            crate::ui::journey::record_conversation_opened(cx, conversation_id);
        }
        cx.notify();
    }

    /// [`Self::open_conversation`] from an event handler, which has no
    /// `window` and may run while nested entity leases are still on the stack.
    fn queue_open_conversation(&mut self, focus: Focus, cx: &mut Context<Self>) {
        self.pending_open_conversation = Some(focus);
        cx.notify();
    }

    fn drain_pending_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = self.pending_open_conversation.take() {
            self.open_conversation(focus, window, cx);
        }
        if let Some(node) = self.pending_on_entry.take() {
            self.open_conversation_with(Focus::Node(node), ProtocolKind::OnEntry, true, window, cx);
        }
        if std::mem::take(&mut self.pending_leave_conversation) {
            let view = self.view_before_conversation;
            self.select_view(view, window, cx);
        }
        if let Some(node) = self.pending_gate_check.take() {
            self.select_view(ShellView::Conversation, window, cx);
            self.conversation.update(cx, |conversation, cx| {
                conversation.check_gate(node, window, cx)
            });
        }
        if let Some((node_id, obligation_id)) = self.pending_go_to_tasks.take() {
            self.go_to_tasks(node_id, obligation_id, window, cx);
        }
    }

    /// Show `node_id` in the Tasks view with its obligations drawer open,
    /// highlighting `obligation_id` when given. Tasks has no plan drawer, so
    /// plan-step targets land on the node's obligations too.
    fn go_to_tasks(
        &mut self,
        node_id: Uuid,
        obligation_id: Option<Uuid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task_id = node_id.to_string();
        let Some(title) = self
            .fleet
            .get_node(&task_id)
            .ok()
            .flatten()
            .map(|n| n.title)
        else {
            return;
        };
        self.select_view(ShellView::Tasks, window, cx);
        self.task_list
            .update(cx, |list, cx| list.reveal_node(&task_id, window, cx));
        self.apply_drawer_request(
            DrawerRequest::OpenObligations { task_id, title },
            window,
            cx,
        );
        if let Some(id) = obligation_id {
            self.drawer
                .obligations
                .update(cx, |panel, cx| panel.highlight_item(id, window, cx));
        }
        cx.notify();
    }

    /// Ctrl+J that no view handled: the task tree's selection, else the
    /// whole project.
    fn on_open_agent_chat(
        &mut self,
        _: &OpenAgentChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = fallback_focus(self.task_list.read(cx).selected_node_id());
        self.open_conversation(focus, window, cx);
    }

    /// Show `message` as an error banner on the next render; messages that
    /// arrive before then are shown together.
    fn queue_error_toast(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        let message = message.into();
        self.pending_error_toast = Some(match self.pending_error_toast.take() {
            Some(earlier) => format!(
                "{earlier}

{message}"
            ),
            None => message,
        });
        cx.notify();
    }

    /// Like [`Self::queue_error_toast`], for something the user should know
    /// but that is not a failure.
    fn queue_warning_toast(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        let message = message.into();
        self.pending_warning_toast = Some(match self.pending_warning_toast.take() {
            Some(earlier) => format!(
                "{earlier}

{message}"
            ),
            None => message,
        });
        cx.notify();
    }

    fn drain_pending_error_toast(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(message) = self.pending_warning_toast.take() {
            warning_toast(window, cx, message);
        }
        if let Some(message) = self.pending_error_toast.take() {
            error_toast(window, cx, message);
        }
    }

    fn handle_open_shell(
        &mut self,
        task_id: String,
        shell_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        // Off the UI thread: opening waits for the terminal's shell to start,
        // and for Docker when the node runs in a dev container.
        let fleet = self.fleet.clone();
        let paths = self.paths.clone();
        cx.spawn(async move |this, cx| {
            let result: anyhow::Result<String> = cx
                .background_spawn(async move {
                    let settings = TodSettings::load(&paths).unwrap_or_default();
                    if let Some(shell_id) = shell_id {
                        let shell = fleet
                            .get_shell(&shell_id)?
                            .ok_or_else(|| anyhow::anyhow!("shell session not found"))?;
                        let cwd = focus_shell_session(&fleet, &paths, &settings, &shell)?;
                        return Ok(format!("Focused shell in {cwd}"));
                    }
                    let (_, cwd) = open_shell_for_node(&fleet, &paths, &settings, &task_id, None)?;
                    Ok(format!("Opened terminal in {cwd}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(msg) => {
                        let _ = this.fleet.reload_if_stale();
                        this.task_list.update(cx, |list, cx| {
                            list.set_status_message(msg, cx);
                            list.request_live_refresh(cx);
                        });
                    }
                    Err(err) => {
                        this.queue_error_toast(format!("Shell failed: {err:#}"), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A — open the node's most recent chat session, or start a new one.
    fn handle_launch_or_focus_agent(&mut self, task_id: String, cx: &mut Context<Self>) {
        let _ = self.fleet.reload_if_stale();
        let latest = self
            .fleet
            .list_interactive_sessions_for_node(&task_id)
            .unwrap_or_default()
            .into_iter()
            .next();
        let result = match latest {
            Some(run) => self._interactive_agent_window.open_session(
                InteractiveAgentOpenParams {
                    node_id: task_id.clone(),
                    session_run_id: run.id,
                    initial_context: None,
                    auto_submit_message: None,
                },
                cx,
            ),
            None => self
                ._interactive_agent_window
                .create_and_open_session(&task_id, None, None, cx)
                .map(|_| ()),
        };
        match result {
            Ok(()) => {
                let _ = self.fleet.reload_if_stale();
                self.task_list.update(cx, |list, cx| {
                    list.set_status_message("Opened agent chat".to_string(), cx);
                    list.request_live_refresh(cx);
                });
            }
            Err(err) => {
                self.queue_error_toast(format!("Agent chat failed: {err}"), cx);
            }
        }
        cx.notify();
    }

    fn handle_open_code_editor(
        &mut self,
        task_id: String,
        editor_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = code_editor(&editor_id) else {
            self.queue_error_toast(format!("Unknown code editor: {editor_id}"), cx);
            return;
        };
        match open_code_editor_for_node(&self.fleet, editor, &task_id) {
            Ok(cwd) => {
                self.task_list.update(cx, |list, cx| {
                    list.set_status_message(
                        format!("Opened {} in {}", editor.label(), cwd.display()),
                        cx,
                    );
                });
            }
            Err(err) => {
                self.queue_error_toast(format!("Open code failed: {err:#}"), cx);
            }
        }
        cx.notify();
    }

    fn undo_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.fleet.undo_last() {
            Ok(Some(label)) => {
                self.task_list.update(cx, |list, cx| {
                    list.set_status_message(format!("Undid: {label}"), cx);
                    list.refresh(window, cx);
                });
                if self.drawer.task_edit.read(cx).is_open() {
                    self.drawer.task_edit.update(cx, |edit, cx| {
                        if let Some(id) = edit.open_task_id(cx) {
                            edit.retarget(&id, window, cx);
                        }
                    });
                }
                if self.drawer.obligations.read(cx).is_open() {
                    self.drawer.obligations.update(cx, |panel, cx| {
                        panel.reload(window, cx);
                    });
                }
                if self.drawer.plan.read(cx).is_open() {
                    self.drawer.plan.update(cx, |panel, cx| {
                        panel.reload(window, cx);
                    });
                }
            }
            Ok(None) => {
                self.task_list.update(cx, |list, cx| {
                    list.set_status_message("Nothing to undo".into(), cx);
                });
            }
            Err(err) => {
                error_toast(window, cx, format!("Undo failed: {err}"));
            }
        }
    }

    /// The status bar's message: what the active view last posted to the
    /// status hub (`ui::status`); views that post nothing show none. The bar
    /// is one line, so a longer post (an error chain, say — its toast keeps
    /// the full text) shows only its first line, cut short.
    fn status_bar_message(&self, cx: &App) -> SharedString {
        let source = match self.active_view {
            ShellView::Tasks => StatusSource::Tasks,
            ShellView::Conversation => StatusSource::Conversation,
            ShellView::Interview | ShellView::Settings | ShellView::Database => {
                return SharedString::default();
            }
        };
        status::current(cx, source)
            .map(|text| tod_agent::util::one_line_summary(&text, STATUS_BAR_MAX_CHARS).into())
            .unwrap_or_default()
    }

    fn render_status_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let status = self.status_bar_message(cx);
        // A fixed height: the bar never resizes to fit what it shows.
        h_flex()
            .w_full()
            .h(STATUS_BAR_HEIGHT)
            .flex_shrink_0()
            .overflow_hidden()
            .px_4()
            .border_t_1()
            .border_color(border)
            .justify_between()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .max_h(STATUS_BAR_HEIGHT)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .when(!status.is_empty(), |el| {
                        el.child(
                            selectable_text("shell-status", status, window, cx)
                                .text_xs()
                                .text_color(muted),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .flex_shrink_0()
                    .child(
                        Button::new("open-agent-transcripts")
                            .outline()
                            .compact()
                            .label(self.agent_status_text.clone())
                            .tooltip(
                                "Open agent transcripts (requests and responses grouped by agent type)",
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_transcript_window(cx);
                            })),
                    )
                    .when_some(
                        render_shortcut_pill_in_context(
                            window,
                            &ShellOpenAgentTranscripts,
                            None,
                            cx,
                        ),
                        |el, pill| el.child(pill),
                    ),
            )
    }

    fn render_title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child("tod")
                .child(div().flex_1())
                // The shortcut pill sits beside the button, not under it as
                // `chrome_control_with_shortcut_in_context` puts it: the
                // title bar has no room below.
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap(crate::ui::style::space::INLINE)
                        .child(
                            Button::new("title-talk")
                                .icon(gpui_component::Icon::new(
                                    gpui_kit_assets::IconName::MessagesSquare,
                                ))
                                .label("Talk about the selection")
                                .ghost()
                                .compact()
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(OpenAgentChat), cx);
                                }),
                        )
                        .children(render_shortcut_pill_in_context(
                            window,
                            &OpenAgentChat,
                            None,
                            cx,
                        )),
                )
                .when(always_on_top::is_supported(), |bar| {
                    bar.child(
                        Button::new("always-on-top")
                            .ghost()
                            .compact()
                            .selected(self.always_on_top)
                            .icon(if self.always_on_top {
                                IconName::Star
                            } else {
                                IconName::StarOff
                            })
                            .tooltip(if self.always_on_top {
                                "Unpin window"
                            } else {
                                "Always on top"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_always_on_top(cx);
                            })),
                    )
                }),
        )
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_pending_open_interview(window, cx);
        self.drain_pending_open_interview_for_task(window, cx);
        self.drain_pending_conversation(window, cx);
        self.drain_pending_return_to_tasks(window, cx);
        self.drain_pending_open_lifecycle(window, cx);
        self.drain_pending_drawer(window, cx);
        self.drain_pending_task_list(window, cx);
        self.drain_pending_error_toast(window, cx);
        crate::ui::agent_permission::drain_queued_requests(window, cx);

        div()
            .v_flex()
            .size_full()
            .relative()
            .on_action(cx.listener(|this, _: &ShellGoTasks, window, cx| {
                this.select_view(ShellView::Tasks, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShellGoConversation, window, cx| {
                this.open_conversation(Focus::Project, window, cx);
            }))
            .on_action(cx.listener(Self::on_open_agent_chat))
            .on_action(cx.listener(|this, action: &OpenConversation, window, cx| {
                this.open_conversation_with(
                    action.focus,
                    action.protocol,
                    action.start,
                    window,
                    cx,
                );
            }))
            .on_action(cx.listener(|this, _: &ShellGoSettings, window, cx| {
                this.select_view(ShellView::Settings, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShellGoDatabase, window, cx| {
                this.select_view(ShellView::Database, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShellOpenAgentTranscripts, _, cx| {
                this.open_transcript_window(cx);
            }))
            .on_action(cx.listener(|this, _: &ShellOpenHistory, _, cx| {
                this.open_history_window(cx);
            }))
            .on_action(cx.listener(|this, _: &ShellUndo, window, cx| {
                this.undo_last(window, cx);
            }))
            .child(self.render_title_bar(window, cx))
            .child(self.render_migration_notice(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .w_full()
                    .child(self.render_content(window, cx)),
            )
            .child(self.render_status_bar(window, cx))
            // Without this layer `window.open_dialog` queues a dialog nobody
            // sees — the agent permission prompt among them.
            .when_some(Root::render_dialog_layer(window, cx), |el, layer| {
                el.child(layer)
            })
            .when_some(notification_overlay(window, cx), |el, layer| {
                el.child(layer)
            })
    }
}

impl Shell {
    fn render_migration_notice(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.migration_notice_dismissed || !self.fleet.migration_in_progress() {
            return div().into_any_element();
        }
        let border = cx.theme().border;
        div()
            .v_flex()
            .gap_2()
            .px_4()
            .py_2()
            .bg(gpui::yellow())
            .text_color(gpui::black())
            .border_b_1()
            .border_color(border)
            .child("Storage-root migration is in progress. Fleet mutations remain blocked.")
            .child(
                gpui_component::button::Button::new("migration-notice-dismiss")
                    .label("Dismiss")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.dismiss_migration_notice(cx);
                    })),
            )
            .into_any_element()
    }

    fn render_content(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.active_view {
            ShellView::Tasks => self.render_tasks_split(cx).into_any_element(),
            ShellView::Interview => self.sessions.clone().into_any_element(),
            ShellView::Conversation => self.conversation.clone().into_any_element(),
            ShellView::Settings => self.settings.clone().into_any_element(),
            ShellView::Database => self.database.clone().into_any_element(),
        }
    }

    /// Tasks always use a left tree + right drawer host. Whichever drawer
    /// panel is open shows the tree's selected node; the tree stays.
    fn render_tasks_split(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let drawer = if let Some(panel) = self.drawer.element(cx) {
            panel
        } else {
            div()
                .size_full()
                .v_flex()
                .items_center()
                .justify_center()
                .gap_2()
                .bg(theme.background)
                .text_color(muted)
                .child(div().text_sm().font_semibold().child("Actions"))
                .child(
                    div()
                        .text_xs()
                        .child("A agent chat · T shell · C code editor · F actions"),
                )
                .into_any_element()
        };

        h_panel_split("tasks-split", &self.tasks_split_state)
            .min_left(px(TASKS_TREE_MIN))
            .min_right(px(TASKS_DRAWER_MIN))
            .left(
                div()
                    .id("tasks-tree-pane")
                    .size_full()
                    .child(self.task_list.clone()),
            )
            .right(div().id("tasks-right-drawer").size_full().child(drawer))
    }

    /// Open the visual design panel (mockup `WebView` + embedded agent chat)
    /// for one design-phase obligation, from the "Design"/"+ Design"
    /// affordance on its row in the Obligations panel.
    ///
    /// Constructs the chat in-process via
    /// `InteractiveAgentWindowControl::create_embedded_session` instead of
    /// opening a standalone window, so it can sit inside the panel next to the
    /// mockup preview.
    fn open_visual_design_panel(
        &mut self,
        node_id: uuid::Uuid,
        obligation_id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task_id = node_id.to_string();
        let Some(obligation) = self.fleet.get_obligation(obligation_id).ok().flatten() else {
            return;
        };
        let (fleet, agent, workspace_cwd, settings, session_run_id) = match self
            ._interactive_agent_window
            .create_embedded_session(&task_id, Some("design/visual-design"))
        {
            Ok(session) => session,
            Err(err) => {
                self.queue_error_toast(err, cx);
                return;
            }
        };
        let initial_context = match self.build_visual_design_agent_context(node_id, obligation_id) {
            Ok(text) => Some(text),
            Err(err) => {
                tracing::warn!(
                    event = "agent_chat",
                    action = "context_unavailable",
                    error = %err,
                    "opening visual design chat without app context"
                );
                None
            }
        };

        let title = obligation
            .body
            .lines()
            .next()
            .filter(|line| !line.is_empty())
            .unwrap_or("Visual design")
            .to_string();

        self.drawer
            .close_except(Some(DrawerKind::VisualDesign), window, cx);
        self.drawer.visual_design.update(cx, |panel, cx| {
            panel.open(
                node_id,
                obligation_id,
                &title,
                EmbeddedChatParams {
                    node_id: task_id.clone(),
                    session_run_id,
                    fleet,
                    agent,
                    workspace_cwd,
                    settings,
                    window_control: self._interactive_agent_window.clone(),
                    initial_context,
                },
                window,
                cx,
            );
        });
    }

    /// Assemble the visual-design agent's first message: bundled context docs
    /// plus the live obligation selection.
    fn build_visual_design_agent_context(
        &self,
        node_id: uuid::Uuid,
        obligation_id: uuid::Uuid,
    ) -> anyhow::Result<String> {
        use tod_core::agent_context::{
            ContextRequest, NodeSelection, ObligationSelection, build_first_message,
        };
        use tod_core::media::MediaPaths;

        let media = MediaPaths::discover()?;
        let node = self
            .fleet
            .get_node(&node_id.to_string())
            .ok()
            .flatten()
            .ok_or_else(|| anyhow::anyhow!("node {node_id} not found"))?;
        let body = self
            .fleet
            .get_extra_content(node_id, tod_store::outline::EXTRA_CONTENT_DETAILS)
            .ok()
            .flatten();
        let obligation = self
            .fleet
            .get_obligation(obligation_id)
            .ok()
            .flatten()
            .ok_or_else(|| anyhow::anyhow!("obligation {obligation_id} not found"))?;
        let ancestor_context = self
            .fleet
            .read(|conn| {
                tod_core::node_context::render_inherited_context(
                    conn,
                    &tod_store::outline::repos::NodeRepo::new(conn),
                    node_id,
                    None,
                )
            })
            .unwrap_or_default();

        build_first_message(
            &media,
            &ContextRequest {
                recipe: tod_core::agent_context::VISUAL_DESIGN_RECIPE,
                data_root: self.paths.data_root(),
                node: NodeSelection {
                    id: node_id,
                    slug: Some(node.slug.clone()),
                    title: node.title,
                    body,
                    lifecycle: Some(node.lifecycle),
                },
                obligation: Some(ObligationSelection {
                    id: obligation.id,
                    kind: obligation.kind,
                    body: obligation.body,
                    visual_design_path: obligation.visual_design_path,
                }),
                ancestor_context,
            },
        )
    }

    fn drain_pending_task_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_refocus_task_list {
            self.pending_refocus_task_list = false;
            self.task_list.update(cx, |list, cx| {
                list.restore_focus(window, cx);
            });
        }
        if self.pending_delete_selected_task {
            self.pending_delete_selected_task = false;
            self.task_list.update(cx, |list, cx| {
                list.delete_selected_task(window, cx);
            });
        }
        if let Some(node_id) = self.pending_linear_credentials_for.take() {
            self.task_list.update(cx, |list, cx| {
                list.prompt_linear_credentials_for_generator(node_id, window, cx);
            });
        }
    }
}

/// The conversation focus for an obligations-panel selection.
fn obligation_focus(node_id: Uuid, obligation_id: Option<Uuid>) -> Focus {
    match obligation_id {
        Some(id) => Focus::Obligation { node: node_id, id },
        None => Focus::Node(node_id),
    }
}

/// What Ctrl+J talks about when no view claimed it: the task tree's
/// selected node, else the whole project.
pub(crate) fn fallback_focus(selected_node: Option<Uuid>) -> Focus {
    selected_node.map_or(Focus::Project, Focus::Node)
}

#[cfg(feature = "agent-socket")]
fn platform_label(platform: AgentPlatform) -> &'static str {
    match platform {
        AgentPlatform::Cursor => "cursor",
        AgentPlatform::Claude => "claude",
    }
}

#[cfg(feature = "agent-socket")]
fn parse_agent_platform(raw: &str) -> Result<AgentPlatform, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "cursor" => Ok(AgentPlatform::Cursor),
        "claude" | "anthropic" => Ok(AgentPlatform::Claude),
        other => Err(format!(
            "unknown agent platform `{other}` (expected cursor|claude)"
        )),
    }
}

fn resolve_fleet_root() -> Result<PathBuf, anyhow::Error> {
    let paths = TodPaths::discover()?;
    let settings = TodSettings::load(&paths)?;
    settings.resolve_fleet_storage_root(&paths)
}

fn open_fleet_store(
    traffic_log: SharedAgentTrafficLog,
) -> Result<Arc<FleetStore>, (FleetLaunchError, PathBuf)> {
    let root = resolve_fleet_root()
        .map_err(|err| (FleetLaunchError::Other(err), PathBuf::from("<unresolved>")))?;
    // Skip launch-time reattach here: it walks every agent/shell with a recorded reconnect
    // identity and probes whether its process is still alive, which does not need to finish
    // before the window can be shown. `run_launch_hooks` runs afterward on a background
    // thread (see the `open()` caller below) so it can never delay first paint.
    let mut store = FleetStore::open_without_reattach(&root).map_err(|err| (err, root.clone()))?;
    store.set_traffic_log(traffic_log);
    Ok(Arc::new(store))
}

/// The banner for a transcript format the reader does not know.
fn transcript_format_message(notice: &run_transcript::FormatNotice) -> String {
    let problems = notice
        .problems
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "{}'s transcript format has changed: {problems}. Transcripts were read as far as possible; the reader needs updating.",
        notice.platform.label()
    )
}

pub fn open(cx: &mut AsyncApp, opts: LaunchOptions) -> Result<()> {
    #[cfg(feature = "agent-socket")]
    let socket_addr = opts.agent_socket;
    let width = opts.width;
    let height = opts.height;
    let no_focus = opts.no_focus;
    let traffic_log = shared_log();
    let paths = TodPaths::discover()?;
    let app_settings = TodSettings::load(&paths).unwrap_or_default();
    let agent_backend = if opts.agent_backend_from_cli {
        opts.agent_backend
    } else {
        AgentBackend::from_platform(app_settings.agent_platform)
    };
    let agent: SharedAgent = agent_backend.create(traffic_log.clone());

    let fleet_open = open_fleet_store(traffic_log.clone());
    let restore_always_on_top = app_settings.always_on_top;
    let window_bounds = resolve_open_window_bounds(
        &app_settings,
        width,
        height,
        opts.width_from_cli,
        opts.height_from_cli,
    );

    #[cfg(windows)]
    let previous_foreground = if no_focus {
        no_focus::foreground_hwnd()
    } else {
        None
    };

    let transcript_window = TranscriptWindowControl::new();
    let interactive_agent_window = InteractiveAgentWindowControl::new();
    let history_window = HistoryWindowControl::new();
    #[cfg(feature = "agent-socket")]
    let transcript_for_socket = transcript_window.clone();

    #[cfg(feature = "agent-socket")]
    let socket_listener = if let Some(addr) = socket_addr {
        Some((agent_socket::bind(addr)?, addr))
    } else {
        None
    };

    #[cfg(feature = "agent-socket")]
    let shell_for_socket =
        std::sync::Arc::new(std::sync::Mutex::new(None::<gpui::WeakEntity<Shell>>));

    // Only the agent socket consumes the handle; the window itself lives on regardless.
    #[cfg_attr(not(feature = "agent-socket"), allow(unused_variables))]
    let handle = cx.open_window(
        WindowOptions {
            titlebar: Some(TitleBar::title_bar_options()),
            window_bounds: Some(window_bounds),
            is_resizable: {
                #[cfg(feature = "agent-socket")]
                {
                    socket_addr.is_none()
                }
                #[cfg(not(feature = "agent-socket"))]
                {
                    true
                }
            },
            focus: !no_focus,
            ..Default::default()
        },
        {
            let paths = paths.clone();
            let transcript_window = transcript_window.clone();
            let interactive_agent_window = interactive_agent_window.clone();
            let history_window = history_window.clone();
            #[cfg(feature = "agent-socket")]
            let shell_for_socket = shell_for_socket.clone();
            move |window, cx| {
                let paths_for_geometry = paths.clone();
                let transcript_for_close = transcript_window.clone();
                let history_for_close = history_window.clone();
                let interactive_for_close = interactive_agent_window.clone();
                match fleet_open {
                    Err((error, resolved_root)) => {
                        window.on_window_should_close(cx, move |window, cx| {
                            persist_window_geometry(window, &paths_for_geometry);
                            let _ = transcript_for_close.close(cx);
                            history_for_close.close(cx);
                            interactive_for_close.close_all(cx);
                            true
                        });
                        let view =
                            cx.new(|cx| FleetBlockedView::new(error, resolved_root, window, cx));
                        cx.new(|cx| Root::new(view, window, cx))
                    }
                    Ok(fleet) => {
                        // No turn is in flight yet, so any conversation still
                        // waiting on its agent was cut off when the app last
                        // stopped. Before the window opens, so no new turn is
                        // mistaken for one.
                        if let Err(err) = fleet.interview(
                            tod_store::interview::ACTOR_USER,
                            tod_store::interview::InterviewCommand::CloseInterruptedConversationTurns {
                                body: "Interrupted: the app stopped before the agent replied"
                                    .to_string(),
                            },
                        ) {
                            tracing::error!("closing interrupted conversation turns failed: {err:#}");
                        }
                        // Every agent session is recorded as soon as the agent
                        // reports its id, so the transcripts window can read
                        // its transcript later, whatever started it.
                        let observer_fleet = fleet.clone();
                        if let Ok(mut provider) = agent.lock() {
                        provider.set_session_observer(Arc::new(move |started| {
                            let session = tod_store::fleet::NewAgentSession {
                                agent_session_id: started.agent_session_id,
                                platform: started.platform.label().to_string(),
                                session_key: started.key,
                                title: started.title,
                                cwd: started.cwd.display().to_string(),
                            };
                            if let Err(err) = observer_fleet
                                .enqueue(tod_store::fleet::FleetMutation::RecordAgentSession(session))
                            {
                                tracing::warn!("recording agent session failed: {err}");
                            }
                        }));
                        }
                        // Reconcile stale agent/shell runtime status off the main thread. This
                        // probes OS process liveness for every reconnect-tracked row, which is
                        // unbounded work (and, on Windows, previously shelled out to
                        // powershell.exe per row) — none of it needs to finish before the window
                        // is visible, so it must never sit on the path to `cx.open_window`.
                        let reattach_fleet = fleet.clone();
                        let (format_tx, format_rx) =
                            async_channel::unbounded::<run_transcript::FormatNotice>();
                        std::thread::spawn(move || {
                            if let Err(err) = reattach_fleet
                                .run_launch_hooks(&tod_store::fleet::NoopGuestLiveness)
                            {
                                tracing::error!("background launch-time reattach failed: {err:#}");
                            }
                            // Started once reattach has settled which runs are
                            // still live, so none of them is read mid-run.
                            run_transcript::spawn_capture(reattach_fleet, move |notice| {
                                let _ = format_tx.send_blocking(notice);
                            });
                        });
                        // Only the one long-lived GUI process should run this listener, so it
                        // starts here rather than inside `FleetStore::open` (which `tod-cli`
                        // also calls, as a one-shot process, when no GUI instance is running).
                        let mutation_socket = tod_store::fleet::mutation_socket::start(
                            fleet.clone(),
                            fleet.paths().root(),
                        )
                        .inspect_err(|err| {
                            tracing::error!("mutation socket failed to start: {err:#}");
                        })
                        .ok();
                        if agent_backend == AgentBackend::Mock {
                            // Mock interview agents write through the socket just started,
                            // the same path `tod-cli` gives real agents.
                            tod_core::interview::mock::install_mock_interview_handler(
                                fleet.paths().root().to_path_buf(),
                            );
                        }
                        // The journey recorder and change-feed thread start with the
                        // store (never for a data root that failed to open above),
                        // and stop with the app: no explicit shutdown, matching the
                        // other background threads started here.
                        tod_core::journey::start(
                            fleet.paths().root().join("journeys"),
                            fleet.clone(),
                            app_settings.journeys.clone(),
                        );
                        transcript_window.bind(fleet.clone(), traffic_log.clone());
                        history_window.bind(fleet.clone());
                        let app_settings = TodSettings::load(&paths).unwrap_or_default();
                        interactive_agent_window.bind(
                            fleet.clone(),
                            agent.clone(),
                            paths.clone(),
                            app_settings,
                        );
                        let _ = crate::interview::bootstrap(fleet.clone());
                        // Before the tree is built, so no row is ever drawn as
                        // "refreshing…" for a refresh the last run never finished.
                        if let Err(err) = tod_core::generator::clear_interrupted_refreshes(&fleet) {
                            tracing::error!("clearing interrupted generator refreshes failed: {err}");
                        }
                        let task_list = cx.new(|cx| TaskListView::new(window, cx, fleet.clone()));
                        let task_edit = cx
                            .new(|cx| TaskEditView::new(window, cx, fleet.clone(), paths.clone()));
                        let obligations =
                            cx.new(|cx| ObligationsView::new(window, cx, fleet.clone()));
                        let plan = cx.new(|cx| PlanStepsView::new(window, cx, fleet.clone()));
                        let lifecycle = cx.new(|_| LifecycleController::new(fleet.clone()));
                        let incoming_check = cx.new(|_| {
                            IncomingCheck::new(fleet.clone(), agent.clone(), lifecycle.clone())
                        });
                        task_list.update(cx, |list, cx| {
                            list.bind_incoming_check(incoming_check.clone(), cx)
                        });
                        let lifecycle_panel = cx.new(|cx| {
                            LifecyclePanelView::new(
                                cx,
                                fleet.clone(),
                                lifecycle.clone(),
                                incoming_check.clone(),
                            )
                        });
                        let visual_design_panel =
                            cx.new(|cx| VisualDesignPanelView::new(fleet.clone(), cx));
                        let action_panel = cx.new(|cx| {
                            ActionPanelView::new(
                                cx,
                                fleet.clone(),
                                agent.clone(),
                                interactive_agent_window.clone(),
                            )
                        });
                        let agent_for_sessions = agent.clone();
                        let sessions = cx.new(|cx| {
                            SessionsView::new(window, cx, agent_for_sessions, fleet.clone())
                        });
                        let agent_for_conversation = agent.clone();
                        let conversation = cx.new(|cx| {
                            ConversationView::new(
                                window,
                                cx,
                                agent_for_conversation,
                                fleet.clone(),
                                lifecycle.clone(),
                            )
                        });
                        conversation.update(cx, |conversation, cx| {
                            conversation.bind_incoming_check(incoming_check.clone(), cx)
                        });
                        let settings = cx.new(|cx| SettingsView::new(window, cx));
                        let database = cx.new(|cx| DatabaseView::new(window, cx, fleet.clone()));
                        let view = cx.new(|cx| {
                            let _task_list_subscription =
                                cx.subscribe(&task_list, |this: &mut Shell, _, event, cx| {
                                    match event {
                                        TaskListEvent::FocusDrawer => {
                                            this.queue_drawer(DrawerRequest::Focus, cx);
                                        }
                                        TaskListEvent::GeneratorRefreshed { node_id } => {
                                            let node_id = *node_id;
                                            this.drawer.task_edit.update(cx, |edit, cx| {
                                                edit.reload_generator_for(node_id, cx);
                                            });
                                        }
                                        TaskListEvent::OpenInterview {
                                            task_id,
                                            node_id,
                                            lifecycle,
                                            title,
                                        } => {
                                            this.queue_open_interview(
                                                task_id.clone(),
                                                node_id.clone(),
                                                lifecycle.clone(),
                                                title.clone(),
                                                cx,
                                            );
                                        }
                                        TaskListEvent::OpenTaskEdit { task_id } => {
                                            this.queue_drawer(
                                                DrawerRequest::OpenTaskEdit {
                                                    task_id: task_id.clone(),
                                                },
                                                cx,
                                            );
                                        }
                                        TaskListEvent::OpenObligations { task_id, title } => {
                                            this.queue_drawer(
                                                DrawerRequest::OpenObligations {
                                                    task_id: task_id.clone(),
                                                    title: title.clone(),
                                                },
                                                cx,
                                            );
                                        }
                                        TaskListEvent::OpenPlan { task_id, title } => {
                                            this.queue_drawer(
                                                DrawerRequest::OpenPlan {
                                                    task_id: task_id.clone(),
                                                    title: title.clone(),
                                                },
                                                cx,
                                            );
                                        }
                                        TaskListEvent::CloseDrawer => {
                                            this.queue_drawer(DrawerRequest::Close, cx);
                                        }
                                        TaskListEvent::OpenLifecycle { task_id, .. } => {
                                            this.queue_drawer(
                                                DrawerRequest::OpenLifecycle {
                                                    task_id: task_id.clone(),
                                                },
                                                cx,
                                            );
                                        }
                                        TaskListEvent::SelectionChanged { task_id } => {
                                            this.queue_drawer(
                                                DrawerRequest::Follow {
                                                    task_id: task_id.clone(),
                                                },
                                                cx,
                                            );
                                        }
                                        TaskListEvent::OpenActionPanel { task_id } => {
                                            this.queue_drawer(
                                                DrawerRequest::OpenActionPanel {
                                                    task_id: task_id.clone(),
                                                },
                                                cx,
                                            );
                                        }
                                        TaskListEvent::LaunchOrFocusAgent { task_id } => {
                                            this.handle_launch_or_focus_agent(task_id.clone(), cx);
                                        }
                                        TaskListEvent::OpenShell { task_id, shell_id } => {
                                            this.handle_open_shell(
                                                task_id.clone(),
                                                shell_id.clone(),
                                                cx,
                                            );
                                        }
                                        TaskListEvent::OpenCodeEditor { task_id, editor_id } => {
                                            this.handle_open_code_editor(
                                                task_id.clone(),
                                                editor_id.clone(),
                                                cx,
                                            );
                                        }
                                    }
                                });
                            let _task_edit_subscription =
                                cx.subscribe(&task_edit, |this: &mut Shell, _, event, cx| {
                                    match event {
                                        TaskEditEvent::Close => {
                                            this.on_drawer_panel_closed(cx);
                                        }
                                        TaskEditEvent::FocusTaskList => {
                                            this.pending_refocus_task_list = true;
                                            cx.notify();
                                        }
                                        // The edit panel has no credential
                                        // prompt of its own; the task list
                                        // owns the one prompt, collects the
                                        // key and resumes the refresh.
                                        TaskEditEvent::LinearCredentialsRequired { node_id } => {
                                            this.pending_linear_credentials_for = Some(*node_id);
                                            cx.notify();
                                        }
                                        TaskEditEvent::Changed => {
                                            this.task_list.update(cx, |list, cx| {
                                                list.request_live_refresh(cx);
                                            });
                                        }
                                        TaskEditEvent::OpenObligations { task_id, title } => {
                                            this.queue_drawer(
                                                DrawerRequest::OpenObligations {
                                                    task_id: task_id.clone(),
                                                    title: title.clone(),
                                                },
                                                cx,
                                            );
                                        }
                                    }
                                });
                            let _obligations_subscription =
                                cx.subscribe(&obligations, |this: &mut Shell, _, event, cx| {
                                    match event {
                                        ObligationsEvent::Close => {
                                            this.on_drawer_panel_closed(cx);
                                        }
                                        ObligationsEvent::FocusTaskList => {
                                            this.pending_refocus_task_list = true;
                                            cx.notify();
                                        }
                                        ObligationsEvent::DeleteSelectedTask => {
                                            this.pending_delete_selected_task = true;
                                            cx.notify();
                                        }
                                        ObligationsEvent::OpenAgentChat {
                                            node_id,
                                            obligation_id,
                                        } => {
                                            this.queue_open_conversation(
                                                obligation_focus(*node_id, *obligation_id),
                                                cx,
                                            );
                                        }
                                        ObligationsEvent::OpenVisualDesign {
                                            node_id,
                                            obligation_id,
                                        } => {
                                            this.queue_drawer(
                                                DrawerRequest::OpenVisualDesign {
                                                    node_id: *node_id,
                                                    obligation_id: *obligation_id,
                                                },
                                                cx,
                                            );
                                        }
                                    }
                                });
                            let _plan_subscription =
                                cx.subscribe(&plan, |this: &mut Shell, _, event, cx| match event {
                                    PlanStepsEvent::Close => {
                                        this.on_drawer_panel_closed(cx);
                                    }
                                    PlanStepsEvent::FocusTaskList => {
                                        this.pending_refocus_task_list = true;
                                        cx.notify();
                                    }
                                    PlanStepsEvent::DeleteSelectedTask => {
                                        this.pending_delete_selected_task = true;
                                        cx.notify();
                                    }
                                });
                            let _lifecycle_panel_subscription =
                                cx.subscribe(&lifecycle_panel, |this: &mut Shell, _, event, cx| {
                                    match event {
                                        LifecyclePanelEvent::Close => {
                                            this.task_list.update(cx, |list, cx| {
                                                list.request_live_refresh(cx);
                                            });
                                            this.on_drawer_panel_closed(cx);
                                        }
                                        LifecyclePanelEvent::FocusTaskList => {
                                            this.pending_refocus_task_list = true;
                                            cx.notify();
                                        }
                                        LifecyclePanelEvent::OpenInterview {
                                            task_id,
                                            lifecycle,
                                        } => {
                                            this.pending_open_interview_for_task =
                                                Some((task_id.clone(), lifecycle.clone()));
                                            cx.notify();
                                        }
                                    }
                                });
                            let _visual_design_panel_subscription = cx.subscribe(
                                &visual_design_panel,
                                |this: &mut Shell, _, event, cx| match event {
                                    VisualDesignPanelEvent::Close => {
                                        this.on_drawer_panel_closed(cx);
                                    }
                                    VisualDesignPanelEvent::FocusTaskList => {
                                        this.pending_refocus_task_list = true;
                                        cx.notify();
                                    }
                                },
                            );
                            let _action_panel_subscription =
                                cx.subscribe(&action_panel, |this: &mut Shell, _, event, cx| {
                                    match event {
                                        ActionPanelEvent::Close => {
                                            this.on_drawer_panel_closed(cx);
                                        }
                                        ActionPanelEvent::FocusTaskList => {
                                            this.pending_refocus_task_list = true;
                                            cx.notify();
                                        }
                                        ActionPanelEvent::Changed => {
                                            this.task_list.update(cx, |list, cx| {
                                                list.request_live_refresh(cx);
                                            });
                                        }
                                    }
                                });
                            let _sessions_subscription = cx.subscribe(
                                &sessions,
                                |this: &mut Shell, _, event, cx| match event {
                                    SessionsEvent::ReturnToTaskList => {
                                        this.pending_return_to_tasks = true;
                                        cx.notify();
                                    }
                                    SessionsEvent::ProceedToLifecycle { task_id, lifecycle } => {
                                        this.pending_open_lifecycle = Some(PendingOpenLifecycle {
                                            task_id: task_id.clone(),
                                            lifecycle: lifecycle.clone(),
                                        });
                                        this.pending_return_to_tasks = true;
                                        cx.notify();
                                    }
                                    SessionsEvent::OpenAgentChat {
                                        node_id,
                                        obligation_id,
                                    } => {
                                        // Queued, not opened here: this event arrives
                                        // through nested entity updates (obligations ->
                                        // workspace -> sessions -> shell) whose leases are
                                        // still on the stack, and opening needs `window`.
                                        this.queue_open_conversation(
                                            obligation_focus(*node_id, *obligation_id),
                                            cx,
                                        );
                                    }
                                },
                            );
                            let _conversation_subscription =
                                cx.subscribe(&conversation, |this: &mut Shell, _, event, cx| {
                                    match event {
                                        ConversationViewEvent::Leave => {
                                            this.pending_leave_conversation = true;
                                            cx.notify();
                                        }
                                        ConversationViewEvent::Notice(notice) => match notice {
                                            RunNotice::Error(message) => {
                                                this.queue_error_toast(message.clone(), cx)
                                            }
                                            RunNotice::Warning(message) => {
                                                this.queue_warning_toast(message.clone(), cx)
                                            }
                                        },
                                        ConversationViewEvent::EnterState { node_id } => {
                                            this.pending_on_entry = Some(*node_id);
                                            cx.notify();
                                        }
                                        ConversationViewEvent::GoToTasks {
                                            node_id,
                                            obligation_id,
                                        } => {
                                            this.pending_go_to_tasks =
                                                Some((*node_id, *obligation_id));
                                            cx.notify();
                                        }
                                    }
                                });
                            let _incoming_check_subscription = cx.subscribe(
                                &incoming_check,
                                |this: &mut Shell, _, event, cx| match event {
                                    IncomingCheckEvent::GateReady(node) => {
                                        this.pending_gate_check = Some(*node);
                                        cx.notify();
                                    }
                                },
                            );
                            let _settings_subscription = cx.subscribe(
                                &settings,
                                |this: &mut Shell, _, event, cx| match event {
                                    SettingsEvent::AgentPlatformChanged(platform) => {
                                        this.replace_agent_platform(*platform, cx);
                                    }
                                },
                            );
                            let agent_status_text =
                                format_status_bar(&AgentStatusGroups::default()).into();
                            let tasks_split_state = cx.new(|_| PanelSplitState::centered());
                            let shell = Shell {
                                active_view: ShellView::Tasks,
                                task_list,
                                drawer: RightDrawer {
                                    task_edit,
                                    obligations,
                                    plan,
                                    lifecycle: lifecycle_panel,
                                    visual_design: visual_design_panel,
                                    action: action_panel,
                                },
                                sessions,
                                conversation,
                                view_before_conversation: ShellView::Tasks,
                                settings,
                                database,
                                fleet: fleet.clone(),
                                _mutation_socket: mutation_socket,
                                agent: agent.clone(),
                                traffic_log: traffic_log.clone(),
                                transcript_window: transcript_window.clone(),
                                _interactive_agent_window: interactive_agent_window.clone(),
                                history_window: history_window.clone(),
                                agent_status_text,
                                paths: paths.clone(),
                                migration_notice_dismissed: false,
                                pending_open_interview: None,
                                pending_open_interview_for_task: None,
                                pending_open_conversation: None,
                                pending_on_entry: None,
                                pending_go_to_tasks: None,
                                pending_gate_check: None,
                                pending_leave_conversation: false,
                                pending_open_lifecycle: None,
                                pending_return_to_tasks: false,
                                pending_drawer: Vec::new(),
                                pending_delete_selected_task: false,
                                pending_refocus_task_list: false,
                                pending_linear_credentials_for: None,
                                pending_error_toast: None,
                                pending_warning_toast: None,
                                always_on_top: restore_always_on_top,
                                tasks_split_state,
                                _task_list_subscription,
                                _task_edit_subscription,
                                _obligations_subscription,
                                _plan_subscription,
                                _lifecycle_panel_subscription,
                                _visual_design_panel_subscription,
                                _action_panel_subscription,
                                _sessions_subscription,
                                _conversation_subscription,
                                _settings_subscription,
                                _incoming_check_subscription,
                            };
                            let status_hub = status::hub(cx);
                            cx.observe(&status_hub, |_, _, cx| cx.notify()).detach();
                            let poll_entity = cx.weak_entity();
                            cx.spawn(async move |_, cx| {
                                loop {
                                    cx.background_executor()
                                        .timer(std::time::Duration::from_millis(500))
                                        .await;
                                    let _ = poll_entity.update(cx, |shell, cx| {
                                        shell.refresh_agent_status(cx);
                                    });
                                }
                            })
                            .detach();
                            // A transcript format the reader does not know is shown
                            // at once, so the reader can be brought up to date.
                            let notice_entity = cx.weak_entity();
                            cx.spawn(async move |_, cx| {
                                while let Ok(notice) = format_rx.recv().await {
                                    let message = transcript_format_message(&notice);
                                    let shown = notice_entity.update(cx, |shell, cx| {
                                        shell.queue_error_toast(message, cx);
                                    });
                                    if shown.is_err() {
                                        break;
                                    }
                                }
                            })
                            .detach();
                            #[cfg(feature = "agent-socket")]
                            {
                                if let Ok(mut slot) = shell_for_socket.lock() {
                                    *slot = Some(cx.weak_entity());
                                }
                            }
                            shell
                        });
                        let fleet_for_close = fleet.clone();
                        let lifecycle_panel_for_close = view.read(cx).drawer.lifecycle.clone();
                        let sessions_for_close = view.read(cx).sessions.clone();
                        let conversation_for_close = view.read(cx).conversation.clone();
                        window.on_window_should_close(cx, move |window, cx| {
                            let running = collect_running_work(
                                &fleet_for_close,
                                &lifecycle_panel_for_close,
                                &sessions_for_close,
                                &conversation_for_close,
                                cx,
                            );
                            if running.is_empty() {
                                persist_window_geometry(window, &paths_for_geometry);
                                let _ = transcript_for_close.close(cx);
                                history_for_close.close(cx);
                                interactive_for_close.close_all(cx);
                                true
                            } else {
                                let paths_for_force = paths_for_geometry.clone();
                                let transcript_for_force = transcript_for_close.clone();
                                let history_for_force = history_for_close.clone();
                                let interactive_for_force = interactive_for_close.clone();
                                crate::ui::toast::close_guard_toast(
                                    window,
                                    cx,
                                    running,
                                    move |window, cx| {
                                        persist_window_geometry(window, &paths_for_force);
                                        let _ = transcript_for_force.close(cx);
                                        history_for_force.close(cx);
                                        interactive_for_force.close_all(cx);
                                        window.remove_window();
                                    },
                                );
                                false
                            }
                        });
                        cx.new(|cx| Root::new(view, window, cx))
                    }
                }
            }
        },
    )?;

    #[cfg(windows)]
    if no_focus {
        no_focus::after_window_open(previous_foreground);
    }

    // A topmost window would sit over whatever the user is doing.
    if restore_always_on_top && !no_focus {
        always_on_top::set(true);
    }

    #[cfg(feature = "agent-socket")]
    if let Some((listener, addr)) = socket_listener {
        let shell_weak = shell_for_socket
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .expect("shell weak entity for agent socket");
        agent_socket::start(
            cx,
            handle.into(),
            listener,
            addr,
            width,
            height,
            transcript_for_socket,
            shell_weak,
        );
    }

    Ok(())
}

pub fn open_data_root_setup(cx: &mut AsyncApp, opts: LaunchOptions) -> Result<()> {
    cx.open_window(
        WindowOptions {
            titlebar: Some(TitleBar::title_bar_options()),
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(720.), px(420.)),
            })),
            focus: no_focus::window_focus(),
            ..Default::default()
        },
        {
            move |window, cx| {
                let view = cx.new(|cx| DataRootSetupView::new(opts, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            }
        },
    )?;
    Ok(())
}

pub fn register_shell_keyboard_bindings(cx: &mut App) {
    register_app_nav_keyboard_bindings(cx);
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-a", ShellOpenAgentTranscripts, Some(NOT_INPUT)),
        KeyBinding::new("ctrl-shift-h", ShellOpenHistory, Some(NOT_INPUT)),
        KeyBinding::new("ctrl-z", ShellUndo, Some(NOT_INPUT)),
    ]);
}

#[cfg(test)]
mod tests {
    use super::{fallback_focus, obligation_focus, spec_conversation_focus};
    use tod_store::conversation::Focus;
    use uuid::Uuid;

    #[test]
    fn proposed_and_design_nodes_open_the_conversation_view() {
        let node = Uuid::new_v4();
        for lifecycle in ["proposed", "design"] {
            assert_eq!(
                spec_conversation_focus(lifecycle, node),
                Some(Focus::Node(node)),
                "{lifecycle}"
            );
        }
        for lifecycle in ["planning", "ready", "active", ""] {
            assert_eq!(
                spec_conversation_focus(lifecycle, node),
                None,
                "{lifecycle}"
            );
        }
    }

    #[test]
    fn ctrl_j_falls_back_to_the_tree_selection_then_the_project() {
        let node = Uuid::new_v4();
        assert_eq!(fallback_focus(Some(node)), Focus::Node(node));
        assert_eq!(fallback_focus(None), Focus::Project);
    }

    #[test]
    fn obligations_panel_selection_becomes_the_focus() {
        let (node, id) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(
            obligation_focus(node, Some(id)),
            Focus::Obligation { node, id }
        );
        assert_eq!(obligation_focus(node, None), Focus::Node(node));
    }
}
