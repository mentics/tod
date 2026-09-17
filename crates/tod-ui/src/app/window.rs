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
use crate::drafting::{DraftingView, DraftingViewEvent};
use crate::interview::agent::{AgentBackend, AgentPlatform, SharedAgent};
use crate::interview::settings::{persist_window_geometry, resolve_open_window_bounds};
use crate::interview::views::{SessionsEvent, SessionsView, SettingsEvent, SettingsView};
use crate::interview::{TaskListProceedContext, TodPaths, TodSettings};
use crate::ui::actionable::render_shortcut_pill_in_context;
use crate::ui::app_nav::{
    HasAppNav, ShellGoDatabase, ShellGoSettings, ShellGoTasks, register_app_nav_keyboard_bindings,
};
use crate::ui::key_context::NOT_INPUT;
use crate::ui::panel_split::{PanelSplitState, h_panel_split};
use crate::ui::selectable_text::selectable_text;
use crate::ui::toast::{error_toast, notification_overlay};
use crate::views::action_panel::{ActionPanelEvent, ActionPanelView};
use crate::views::database::DatabaseView;
use crate::views::lifecycle_panel::{LifecyclePanelEvent, LifecyclePanelView};
use crate::views::obligations::{ObligationsEvent, ObligationsView};
use crate::views::task_edit::{TaskEditEvent, TaskEditView};
use crate::views::task_list::{TaskListEvent, TaskListView};
use crate::views::visual_design_panel::{
    EmbeddedChatParams, VisualDesignPanelEvent, VisualDesignPanelView,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use tod_agent::EngagementState;
use gpui_component::{ActiveTheme, IconName, Root, Selectable, StyledExt, TitleBar, h_flex};
use std::path::PathBuf;
use std::sync::Arc;
use tod_core::drafting::DraftingMode;
use tod_core::process::{interview_phase_for_lifecycle, interview_phase_label};
use tod_store::agent_traffic::{
    AgentStatusGroups, SharedAgentTrafficLog, format_status_bar, shared_log,
};
use tod_store::fleet::terminal::{focus_shell_session, open_shell_for_node};
use tod_store::fleet::{FleetLaunchError, FleetStore, code_editor, open_code_editor_for_node};
use uuid::Uuid;

actions!(
    shell,
    [ShellOpenAgentTranscripts, ShellOpenHistory, ShellUndo]
);

const TASKS_TREE_MIN: f32 = 240.0;
const TASKS_DRAWER_MIN: f32 = 280.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellView {
    Tasks,
    Interview,
    /// Capture (`proposed`) and the drafting loop (`design`).
    Drafting,
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
    drafting: Entity<DraftingView>,
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
    status_line: SharedString,
    paths: TodPaths,
    migration_notice_dismissed: bool,
    pending_open_interview: Option<PendingOpenInterview>,
    /// (task_id, lifecycle) — from the lifecycle panel's on-demand
    /// "Open interview" affordance, validated and routed through
    /// `TaskListView::open_interview_for_task` once `window` is available.
    pending_open_interview_for_task: Option<(String, String)>,
    /// Node whose pre-v3 obligations the drafter should rewrite (from the
    /// obligations panel), opened in the drafting view once `window` is available.
    pending_rewrite_pre_v3: Option<Uuid>,
    pending_open_lifecycle: Option<PendingOpenLifecycle>,
    pending_return_to_tasks: bool,
    /// Drawer changes queued by event handlers, applied in order on render.
    pending_drawer: Vec<DrawerRequest>,
    pending_delete_selected_task: bool,
    pending_refocus_task_list: bool,
    pending_error_toast: Option<String>,
    always_on_top: bool,
    tasks_split_state: Entity<PanelSplitState>,
    _task_list_subscription: Subscription,
    _task_edit_subscription: Subscription,
    _obligations_subscription: Subscription,
    _lifecycle_panel_subscription: Subscription,
    _visual_design_panel_subscription: Subscription,
    _action_panel_subscription: Subscription,
    _sessions_subscription: Subscription,
    _drafting_subscription: Subscription,
    _settings_subscription: Subscription,
}

/// Human-readable summary of background work that would be lost if the
/// window closed right now: agents mid-run and gate checks in flight.
fn collect_running_work(
    fleet: &FleetStore,
    lifecycle_panel: &Entity<LifecyclePanelView>,
    sessions: &Entity<SessionsView>,
    drafting: &Entity<DraftingView>,
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
    for task_id in lifecycle_panel.read(cx).running_gate_check_task_ids() {
        let title = fleet
            .get_task(&task_id)
            .ok()
            .flatten()
            .map(|t| t.title)
            .unwrap_or_else(|| task_id.clone());
        items.push(SharedString::from(format!("Gate check running: {title}")));
    }
    for item in sessions.read(cx).running_interview_work() {
        items.push(SharedString::from(item));
    }
    for item in drafting.read(cx).running_drafting_work() {
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
        self.drafting
            .update(cx, |drafting, _| drafting.close_app_nav());
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
        self.active_view = view;
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
            ShellView::Drafting => {
                let focus = self.drafting.read(cx).focus_handle(cx);
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
        // Capture and design are drafted; planning keeps its interview.
        self.active_view = if DraftingMode::for_lifecycle(&lifecycle).is_some() {
            ShellView::Drafting
        } else {
            ShellView::Interview
        };
        self.pending_open_interview = Some(PendingOpenInterview {
            task_id,
            node_id,
            lifecycle,
            title,
        });
        cx.notify();
    }

    fn drain_pending_rewrite_pre_v3(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node_id) = self.pending_rewrite_pre_v3.take() else {
            return;
        };
        let lifecycle = self
            .fleet
            .get_task(&node_id.to_string())
            .ok()
            .flatten()
            .map(|t| t.lifecycle)
            .unwrap_or_default();
        let proceed = (!lifecycle.is_empty()).then(|| TaskListProceedContext {
            task_id: node_id.to_string(),
            lifecycle,
        });
        self.active_view = ShellView::Drafting;
        self.drafting.update(cx, |drafting, cx| {
            drafting.open(node_id, proceed, Some(false), window, cx);
        });
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
        if DraftingMode::for_lifecycle(&pending.lifecycle).is_some() {
            self.drafting.update(cx, |drafting, cx| {
                drafting.open(
                    pending.node_id,
                    Some(TaskListProceedContext {
                        task_id: pending.task_id,
                        lifecycle: pending.lifecycle,
                    }),
                    None,
                    window,
                    cx,
                );
            });
            return;
        }
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
        let gate_activity = self.drawer.lifecycle.read(cx).in_flight_activity();
        self.task_list.update(cx, |list, cx| {
            list.set_agent_activity(gate_activity, cx);
        });
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

    /// Open an agent conversation scoped to the obligations panel.
    ///
    /// Sessions are deliberately not reused: every click starts a fresh
    /// conversation whose first message carries the assembled app context.
    /// Nothing is sent to the agent until the user submits that message.
    fn open_obligations_agent_chat(
        &mut self,
        node_id: uuid::Uuid,
        obligation_id: Option<uuid::Uuid>,
        cx: &mut Context<Self>,
    ) {
        let context = match self.build_obligations_agent_context(node_id, obligation_id) {
            Ok(text) => Some(text),
            Err(err) => {
                // Context assembly failing should not block the conversation.
                tracing::warn!(
                    event = "agent_chat",
                    action = "context_unavailable",
                    error = %err,
                    "opening agent chat without app context"
                );
                None
            }
        };

        if let Err(err) = self._interactive_agent_window.create_and_open_session(
            &node_id.to_string(),
            Some("obligations"),
            context,
            cx,
        ) {
            self.queue_error_toast(err, cx);
        }
    }

    /// Assemble the agent's first message: bundled context docs plus the live
    /// selection.
    fn build_obligations_agent_context(
        &self,
        node_id: uuid::Uuid,
        obligation_id: Option<uuid::Uuid>,
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
        let obligation = obligation_id.and_then(|id| {
            self.fleet
                .list_obligations_for_node(node_id)
                .ok()?
                .into_iter()
                .find(|o| o.id == id)
                .map(|o| ObligationSelection {
                    id: o.id,
                    kind: o.kind,
                    body: o.body,
                    visual_design_path: o.visual_design_path,
                })
        });

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
                recipe: tod_core::agent_context::OBLIGATIONS_RECIPE,
                data_root: self.paths.data_root(),
                node: NodeSelection {
                    id: node_id,
                    slug: Some(node.slug.clone()),
                    title: node.title,
                    body,
                    lifecycle: Some(node.lifecycle),
                },
                obligation,
                ancestor_context,
            },
        )
    }

    fn queue_error_toast(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.pending_error_toast = Some(message.into());
        cx.notify();
    }

    fn drain_pending_error_toast(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let settings = TodSettings::load(&self.paths).unwrap_or_default();
        let result: anyhow::Result<String> = (|| {
            if let Some(shell_id) = shell_id {
                let shell = self
                    .fleet
                    .get_shell(&shell_id)?
                    .ok_or_else(|| anyhow::anyhow!("shell session not found"))?;
                let cwd = focus_shell_session(&self.fleet, &self.paths, &settings, &shell)?;
                return Ok(format!("Focused shell in {}", cwd.display()));
            }
            let (_, cwd) =
                open_shell_for_node(&self.fleet, &self.paths, &settings, &task_id, None)?;
            Ok(format!("Opened terminal in {}", cwd.display()))
        })();

        match result {
            Ok(msg) => {
                let _ = self.fleet.reload_if_stale();
                self.task_list.update(cx, |list, cx| {
                    list.set_status_message(msg, cx);
                    list.request_live_refresh(cx);
                });
            }
            Err(err) => {
                self.queue_error_toast(format!("Shell failed: {err:#}"), cx);
            }
        }
        cx.notify();
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

    fn render_status_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let status = self.status_line.clone();
        h_flex()
            .w_full()
            .flex_shrink_0()
            .px_4()
            .py_1p5()
            .border_t_1()
            .border_color(border)
            .justify_between()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
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

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child("tod")
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
        self.drain_pending_rewrite_pre_v3(window, cx);
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
            .child(self.render_title_bar(cx))
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
            ShellView::Drafting => self.drafting.clone().into_any_element(),
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
    /// Mirrors `open_obligations_agent_chat`, but constructs the chat
    /// in-process via `InteractiveAgentWindowControl::create_embedded_session`
    /// instead of opening a standalone window, so it can sit inside the panel
    /// next to the mockup preview.
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
    /// plus the live obligation selection (see `build_obligations_agent_context`).
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
    }
}

fn platform_label(platform: AgentPlatform) -> &'static str {
    match platform {
        AgentPlatform::Cursor => "cursor",
        AgentPlatform::Claude => "claude",
    }
}

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

/// One-time fetch-and-cache for runs `run_launch_hooks` just found dead (or
/// that ended in an earlier process) with a resumable agent-side session but
/// no cached transcript yet. Read-only (no prompt sent) and best-effort: a
/// run whose worktree is gone or whose agent process can't be reached is
/// skipped silently, since a live open of the transcript window will retry.
fn backfill_missing_transcripts(fleet: &Arc<FleetStore>, agent: &SharedAgent) {
    let runs = match fleet.list_all_runs() {
        Ok(runs) => runs,
        Err(err) => {
            tracing::error!("backfill_missing_transcripts: listing runs failed: {err:#}");
            return;
        }
    };
    for run in runs {
        if run.is_live() || run.cached_transcript.is_some() {
            continue;
        }
        let (Some(agent_session_id), Some(platform_str)) =
            (run.agent_session_id.clone(), run.platform.clone())
        else {
            continue;
        };
        let Some(platform) = tod_store::parse_platform(&platform_str) else {
            continue;
        };
        let Ok(cwd) = tod_store::fleet::resolve_launch_cwd(fleet, &run.node_id) else {
            continue;
        };
        let transcript = {
            let agent = agent.lock().unwrap_or_else(|e| e.into_inner());
            agent.fetch_full_transcript(platform, &cwd, &agent_session_id)
        };
        match transcript {
            Ok(text) => {
                let _ = fleet.enqueue(tod_store::fleet::FleetMutation::CacheAgentRunTranscript {
                    run_id: run.id,
                    transcript: text,
                    fingerprint: None,
                });
            }
            Err(err) => {
                tracing::warn!(
                    run_id = %run.id,
                    "backfill_missing_transcripts: fetch failed: {err:#}"
                );
            }
        }
    }
    let _ = fleet.writer().flush();
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
                        // Reconcile stale agent/shell runtime status off the main thread. This
                        // probes OS process liveness for every reconnect-tracked row, which is
                        // unbounded work (and, on Windows, previously shelled out to
                        // powershell.exe per row) — none of it needs to finish before the window
                        // is visible, so it must never sit on the path to `cx.open_window`.
                        let reattach_fleet = fleet.clone();
                        let reattach_agent = agent.clone();
                        std::thread::spawn(move || {
                            if let Err(err) = reattach_fleet
                                .run_launch_hooks(&tod_store::fleet::NoopGuestLiveness)
                            {
                                tracing::error!("background launch-time reattach failed: {err:#}");
                            }
                            backfill_missing_transcripts(&reattach_fleet, &reattach_agent);
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
                        if opts.import_process {
                            let repo = paths.repo_root().to_path_buf();
                            match fleet.import_doc_process(&repo) {
                                Ok(()) => {
                                    if let Ok(lists) = fleet.list_outline_lists() {
                                        for list in lists {
                                            if let Ok(rows) = fleet.flatten_outline(list.id) {
                                                tracing::info!(
                                                    event = "doc_process_import",
                                                    outline_rows = rows.len(),
                                                    repo = %repo.display(),
                                                    "doc/process import finished"
                                                );
                                            }
                                        }
                                    }
                                }
                                Err(err) => {
                                    tracing::error!("doc/process import failed: {err:#}");
                                }
                            }
                        }
                        let task_list = cx.new(|cx| TaskListView::new(window, cx, fleet.clone()));
                        let task_edit = cx
                            .new(|cx| TaskEditView::new(window, cx, fleet.clone(), paths.clone()));
                        let obligations =
                            cx.new(|cx| ObligationsView::new(window, cx, fleet.clone()));
                        let lifecycle_panel = cx.new(|cx| {
                            LifecyclePanelView::new(
                                cx,
                                fleet.clone(),
                                agent.clone(),
                                paths.clone(),
                                interactive_agent_window.clone(),
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
                        let agent_for_drafting = agent.clone();
                        let drafting = cx.new(|cx| {
                            DraftingView::new(window, cx, agent_for_drafting, fleet.clone())
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
                                        TaskListEvent::StatusChanged(message) => {
                                            this.status_line = message.clone();
                                            cx.notify();
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
                                            this.open_obligations_agent_chat(
                                                *node_id,
                                                *obligation_id,
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
                                        ObligationsEvent::RewritePreV3 { node_id } => {
                                            this.pending_rewrite_pre_v3 = Some(*node_id);
                                            cx.notify();
                                        }
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
                                        // `cx.defer`, not a direct call: this event
                                        // arrives synchronously through several nested
                                        // entity updates still on the stack (obligations
                                        // -> workspace -> sessions -> shell), and opening
                                        // the chat window here would try to lease those
                                        // same entities again before they're returned to
                                        // the app. Deferring runs this once the current
                                        // effect cycle (and all those leases) has flushed.
                                        let node_id = *node_id;
                                        let obligation_id = *obligation_id;
                                        let weak = cx.weak_entity();
                                        cx.defer(move |cx| {
                                            let _ = weak.update(cx, |this, cx| {
                                                this.open_obligations_agent_chat(
                                                    node_id,
                                                    obligation_id,
                                                    cx,
                                                );
                                            });
                                        });
                                    }
                                },
                            );
                            let _drafting_subscription = cx.subscribe(
                                &drafting,
                                |this: &mut Shell, _, event, cx| match event {
                                    DraftingViewEvent::ReturnToTaskList => {
                                        this.pending_return_to_tasks = true;
                                        cx.notify();
                                    }
                                    DraftingViewEvent::ProceedToLifecycle {
                                        task_id,
                                        lifecycle,
                                    } => {
                                        this.pending_open_lifecycle = Some(PendingOpenLifecycle {
                                            task_id: task_id.clone(),
                                            lifecycle: lifecycle.clone(),
                                        });
                                        this.pending_return_to_tasks = true;
                                        cx.notify();
                                    }
                                    DraftingViewEvent::OpenAgentChat {
                                        node_id,
                                        obligation_id,
                                    } => {
                                        // Deferred for the same reason as the sessions arm:
                                        // nested entity leases are still on the stack.
                                        let node_id = *node_id;
                                        let obligation_id = *obligation_id;
                                        let weak = cx.weak_entity();
                                        cx.defer(move |cx| {
                                            let _ = weak.update(cx, |this, cx| {
                                                this.open_obligations_agent_chat(
                                                    node_id,
                                                    obligation_id,
                                                    cx,
                                                );
                                            });
                                        });
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
                                    lifecycle: lifecycle_panel,
                                    visual_design: visual_design_panel,
                                    action: action_panel,
                                },
                                sessions,
                                drafting,
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
                                status_line: SharedString::default(),
                                paths: paths.clone(),
                                migration_notice_dismissed: false,
                                pending_open_interview: None,
                                pending_open_interview_for_task: None,
                                pending_rewrite_pre_v3: None,
                                pending_open_lifecycle: None,
                                pending_return_to_tasks: false,
                                pending_drawer: Vec::new(),
                                pending_delete_selected_task: false,
                                pending_refocus_task_list: false,
                                pending_error_toast: None,
                                always_on_top: restore_always_on_top,
                                tasks_split_state,
                                _task_list_subscription,
                                _task_edit_subscription,
                                _obligations_subscription,
                                _lifecycle_panel_subscription,
                                _visual_design_panel_subscription,
                                _action_panel_subscription,
                                _sessions_subscription,
                                _drafting_subscription,
                                _settings_subscription,
                            };
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
                        let drafting_for_close = view.read(cx).drafting.clone();
                        window.on_window_should_close(cx, move |window, cx| {
                            let running = collect_running_work(
                                &fleet_for_close,
                                &lifecycle_panel_for_close,
                                &sessions_for_close,
                                &drafting_for_close,
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
