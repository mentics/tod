use super::always_on_top;
use super::data_root_setup::DataRootSetupView;
use super::fleet_blocked::FleetBlockedView;
use super::no_focus;
#[cfg(feature = "agent-socket")]
use crate::agent_socket;
#[cfg(feature = "agent-socket")]
use crate::agent_socket::commands::AgentPlatformSocketCommand;
use crate::app::history_window::HistoryWindowControl;
use crate::app::transcript_window::TranscriptWindowControl;
use crate::cli::LaunchOptions;
use crate::interview::agent::{AgentBackend, AgentPlatform, SharedAgent};
use crate::interview::settings::{persist_window_geometry, resolve_open_window_bounds};
use crate::interview::views::{SettingsEvent, SettingsView};
use crate::interview::{TodPaths, TodSettings};
use crate::ui::actionable::render_shortcut_pill_in_context;
use crate::ui::agent_chat::OpenAgentChat;
use crate::ui::agent_runs::AgentRuns;
use crate::ui::report_problem::{
    self, OpenReportDialog, REPORT_DIALOG_CONTEXT, ReportDialogSubmit, ReportProblem,
};
use crate::ui::app_nav::{
    HasAppNav, ShellGoDatabase, ShellGoPullRequests, ShellGoSettings,
    ShellGoWorkbench, register_app_nav_keyboard_bindings,
};
use crate::ui::code_links::{OpenCodeRef, open_code_ref};
use crate::ui::key_context::NOT_INPUT;
use crate::ui::selectable_text::selectable_text;
use crate::ui::status::{self, StatusSource};
use crate::ui::toast::{error_toast, info_toast, notification_overlay, warning_toast};
use crate::views::database::DatabaseView;
use crate::views::lifecycle_control::LifecycleController;
use crate::views::pull_requests::PullRequestsView;
use crate::ui::nav_history::{
    NavHistory, NavigateBack, NavigateForward, register_nav_history_bindings,
};
use crate::unified::{UnifiedView, WorkbenchPlace};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::{
    ActiveTheme, Disableable, IconName, Root, Selectable, StyledExt, TitleBar, WindowExt, h_flex,
};
use std::path::PathBuf;
use std::sync::Arc;
use tod_agent::EngagementState;
use tod_core::run_transcript;
use tod_store::agent_traffic::{
    AgentStatusGroups, SharedAgentTrafficLog, format_status_bar, shared_log,
};
use tod_journey::JourneyKey;
use tod_store::fleet::{FleetLaunchError, FleetStore};
use uuid::Uuid;

actions!(
    shell,
    [ShellOpenAgentTranscripts, ShellOpenHistory, ShellUndo]
);

/// The status bar's fixed height: a compact button plus its padding.
const STATUS_BAR_HEIGHT: Pixels = px(36.);
/// The most characters of status text the bar shows before cutting it short.
const STATUS_BAR_MAX_CHARS: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellView {
    Settings,
    Database,
    /// The pull requests of the node selected in the workbench.
    PullRequests,
    /// The unified view ("Workbench") — `doc/ui/unified-view.md`.
    Unified,
}

/// Where Back and Forward take the user (`ui::nav_history`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Location {
    Workbench(WorkbenchPlace),
    /// Any other view, shown as it is when the user returns to it.
    View(ShellView),
}

impl Location {
    /// Two places in the workbench are the same kind: stepping through the
    /// tree's rows quickly keeps only the row the user settles on.
    fn same_kind(a: &Self, b: &Self) -> bool {
        matches!((a, b), (Location::Workbench(_), Location::Workbench(_)))
    }
}

pub struct Shell {
    active_view: ShellView,
    settings: Entity<SettingsView>,
    database: Entity<DatabaseView>,
    pull_requests: Entity<PullRequestsView>,
    unified: Entity<UnifiedView>,
    /// Where the user has been, for Back and Forward.
    history: NavHistory<Location>,
    fleet: Arc<FleetStore>,
    _mutation_socket: Option<tod_store::fleet::mutation_socket::PortFileGuard>,
    agent: SharedAgent,
    traffic_log: SharedAgentTrafficLog,
    transcript_window: TranscriptWindowControl,
    engagement: tod_agent::SharedEngagementRegistry,
    history_window: HistoryWindowControl,
    agent_status_text: SharedString,
    paths: TodPaths,
    migration_notice_dismissed: bool,
    pending_error_toast: Option<String>,
    pending_warning_toast: Option<String>,
    always_on_top: bool,
    _settings_subscription: Subscription,
    _unified_nav_subscription: Subscription,
}

/// Human-readable summary of background work that would be lost if the
/// window closed right now: agents mid-run.
fn collect_running_work(fleet: &FleetStore) -> Vec<SharedString> {
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
    items
}

impl Shell {
    /// Where the UI is, as Back and Forward record it.
    fn location(&self, cx: &App) -> Option<Location> {
        match self.active_view {
            ShellView::Unified => Some(Location::Workbench(self.unified.read(cx).place(cx))),
            view => Some(Location::View(view)),
        }
    }

    /// Tell the history where the UI is now.
    fn note_location(&mut self, cx: &mut Context<Self>) {
        let could = (self.history.can_go_back(), self.history.can_go_forward());
        match self.location(cx) {
            Some(location) => self.history.visit(location, std::time::Instant::now()),
            None => self.history.leave(),
        }
        if could != (self.history.can_go_back(), self.history.can_go_forward()) {
            cx.notify();
        }
    }

    /// Alt+Left / Alt+Right and the title bar's arrows.
    fn navigate(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.note_location(cx);
        let target = if back {
            self.history.back()
        } else {
            self.history.forward()
        };
        let Some(target) = target else {
            return;
        };
        match &target {
            Location::Workbench(place) => {
                self.select_view(ShellView::Unified, window, cx);
                self.unified
                    .update(cx, |unified, cx| unified.restore_place(place, window, cx));
            }
            Location::View(view) => self.select_view(*view, window, cx),
        }
        if let Some(reached) = self.location(cx) {
            self.history.arrive(reached);
        }
        cx.notify();
    }

    fn select_view(&mut self, view: ShellView, window: &mut Window, cx: &mut Context<Self>) {
        self.settings
            .update(cx, |settings, _| settings.app_nav_mut().close());
        self.database
            .update(cx, |database, _| database.app_nav_mut().close());
        self.pull_requests
            .update(cx, |pull_requests, _| pull_requests.app_nav_mut().close());
        self.unified
            .update(cx, |unified, cx| unified.close_app_nav(cx));
        if self.active_view == view {
            return;
        }
        let previous = self.active_view;
        self.active_view = view;
        crate::ui::journey::record_nav(
            cx,
            tod_journey::NavEvent::ViewSelected {
                view: format!("{view:?}"),
            },
        );
        match view {
            ShellView::Settings => {
                let focus = self.settings.read(cx).focus_handle(cx);
                focus.focus(window, cx);
            }
            ShellView::Database => {
                let focus = self.database.read(cx).focus_handle(cx);
                focus.focus(window, cx);
            }
            ShellView::PullRequests => {
                // The node selected in the workbench the user came from.
                let node = if previous == ShellView::Unified {
                    self.unified.read(cx).selected_node_with_title(cx)
                } else {
                    None
                };
                self.pull_requests
                    .update(cx, |pull_requests, cx| pull_requests.show(node, cx));
                let focus = self.pull_requests.read(cx).focus_handle(cx);
                focus.focus(window, cx);
            }
            ShellView::Unified => {
                self.unified
                    .update(cx, |unified, cx| unified.focus_tree(window, cx));
            }
        }
        cx.notify();
    }

    /// Keyboard actions reach the shell only through the focused element's
    /// ancestors. When the focused element is removed (a button in a panel
    /// that a click replaced, say) nothing is focused and every shortcut,
    /// Alt+Left included, goes nowhere until the user clicks something.
    /// Put focus back on the active view when that happens.
    fn restore_lost_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.focused(cx).is_some() || !window.is_window_active() {
            return;
        }
        match self.active_view {
            ShellView::Settings => self.settings.read(cx).focus_handle(cx).focus(window, cx),
            ShellView::Database => self.database.read(cx).focus_handle(cx).focus(window, cx),
            ShellView::PullRequests => self.pull_requests.read(cx).focus_handle(cx).focus(window, cx),
            ShellView::Unified => self
                .unified
                .update(cx, |unified, cx| unified.restore_focus(window, cx)),
        }
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
        if let Ok(registry) = self.engagement.lock() {
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

    /// Ctrl+J from a view that is not the workbench: go there, where the
    /// chat drawer is.
    fn on_open_agent_chat(
        &mut self,
        _: &OpenAgentChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_view(ShellView::Unified, window, cx);
        self.unified
            .update(cx, |unified, cx| unified.toggle_chat(window, cx));
    }

    /// Ctrl+Shift+R that no view handled: the workbench's selection, else
    /// the whole project.
    fn on_report_problem(&mut self, _: &ReportProblem, window: &mut Window, cx: &mut Context<Self>) {
        let key = match self.unified.read(cx).selected_node_with_title(cx) {
            Some((id, _)) => JourneyKey::Node(id),
            None => JourneyKey::Project,
        };
        self.on_open_report_dialog(&OpenReportDialog { key, conversation: None }, window, cx);
    }

    /// Handle [`OpenReportDialog`] dispatched by any view: opens the report
    /// dialog for `key`. Snapshots the app journey ring buffer synchronously
    /// on submit, then does the (potentially slow) screenshot capture and the
    /// journey write on a background thread so the UI never blocks.
    fn on_open_report_dialog(
        &mut self,
        action: &OpenReportDialog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Every way in (shortcut, menus, views) ends here; with reporting
        // not set up, none of them opens a dialog whose report goes nowhere.
        if !report_problem::is_available(cx) {
            return;
        }
        let key = action.key;
        let title: SharedString = match key {
            JourneyKey::Project => "Report a problem: project".into(),
            JourneyKey::Node(id) => {
                let node_title = self
                    .fleet
                    .get_node(&id.to_string())
                    .ok()
                    .flatten()
                    .map(|task| task.title)
                    .unwrap_or_else(|| id.to_string());
                format!("Report a problem: {node_title}").into()
            }
        };

        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .placeholder("What went wrong?")
        });
        let focus_handle = input.read(cx).focus_handle(cx);
        window.focus(&focus_handle, cx);

        let fleet = self.fleet.clone();
        let paths = self.paths.clone();

        window.open_dialog(cx, move |dialog, _window, _cx| {
            let input = input.clone();
            let input_for_submit = input.clone();
            let fleet = fleet.clone();
            let paths = paths.clone();
            dialog
                .title(title.clone())
                .overlay(true)
                .overlay_closable(true)
                .keyboard(true)
                .close_button(true)
                .child({
                    let fleet = fleet.clone();
                    let paths = paths.clone();
                    div()
                        .key_context(REPORT_DIALOG_CONTEXT)
                        .on_action(move |_: &ReportDialogSubmit, window, cx| {
                            let note = input_for_submit.read(cx).value().trim().to_string();
                            if note.is_empty() {
                                return;
                            }
                            submit_report(key, note, fleet.clone(), paths.clone(), window, cx);
                            window.close_dialog(cx);
                        })
                        .child(
                            div()
                                .w_full()
                                .h(px(120.))
                                .child(Textarea::new(&input).w_full().h(px(120.))),
                        )
                })
                .footer(
                    div().flex().justify_end().gap_2().child(
                        Button::new("report-dialog-submit").label("Report").primary().on_click({
                            let input = input.clone();
                            let fleet = fleet.clone();
                            let paths = paths.clone();
                            move |_, window, cx| {
                                let note = input.read(cx).value().trim().to_string();
                                if note.is_empty() {
                                    return;
                                }
                                submit_report(key, note, fleet.clone(), paths.clone(), window, cx);
                                window.close_dialog(cx);
                            }
                        }),
                    ),
                )
        });
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

    fn undo_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.fleet.undo_last() {
            Ok(Some(label)) => info_toast(window, cx, format!("Undid: {label}")),
            Ok(None) => info_toast(window, cx, "Nothing to undo"),
            Err(err) => error_toast(window, cx, format!("Undo failed: {err}")),
        }
    }

    /// The status bar's message: what the active view last posted to the
    /// status hub (`ui::status`); views that post nothing show none. The bar
    /// is one line, so a longer post (an error chain, say — its toast keeps
    /// the full text) shows only its first line, cut short.
    fn status_bar_message(&self, cx: &App) -> SharedString {
        // The workbench's node tree posts its messages under `Tasks`.
        let source = match self.active_view {
            ShellView::Unified => StatusSource::Tasks,
            ShellView::Settings | ShellView::Database | ShellView::PullRequests => {
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
                .child(
                    h_flex()
                        .ml(crate::ui::style::space::INLINE)
                        .child(
                            Button::new("nav-back")
                                .icon(IconName::ArrowLeft)
                                .ghost()
                                .compact()
                                .disabled(!self.history.can_go_back())
                                .tooltip("Back (Alt+Left)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.navigate(true, window, cx);
                                })),
                        )
                        .child(
                            Button::new("nav-forward")
                                .icon(IconName::ArrowRight)
                                .ghost()
                                .compact()
                                .disabled(!self.history.can_go_forward())
                                .tooltip("Forward (Alt+Right)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.navigate(false, window, cx);
                                })),
                        ),
                )
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
                        ))
                        .when(report_problem::is_available(cx), |bar| {
                            bar.child(
                                Button::new("title-report-problem")
                                    .icon(gpui_component::Icon::new(
                                        gpui_kit_assets::IconName::Flag,
                                    ))
                                    .label("Report a problem")
                                    .ghost()
                                    .compact()
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(ReportProblem), cx);
                                    }),
                            )
                            .children(render_shortcut_pill_in_context(
                                window,
                                &ReportProblem,
                                None,
                                cx,
                            ))
                        }),
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
        self.drain_pending_error_toast(window, cx);
        crate::ui::agent_permission::drain_queued_requests(window, cx);
        crate::ui::credential_request::drain_queued(window, cx);
        self.note_location(cx);
        self.restore_lost_focus(window, cx);

        div()
            .v_flex()
            .size_full()
            .relative()
            .on_action(cx.listener(Self::on_open_agent_chat))
            .on_action(cx.listener(|this, _: &NavigateBack, window, cx| {
                this.navigate(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NavigateForward, window, cx| {
                this.navigate(false, window, cx);
            }))
            .on_action(cx.listener(|this, action: &OpenCodeRef, window, cx| {
                // Text outside a view that knows its node: the workbench's
                // selection is what the user is looking at.
                let node = this
                    .unified
                    .read(cx)
                    .selected_node_with_title(cx)
                    .map(|(id, _)| id);
                open_code_ref(this.fleet.clone(), node, &action.target, window, cx);
            }))
            .on_action(cx.listener(Self::on_report_problem))
            .on_action(cx.listener(Self::on_open_report_dialog))
            .on_action(cx.listener(|this, _: &ShellGoSettings, window, cx| {
                this.select_view(ShellView::Settings, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShellGoDatabase, window, cx| {
                this.select_view(ShellView::Database, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShellGoPullRequests, window, cx| {
                this.select_view(ShellView::PullRequests, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShellGoWorkbench, window, cx| {
                this.select_view(ShellView::Unified, window, cx);
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

    fn render_content(&self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        match self.active_view {
            ShellView::Settings => self.settings.clone().into_any_element(),
            ShellView::Database => self.database.clone().into_any_element(),
            ShellView::PullRequests => self.pull_requests.clone().into_any_element(),
            ShellView::Unified => self.unified.clone().into_any_element(),
        }
    }
}

/// Finishes a report-a-problem submission (implementation plan Step 6c): the
/// app journey ring buffer is snapshotted synchronously (cheap, in-memory)
/// before this runs; screenshot capture and the journey write happen on a
/// background thread since the screenshot can be slow (Win32 GDI) and must
/// never block the UI. Toasts once the report is queued, and again once the
/// submission worker has tried to send it; anything that goes wrong along
/// the way is an error toast saying what.
fn submit_report(
    key: JourneyKey,
    note: String,
    fleet: Arc<FleetStore>,
    paths: TodPaths,
    window: &mut Window,
    cx: &mut App,
) {
    let app_journey: Vec<tod_journey::Record> = crate::ui::journey::hub(cx)
        .read(cx)
        .snapshot()
        .into_iter()
        .map(|entry| entry.record)
        .collect();
    let window_handle = window.window_handle();

    // How the worker's first attempt to send went, or `None` if it did not
    // happen within `REPORT_SEND_WAIT`.
    let (outcome_tx, outcome_rx) = async_channel::unbounded::<Option<Result<(), String>>>();
    cx.spawn(async move |cx| {
        let (app_journey, note) = (app_journey, note);
        let timeout_tx = outcome_tx.clone();
        let queued = cx
            .background_spawn(async move {
                let screenshot = crate::ui::screenshot::capture_app_screenshot()
                    .and_then(|img| {
                        crate::ui::screenshot::encode_png(&img)
                            .inspect_err(|err| {
                                tracing::warn!("report: encoding the screenshot failed: {err}")
                            })
                            .ok()
                    })
                    .map(|bytes| tod_journey::Blob {
                        mime: "image/png".into(),
                        bytes,
                    });
                let event = tod_journey::Event::Report {
                    note,
                    app_journey,
                    screenshot,
                };
                let seq =
                    tod_core::journey::record_and_get_seq(key, tod_journey::Actor::User, event);
                queue_report(seq, key, &fleet, &paths, outcome_tx)
            })
            .await;

        let listening = match queued {
            Ok(listening) => listening,
            Err(message) => {
                tracing::warn!("report: {message}");
                let _ = cx.update_window(window_handle, move |_view, window, cx| {
                    error_toast(window, cx, message);
                });
                return;
            }
        };
        if !listening {
            // No submission worker to hear back from (it starts with the
            // store, so only when that failed to open).
            let _ = cx.update_window(window_handle, |_view, window, cx| {
                info_toast(window, cx, "Report recorded and queued to send");
            });
            return;
        }
        let _ = cx.update_window(window_handle, |_view, window, cx| {
            info_toast(window, cx, "Report recorded, sending…");
        });
        cx.background_spawn({
            let timer = cx.background_executor().timer(REPORT_SEND_WAIT);
            async move {
                timer.await;
                let _ = timeout_tx.try_send(None);
            }
        })
        .detach();
        let outcome = outcome_rx.recv().await.ok().flatten();
        let _ = cx.update_window(window_handle, move |_view, window, cx| match outcome {
            Some(Ok(())) => info_toast(window, cx, "Report sent"),
            Some(Err(err)) => error_toast(
                window,
                cx,
                format!(
                    "The report could not be sent: {err}. It stays queued, and tod keeps \
                     trying while it is running."
                ),
            ),
            None => warning_toast(
                window,
                cx,
                "The report has not been sent yet. It stays queued, and tod keeps trying \
                 while it is running.",
            ),
        });
    })
    .detach();
}

/// How long [`submit_report`] waits to hear how the first send went.
const REPORT_SEND_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Queues a just-recorded report for the submission worker (spec §9.6), and
/// has the worker send how its first attempt went on `outcome`. Runs on a
/// background thread.
///
/// `Ok(false)` when no worker is running to hear back from; `Err` says why
/// the report could not be queued at all.
fn queue_report(
    seq: Option<u64>,
    key: JourneyKey,
    fleet: &FleetStore,
    paths: &TodPaths,
    outcome: async_channel::Sender<Option<Result<(), String>>>,
) -> Result<bool, String> {
    let seq = seq.ok_or(
        "The report could not be recorded: the journey writer is not running or could not \
         open the journey file (see the log)",
    )?;
    let settings = TodSettings::load(paths)
        .map_err(|err| format!("The report was recorded but not sent: reading settings: {err:#}"))?;
    if !settings.journeys.can_submit() {
        return Err("The report was recorded but not sent: sending journeys is not set up in \
                    Settings"
            .to_string());
    }

    // The queue entry gets its own bundle id now (the worker builds the
    // bundle itself later) so the journey's `Submission` event can point at
    // it. Listen for the worker before queuing, so its attempt can't be
    // missed.
    let bundle_id = Uuid::new_v4();
    let listening = tod_core::journey::on_next_send_attempt(
        bundle_id,
        Box::new(move |result| {
            let _ = outcome.try_send(Some(result));
        }),
    );
    let node_id = match key {
        JourneyKey::Project => None,
        JourneyKey::Node(id) => Some(id),
    };
    let entry = fleet
        .queue_journey_submission(bundle_id, node_id, seq as i64, "report")
        .map_err(|err| {
            format!("The report was recorded but could not be queued to send: {err:#}")
        })?;
    tod_core::journey::record(
        key,
        tod_journey::Actor::App,
        tod_journey::Event::Submission {
            bundle: entry.bundle_id,
            status: "queued".to_string(),
        },
    );
    Ok(listening)
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
    tod_agent::claude_adapter::set_local_dir(tod_store::install::claude_adapter_dir());
    // Whether a missing or outdated Claude adapter is worth saying at start.
    let uses_claude = !matches!(agent_backend, AgentBackend::Mock)
        && tod_store::AgentRole::ALL
            .iter()
            .any(|role| app_settings.platform_for(*role) == AgentPlatform::Claude);
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
    let engagement = tod_agent::shared_engagement_registry();
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
            ..super::app_icon::window_options()
        },
        {
            let paths = paths.clone();
            let transcript_window = transcript_window.clone();
            let history_window = history_window.clone();
            #[cfg(feature = "agent-socket")]
            let shell_for_socket = shell_for_socket.clone();
            move |window, cx| {
                let paths_for_geometry = paths.clone();
                let transcript_for_close = transcript_window.clone();
                let history_for_close = history_window.clone();
                match fleet_open {
                    Err((error, resolved_root)) => {
                        window.on_window_should_close(cx, move |window, cx| {
                            persist_window_geometry(window, &paths_for_geometry);
                            let _ = transcript_for_close.close(cx);
                            history_for_close.close(cx);
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
                        tod_core::cloud_sync::sync_on_start(fleet.clone());
                        tod_core::cloud_notify::start(fleet.clone());
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
                        report_problem::set_available_from(&app_settings.journeys, cx);
                        transcript_window.bind(fleet.clone(), traffic_log.clone());
                        history_window.bind(fleet.clone());
                        let _ = crate::interview::bootstrap(fleet.clone());
                        // Before the tree is built, so no row is ever drawn as
                        // "refreshing…" for a refresh the last run never finished.
                        if let Err(err) = tod_core::generator::clear_interrupted_refreshes(&fleet) {
                            tracing::error!("clearing interrupted generator refreshes failed: {err}");
                        }
                        let lifecycle = cx.new(|_| LifecycleController::new(fleet.clone()));
                        let agent_runs = cx.new(|_| AgentRuns::new(fleet.clone(), agent.clone()));
                        crate::ui::credential_request::start_watcher(
                            crate::ui::credential_request::Ctx {
                                fleet: fleet.clone(),
                                data_root: paths.data_root().to_path_buf(),
                                agent_runs: agent_runs.clone(),
                            },
                            cx,
                        );
                        let settings = cx.new(|cx| SettingsView::new(window, cx));
                        let database = cx.new(|cx| DatabaseView::new(window, cx, fleet.clone()));
                        let pull_requests = cx.new(|cx| {
                            PullRequestsView::new(cx, fleet.clone(), paths.data_root().to_path_buf())
                        });
                        let unified = cx.new(|cx| {
                            UnifiedView::new(
                                window,
                                cx,
                                fleet.clone(),
                                paths.clone(),
                                agent.clone(),
                                agent_runs.clone(),
                                lifecycle.clone(),
                            )
                        });
                        let view = cx.new(|cx| {
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
                            let _unified_nav_subscription =
                                cx.observe(&unified, |this: &mut Shell, _, cx| {
                                    this.note_location(cx);
                                });
                            let shell = Shell {
                                active_view: ShellView::Unified,
                                settings,
                                database,
                                pull_requests,
                                unified,
                                history: NavHistory::new(Location::same_kind),
                                fleet: fleet.clone(),
                                _mutation_socket: mutation_socket,
                                agent: agent.clone(),
                                traffic_log: traffic_log.clone(),
                                transcript_window: transcript_window.clone(),
                                engagement: engagement.clone(),
                                history_window: history_window.clone(),
                                agent_status_text,
                                paths: paths.clone(),
                                migration_notice_dismissed: false,
                                pending_error_toast: None,
                                pending_warning_toast: None,
                                always_on_top: restore_always_on_top,
                                _settings_subscription,
                                _unified_nav_subscription,
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
                            // Claude's adapter: tod's own install is brought up
                            // to date; a missing one, or an outdated global one,
                            // is said at once when Claude is in use.
                            let adapter_entity = cx.weak_entity();
                            cx.spawn(async move |_, cx| {
                                cx.update(crate::ui::claude_adapter::check).await;
                                if !uses_claude {
                                    return;
                                }
                                let _ = adapter_entity.update(cx, |shell, cx| {
                                    let adapter = crate::ui::claude_adapter::state(cx);
                                    let message = adapter.read(cx).startup_message();
                                    match message {
                                        Some((crate::ui::claude_adapter::Severity::Error, text)) => {
                                            shell.queue_error_toast(text, cx)
                                        }
                                        Some((_, text)) => shell.queue_warning_toast(text, cx),
                                        None => {}
                                    }
                                });
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
                        window.on_window_should_close(cx, move |window, cx| {
                            let running = collect_running_work(&fleet_for_close);
                            if running.is_empty() {
                                persist_window_geometry(window, &paths_for_geometry);
                                let _ = transcript_for_close.close(cx);
                                history_for_close.close(cx);
                                true
                            } else {
                                let paths_for_force = paths_for_geometry.clone();
                                let transcript_for_force = transcript_for_close.clone();
                                let history_for_force = history_for_close.clone();
                                crate::ui::toast::close_guard_toast(
                                    window,
                                    cx,
                                    running,
                                    move |window, cx| {
                                        persist_window_geometry(window, &paths_for_force);
                                        let _ = transcript_for_force.close(cx);
                                        history_for_force.close(cx);
                                        window.remove_window();
                                    },
                                );
                                false
                            }
                        });
                        // The app opens on the workbench with keys on the
                        // node tree, whatever took focus while the views
                        // were being built.
                        view.update(cx, |shell, cx| {
                            shell
                                .unified
                                .update(cx, |unified, cx| unified.focus_tree(window, cx));
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
        // `open_window` builds the root view synchronously, so the slot is
        // set by now unless the window is not a `Shell` (`FleetBlockedView`).
        let shell_weak = shell_for_socket.lock().ok().and_then(|slot| slot.clone());
        match shell_weak {
            Some(shell_weak) => {
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
            // No shell to drive: skip the socket instead of crashing the app.
            None => {
                tracing::error!(
                    "agent socket: no shell entity became available; control socket not started"
                );
            }
        }
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
            ..super::app_icon::window_options()
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
    register_nav_history_bindings(cx);
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-a", ShellOpenAgentTranscripts, Some(NOT_INPUT)),
        KeyBinding::new("ctrl-shift-h", ShellOpenHistory, Some(NOT_INPUT)),
        KeyBinding::new("ctrl-z", ShellUndo, Some(NOT_INPUT)),
    ]);
}
