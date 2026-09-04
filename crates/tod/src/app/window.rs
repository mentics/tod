use super::always_on_top;
use super::data_root_setup::DataRootSetupView;
use super::fleet_blocked::FleetBlockedView;
use super::no_focus;
#[cfg(feature = "agent-socket")]
use crate::agent_socket;
use crate::agent_socket::commands::AgentPlatformSocketCommand;
use crate::app::history_window::HistoryWindowControl;
use crate::app::interactive_agent_window::{
    InteractiveAgentOpenParams, InteractiveAgentWindowControl,
};
use crate::app::transcript_window::TranscriptWindowControl;
use crate::cli::LaunchOptions;
use crate::interview::agent::{AgentBackend, AgentPlatform, SharedAgent};
use crate::interview::settings::{persist_window_geometry, resolve_open_window_bounds};
use crate::interview::views::{SessionsEvent, SessionsView, SettingsEvent, SettingsView};
use crate::interview::{TaskListProceedContext, TodPaths, TodSettings};
use crate::process::{interview_phase_for_lifecycle, interview_phase_label};
use crate::ui::actionable::render_shortcut_pill_in_context;
use crate::ui::app_nav::{
    HasAppNav, ShellGoDatabase, ShellGoSettings, ShellGoTasks, register_app_nav_keyboard_bindings,
};
use crate::ui::key_context::NOT_INPUT;
use crate::ui::panel_split::{PanelSplitState, h_panel_split};
use crate::ui::selectable_text::selectable_text;
use crate::ui::toast::{error_toast, notification_overlay};
use crate::views::agent_config_panel::{AgentConfigPanelEvent, AgentConfigPanelView};
use crate::views::database::DatabaseView;
use crate::views::obligations::{ObligationsEvent, ObligationsView};
use crate::views::task_edit::{TaskEditEvent, TaskEditView};
use crate::views::task_list::{TaskListEvent, TaskListView};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, IconName, Root, Selectable, StyledExt, TitleBar, h_flex};
use std::path::PathBuf;
use std::sync::Arc;
use tod_store::agent_traffic::{
    AgentStatusGroups, SharedAgentTrafficLog, format_status_bar, shared_log,
};
use tod_store::fleet::{
    FleetLaunchError, FleetStore, focus_shell_session, open_shell_for_agent_config,
    verify_shell_session,
};
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

struct PendingOpenAgent {
    task_id: String,
    agent_id: Option<String>,
}

struct PendingLaunchOrFocusAgent {
    task_id: String,
    config_id: String,
    /// When true, open the panel and launch an auto run after open.
    launch_auto: bool,
}

pub struct Shell {
    active_view: ShellView,
    task_list: Entity<TaskListView>,
    task_edit: Entity<TaskEditView>,
    obligations: Entity<ObligationsView>,
    agent_panel: Entity<AgentConfigPanelView>,
    sessions: Entity<SessionsView>,
    settings: Entity<SettingsView>,
    database: Entity<DatabaseView>,
    fleet: Arc<FleetStore>,
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
    pending_open_lifecycle: Option<PendingOpenLifecycle>,
    pending_return_to_tasks: bool,
    pending_open_task_edit: Option<String>,
    pending_close_task_edit: bool,
    pending_retarget_task_edit: Option<String>,
    pending_open_obligations: Option<(String, String)>,
    pending_close_obligations: bool,
    pending_retarget_obligations: Option<(String, String)>,
    pending_delete_selected_task: bool,
    pending_refocus_task_list: bool,
    pending_open_agent: Option<PendingOpenAgent>,
    pending_launch_or_focus_agent: Option<PendingLaunchOrFocusAgent>,
    pending_close_agent_panel: bool,
    pending_retarget_agent: Option<(String, String)>,
    pending_error_toast: Option<String>,
    always_on_top: bool,
    tasks_split_state: Entity<PanelSplitState>,
    _task_list_subscription: Subscription,
    _task_edit_subscription: Subscription,
    _obligations_subscription: Subscription,
    _agent_panel_subscription: Subscription,
    _sessions_subscription: Subscription,
    _settings_subscription: Subscription,
}

impl Shell {
    fn select_view(&mut self, view: ShellView, window: &mut Window, cx: &mut Context<Self>) {
        self.task_list
            .update(cx, |list, _| list.app_nav_mut().close());
        self.sessions
            .update(cx, |sessions, cx| sessions.close_app_nav(cx));
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
                focus.focus(window);
            }
            ShellView::Interview => {
                self.sessions.update(cx, |sessions, _| {
                    sessions.focus(window);
                });
            }
            ShellView::Settings => {
                let focus = self.settings.read(cx).focus_handle(cx);
                focus.focus(window);
            }
            ShellView::Database => {
                let focus = self.database.read(cx).focus_handle(cx);
                focus.focus(window);
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
        self.pending_open_interview = Some(PendingOpenInterview {
            task_id,
            node_id,
            lifecycle,
            title,
        });
        self.active_view = ShellView::Interview;
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

    fn drain_pending_open_lifecycle(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_open_lifecycle.take() else {
            return;
        };
        self.task_list.update(cx, |list, cx| {
            list.open_lifecycle_panel(&pending.task_id, &pending.lifecycle, cx);
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
        if let Ok(agents) = self.fleet.list_all_agents() {
            groups.fleet.total = agents.len() as u32;
            groups.fleet.processing = agents
                .iter()
                .filter(|a| a.runtime_status == "processing")
                .count() as u32;
            groups.fleet.blocked = agents
                .iter()
                .filter(|a| a.runtime_status == "blocked")
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

    fn open_task_edit(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.obligations.read(cx).is_open() {
            self.close_obligations(window, cx);
        }
        self.task_edit.update(cx, |edit, cx| {
            edit.open(task_id, window, cx);
        });
        if !self.task_edit.read(cx).is_open() {
            self.task_list.update(cx, |list, cx| {
                list.show_error("Could not open node for editing", window, cx);
            });
            cx.notify();
            return;
        }
        self.task_list.update(cx, |list, cx| {
            list.set_slide_edit_open(true, cx);
        });
        cx.notify();
    }

    fn close_task_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.task_edit.update(cx, |edit, cx| {
            edit.close(cx);
        });
        self.task_list.update(cx, |list, cx| {
            list.set_slide_edit_open(false, cx);
            list.restore_focus(window, cx);
        });
        cx.notify();
    }

    fn retarget_task_edit(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.task_edit.read(cx).is_open() {
            return;
        }
        self.task_edit.update(cx, |edit, cx| {
            edit.retarget(task_id, window, cx);
        });
        cx.notify();
    }

    fn open_obligations(
        &mut self,
        task_id: &str,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return;
        };
        if self.task_edit.read(cx).is_open() {
            self.close_task_edit(window, cx);
        }
        self.obligations.update(cx, |panel, cx| {
            panel.open(node_id, title, window, cx);
        });
        self.task_list.update(cx, |list, cx| {
            list.set_obligations_open(true, cx);
        });
        cx.notify();
    }

    fn close_obligations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.obligations.update(cx, |panel, cx| {
            panel.close(window, cx);
        });
        self.task_list.update(cx, |list, cx| {
            list.set_obligations_open(false, cx);
            list.restore_focus(window, cx);
        });
        cx.notify();
    }

    fn retarget_obligations(
        &mut self,
        task_id: &str,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.obligations.read(cx).is_open() {
            return;
        }
        let Ok(node_id) = Uuid::parse_str(task_id) else {
            return;
        };
        self.obligations.update(cx, |panel, cx| {
            panel.retarget(node_id, title, window, cx);
        });
        cx.notify();
    }

    fn open_agent_panel(
        &mut self,
        task_id: &str,
        agent_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.task_edit.read(cx).is_open() {
            self.close_task_edit(window, cx);
        }
        if self.obligations.read(cx).is_open() {
            self.close_obligations(window, cx);
        }
        self.agent_panel.update(cx, |panel, cx| {
            if let Some(agent_id) = agent_id {
                panel.open_edit(task_id, agent_id, window, cx);
            } else {
                panel.open_new(task_id, window, cx);
            }
        });
        self.task_list.update(cx, |list, cx| {
            list.set_agent_panel_open(true, cx);
        });
        cx.notify();
    }

    fn close_agent_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agent_panel.update(cx, |panel, cx| {
            panel.close(cx);
        });
        self.task_list.update(cx, |list, cx| {
            list.set_agent_panel_open(false, cx);
            list.restore_focus(window, cx);
        });
        cx.notify();
    }

    fn retarget_agent_panel(
        &mut self,
        task_id: &str,
        agent_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.agent_panel.read(cx).is_open() {
            return;
        }
        self.agent_panel.update(cx, |panel, cx| {
            panel.retarget(task_id, Some(agent_id), window, cx);
        });
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
        agent_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let settings = TodSettings::load(&self.paths).unwrap_or_default();
        let result: anyhow::Result<String> = (|| {
            if let Some(shell_id) = shell_id {
                let shell = self
                    .fleet
                    .get_shell(&shell_id)?
                    .ok_or_else(|| anyhow::anyhow!("shell session not found"))?;
                let config_id = shell.agent_id.clone();
                let cwd = focus_shell_session(
                    &self.fleet,
                    &self.paths,
                    &settings,
                    &config_id,
                    &task_id,
                    &shell,
                )?;
                return Ok(format!("Opened terminal in {}", cwd.display()));
            }

            let config_id = if let Some(agent_id) = agent_id {
                agent_id
            } else {
                self.fleet
                    .resolve_agents_for_node(&task_id)?
                    .configs
                    .into_iter()
                    .next()
                    .map(|row| row.id)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "no agent config for this task; create one in Agent config first"
                        )
                    })?
            };

            let (_, cwd) = open_shell_for_agent_config(
                &self.fleet,
                &self.paths,
                &settings,
                &config_id,
                &task_id,
            )?;
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

    fn handle_launch_or_focus_agent(
        &mut self,
        task_id: String,
        config_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(agent) = self.fleet.get_agent(&config_id).ok().flatten() else {
            self.queue_error_toast(format!("Agent config {config_id} not found"), cx);
            return;
        };
        match agent.mode.as_str() {
            "interview" => {
                self.queue_error_toast(
                    "Interview agents are launched from the Interview view.",
                    cx,
                );
            }
            "shell" => {
                let sessions = self
                    .fleet
                    .list_interactive_sessions_for_config(&config_id)
                    .unwrap_or_default();
                let result = if let Some(run) = sessions.first() {
                    self._interactive_agent_window.open_session(
                        InteractiveAgentOpenParams {
                            task_id: task_id.clone(),
                            config_id: config_id.clone(),
                            session_run_id: run.id.clone(),
                        },
                        cx,
                    )
                } else {
                    self._interactive_agent_window
                        .create_and_open_session(&task_id, &config_id, cx)
                        .map(|_| ())
                };
                match result {
                    Ok(()) => {
                        let _ = self.fleet.reload_if_stale();
                        self.task_list.update(cx, |list, cx| {
                            list.set_status_message(
                                format!("Opened interactive agent {config_id}"),
                                cx,
                            );
                            list.request_live_refresh(cx);
                        });
                    }
                    Err(err) => {
                        self.queue_error_toast(format!("Interactive agent failed: {err}"), cx);
                    }
                }
            }
            _ => {
                // Auto mode: focus panel if running, else open panel and launch.
                let running = matches!(
                    agent.runtime_status.as_str(),
                    "starting" | "processing" | "waiting" | "blocked"
                );
                self.pending_launch_or_focus_agent = Some(PendingLaunchOrFocusAgent {
                    task_id,
                    config_id,
                    launch_auto: !running,
                });
                cx.notify();
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
                if self.task_edit.read(cx).is_open() {
                    self.task_edit.update(cx, |edit, cx| {
                        if let Some(id) = edit.open_task_id(cx) {
                            edit.retarget(&id, window, cx);
                        }
                    });
                }
                if self.obligations.read(cx).is_open() {
                    self.obligations.update(cx, |panel, cx| {
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
        self.drain_pending_return_to_tasks(window, cx);
        self.drain_pending_open_lifecycle(cx);
        self.drain_pending_task_edit(window, cx);
        self.drain_pending_obligations(window, cx);
        self.drain_pending_agent_panel(window, cx);
        self.drain_pending_error_toast(window, cx);

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
            ShellView::Settings => self.settings.clone().into_any_element(),
            ShellView::Database => self.database.clone().into_any_element(),
        }
    }

    /// Tasks always use a left tree + right drawer host. Edit and obligations
    /// replace the (future) agent list in the same right drawer; the tree stays.
    fn render_tasks_split(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let drawer =
            if self.obligations.read(cx).is_open() {
                self.obligations.clone().into_any_element()
            } else if self.task_edit.read(cx).is_open() {
                self.task_edit.clone().into_any_element()
            } else if self.agent_panel.read(cx).is_open() {
                self.agent_panel.clone().into_any_element()
            } else {
                div()
                    .size_full()
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .bg(theme.background)
                    .text_color(muted)
                    .child(div().text_sm().font_semibold().child("Agent configs"))
                    .child(div().text_xs().child(
                        "Press A to launch an agent, T for a shell, Shift+A for a new config",
                    ))
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

    fn drain_pending_task_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_close_task_edit {
            self.pending_close_task_edit = false;
            self.close_task_edit(window, cx);
        }
        if let Some(task_id) = self.pending_retarget_task_edit.take() {
            self.retarget_task_edit(&task_id, window, cx);
        }
        if let Some(task_id) = self.pending_open_task_edit.take() {
            self.open_task_edit(&task_id, window, cx);
        }
    }

    fn drain_pending_obligations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_close_obligations {
            self.pending_close_obligations = false;
            self.close_obligations(window, cx);
        }
        if self.pending_refocus_task_list {
            self.pending_refocus_task_list = false;
            self.task_list.update(cx, |list, cx| {
                list.restore_focus(window, cx);
            });
        }
        if let Some((task_id, title)) = self.pending_retarget_obligations.take() {
            self.retarget_obligations(&task_id, &title, window, cx);
        }
        if let Some((task_id, title)) = self.pending_open_obligations.take() {
            self.open_obligations(&task_id, &title, window, cx);
        }
        if self.pending_delete_selected_task {
            self.pending_delete_selected_task = false;
            self.task_list.update(cx, |list, cx| {
                list.delete_selected_task(window, cx);
            });
        }
    }

    fn drain_pending_agent_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_close_agent_panel {
            self.pending_close_agent_panel = false;
            self.close_agent_panel(window, cx);
        }
        if let Some((task_id, agent_id)) = self.pending_retarget_agent.take() {
            self.retarget_agent_panel(&task_id, &agent_id, window, cx);
        }
        if let Some(pending) = self.pending_open_agent.take() {
            self.open_agent_panel(&pending.task_id, pending.agent_id.as_deref(), window, cx);
        }
        if let Some(pending) = self.pending_launch_or_focus_agent.take() {
            self.open_agent_panel(&pending.task_id, Some(&pending.config_id), window, cx);
            if pending.launch_auto {
                self.agent_panel.update(cx, |panel, cx| {
                    panel.launch_auto_run(window, cx);
                });
            }
        }
    }

    fn queue_open_agent(
        &mut self,
        task_id: String,
        agent_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.pending_open_agent = Some(PendingOpenAgent { task_id, agent_id });
        cx.notify();
    }

    fn queue_retarget_agent(&mut self, task_id: String, agent_id: String, cx: &mut Context<Self>) {
        self.pending_retarget_agent = Some((task_id, agent_id));
        cx.notify();
    }

    fn queue_close_agent_panel(&mut self, cx: &mut Context<Self>) {
        self.pending_close_agent_panel = true;
        cx.notify();
    }

    fn queue_open_task_edit(&mut self, task_id: String, cx: &mut Context<Self>) {
        self.pending_open_task_edit = Some(task_id);
        cx.notify();
    }

    fn queue_retarget_task_edit(&mut self, task_id: String, cx: &mut Context<Self>) {
        self.pending_retarget_task_edit = Some(task_id);
        cx.notify();
    }

    fn queue_close_task_edit(&mut self, cx: &mut Context<Self>) {
        self.pending_close_task_edit = true;
        cx.notify();
    }

    fn queue_open_obligations(&mut self, task_id: String, title: String, cx: &mut Context<Self>) {
        self.pending_open_obligations = Some((task_id, title));
        cx.notify();
    }

    fn queue_retarget_obligations(
        &mut self,
        task_id: String,
        title: String,
        cx: &mut Context<Self>,
    ) {
        self.pending_retarget_obligations = Some((task_id, title));
        cx.notify();
    }

    fn queue_close_obligations(&mut self, cx: &mut Context<Self>) {
        self.pending_close_obligations = true;
        cx.notify();
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
    let mut store = FleetStore::open(&root).map_err(|err| (err, root.clone()))?;
    store.set_traffic_log(traffic_log);
    Ok(Arc::new(store))
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
    let agent: SharedAgent;
    let bootstrap_gate: crate::interview::agent::BootstrapGate;
    {
        let (a, g) = agent_backend.create(traffic_log.clone());
        agent = a;
        bootstrap_gate = g;
    }

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
                window.on_window_should_close(cx, move |window, cx| {
                    persist_window_geometry(window, &paths_for_geometry);
                    let _ = transcript_for_close.close(cx);
                    history_for_close.close(cx);
                    interactive_for_close.close_all(cx);
                    true
                });
                match fleet_open {
                    Err((error, resolved_root)) => {
                        let view =
                            cx.new(|cx| FleetBlockedView::new(error, resolved_root, window, cx));
                        cx.new(|cx| Root::new(view, window, cx))
                    }
                    Ok(fleet) => {
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
                        let task_edit = cx.new(|cx| TaskEditView::new(window, cx, fleet.clone()));
                        let obligations =
                            cx.new(|cx| ObligationsView::new(window, cx, fleet.clone()));
                        let agent_panel = cx.new(|cx| {
                            AgentConfigPanelView::new(
                                window,
                                cx,
                                fleet.clone(),
                                agent.clone(),
                                interactive_agent_window.clone(),
                            )
                        });
                        let agent_for_sessions = agent.clone();
                        let gate_for_sessions = bootstrap_gate.clone();
                        let sessions = cx.new(|cx| {
                            SessionsView::new(
                                window,
                                cx,
                                agent_for_sessions,
                                gate_for_sessions,
                                fleet.clone(),
                            )
                        });
                        let settings = cx.new(|cx| SettingsView::new(window, cx));
                        let database = cx.new(|cx| DatabaseView::new(window, cx, fleet.clone()));
                        let view = cx.new(|cx| {
                            let _task_list_subscription =
                                cx.subscribe(&task_list, |this: &mut Shell, _, event, cx| {
                                    match event {
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
                                        TaskListEvent::OpenTaskEdit { task_id, .. } => {
                                            if this.task_edit.read(cx).is_open() {
                                                this.queue_retarget_task_edit(task_id.clone(), cx);
                                            } else {
                                                this.queue_open_task_edit(task_id.clone(), cx);
                                            }
                                        }
                                        TaskListEvent::OpenObligations { task_id, title } => {
                                            if this.obligations.read(cx).is_open() {
                                                this.queue_retarget_obligations(
                                                    task_id.clone(),
                                                    title.clone(),
                                                    cx,
                                                );
                                            } else {
                                                this.queue_open_obligations(
                                                    task_id.clone(),
                                                    title.clone(),
                                                    cx,
                                                );
                                            }
                                        }
                                        TaskListEvent::CloseTaskEdit => {
                                            this.queue_close_task_edit(cx);
                                        }
                                        TaskListEvent::CloseObligations => {
                                            this.queue_close_obligations(cx);
                                        }
                                        TaskListEvent::OpenLifecycle { lifecycle, .. } => {
                                            eprintln!("tod: lifecycle panel stub — {lifecycle}");
                                        }
                                        TaskListEvent::OpenAgentDetail { task_id, agent_id } => {
                                            if this.agent_panel.read(cx).is_open() {
                                                if let Some(agent_id) = agent_id.clone() {
                                                    this.queue_retarget_agent(
                                                        task_id.clone(),
                                                        agent_id,
                                                        cx,
                                                    );
                                                } else {
                                                    this.queue_open_agent(
                                                        task_id.clone(),
                                                        None,
                                                        cx,
                                                    );
                                                }
                                            } else {
                                                this.queue_open_agent(
                                                    task_id.clone(),
                                                    agent_id.clone(),
                                                    cx,
                                                );
                                            }
                                        }
                                        TaskListEvent::LaunchOrFocusAgent {
                                            task_id,
                                            config_id,
                                        } => {
                                            this.handle_launch_or_focus_agent(
                                                task_id.clone(),
                                                config_id.clone(),
                                                cx,
                                            );
                                        }
                                        TaskListEvent::CloseAgentPanel => {
                                            this.queue_close_agent_panel(cx);
                                        }
                                        TaskListEvent::OpenShell {
                                            task_id,
                                            shell_id,
                                            agent_id,
                                        } => {
                                            this.handle_open_shell(
                                                task_id.clone(),
                                                shell_id.clone(),
                                                agent_id.clone(),
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
                                            this.task_list.update(cx, |list, cx| {
                                                list.set_slide_edit_open(false, cx);
                                            });
                                            this.pending_refocus_task_list = true;
                                            cx.notify();
                                        }
                                        TaskEditEvent::Changed => {
                                            this.task_list.update(cx, |list, cx| {
                                                list.request_live_refresh(cx);
                                            });
                                        }
                                        TaskEditEvent::OpenObligations { task_id, title } => {
                                            this.queue_open_obligations(
                                                task_id.clone(),
                                                title.clone(),
                                                cx,
                                            );
                                        }
                                    }
                                });
                            let _obligations_subscription =
                                cx.subscribe(&obligations, |this: &mut Shell, _, event, cx| {
                                    match event {
                                        ObligationsEvent::Close => {
                                            this.task_list.update(cx, |list, cx| {
                                                list.set_obligations_open(false, cx);
                                            });
                                            this.pending_refocus_task_list = true;
                                            cx.notify();
                                        }
                                        ObligationsEvent::DeleteSelectedTask => {
                                            this.pending_delete_selected_task = true;
                                            cx.notify();
                                        }
                                    }
                                });
                            let _agent_panel_subscription =
                                cx.subscribe(&agent_panel, |this: &mut Shell, _, event, cx| {
                                    match event {
                                        AgentConfigPanelEvent::Close => {
                                            this.task_list.update(cx, |list, cx| {
                                                list.set_agent_panel_open(false, cx);
                                            });
                                            this.pending_refocus_task_list = true;
                                            cx.notify();
                                        }
                                        AgentConfigPanelEvent::Saved { .. }
                                        | AgentConfigPanelEvent::Deleted { .. } => {
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
                                task_edit,
                                obligations,
                                agent_panel,
                                sessions,
                                settings,
                                database,
                                fleet: fleet.clone(),
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
                                pending_open_lifecycle: None,
                                pending_return_to_tasks: false,
                                pending_open_task_edit: None,
                                pending_close_task_edit: false,
                                pending_retarget_task_edit: None,
                                pending_open_obligations: None,
                                pending_close_obligations: false,
                                pending_retarget_obligations: None,
                                pending_delete_selected_task: false,
                                pending_refocus_task_list: false,
                                pending_open_agent: None,
                                pending_launch_or_focus_agent: None,
                                pending_close_agent_panel: false,
                                pending_retarget_agent: None,
                                pending_error_toast: None,
                                always_on_top: restore_always_on_top,
                                tasks_split_state,
                                _task_list_subscription,
                                _task_edit_subscription,
                                _obligations_subscription,
                                _agent_panel_subscription,
                                _sessions_subscription,
                                _settings_subscription,
                            };
                            let poll_entity = cx.weak_entity();
                            cx.spawn(async move |_, cx| {
                                loop {
                                    Timer::after(std::time::Duration::from_millis(500)).await;
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

    if restore_always_on_top {
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
