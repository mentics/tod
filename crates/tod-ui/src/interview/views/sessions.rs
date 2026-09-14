use crate::interview::agent::SharedAgent;
use crate::interview::views::workspace::{WorkspaceEvent, WorkspaceView};
use crate::interview::{
    InterviewSession, InterviewSessionStatus, NewInterviewSession, SessionStore,
    TaskListProceedContext, TodPaths, TodSettings,
};
use crate::ui::app_nav::{AppDestination, AppNavMenu};
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::button::Button;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Sizable as _, Size, StyledExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tod_core::interview::driver::{DriverConfig, InterviewDriver};
use tod_core::interview::phase::base_interview_phase;
use tod_core::process_bundle::{ProcessManifest, TodInstallPaths, interview_session_prefix};
use tod_store::fleet::{FleetStore, ensure_interview_agent_for_node};
use tod_store::interview::Role;
use uuid::Uuid;

const SESSIONS_CONTEXT: &str = "InterviewSessions";

pub fn register_sessions_keyboard_bindings(_cx: &mut App) {
    // Interviews open directly from a task node, so this view only ever hosts a
    // `WorkspaceView`, which registers its own keyboard bindings.
}

#[derive(Debug, Clone)]
pub enum SessionsEvent {
    ReturnToTaskList,
    ProceedToLifecycle { task_id: String, lifecycle: String },
    /// Forwarded from the embedded obligations panel's chat icon.
    OpenAgentChat {
        node_id: Uuid,
        obligation_id: Option<Uuid>,
        config_id: Option<String>,
    },
}

/// Hosts the single active interview [`WorkspaceView`] for the shell's Interview
/// tab. Interviews are always opened for a specific outline node from the task
/// list (see `open_or_kickoff_for_entity`).
pub struct SessionsView {
    paths: TodPaths,
    fleet: Arc<FleetStore>,
    store: SessionStore,
    agent: SharedAgent,
    kickoff_status: SharedString,
    /// True while provisioning is in flight, so the fallback view can show a spinner.
    kickoff_in_progress: bool,
    focus_handle: FocusHandle,
    workspace: Option<Entity<WorkspaceView>>,
    /// Ids for **Proceed** → lifecycle panel, set for every open (always task-list-initiated).
    task_list_context: Option<TaskListProceedContext>,
    /// Interview drivers by session id. They outlive a workspace so switching
    /// between interviews never loses track of an agent turn in progress.
    drivers: HashMap<Uuid, Arc<Mutex<InterviewDriver>>>,
    _workspace_subscription: Option<Subscription>,
    /// Session provisioned by a background kickoff, waiting to be opened on the
    /// next render (opening needs `&mut Window`).
    pending_opened_session: Option<InterviewSession>,
    app_nav: AppNavMenu,
}

impl SessionsView {
    pub fn new(
        _window: &mut Window,
        cx: &mut Context<Self>,
        agent: SharedAgent,
        fleet: Arc<FleetStore>,
    ) -> Self {
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let store = SessionStore::open(fleet.clone());
        Self {
            paths,
            fleet,
            store,
            agent,
            kickoff_status: SharedString::default(),
            kickoff_in_progress: false,
            focus_handle: cx.focus_handle(),
            workspace: None,
            task_list_context: None,
            drivers: HashMap::new(),
            _workspace_subscription: None,
            pending_opened_session: None,
            app_nav: AppNavMenu::default(),
        }
    }

    pub fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    pub fn close_app_nav(&mut self, cx: &mut App) {
        self.app_nav.close();
        if let Some(workspace) = &self.workspace {
            workspace.update(cx, |view, _| view.close_app_nav());
        }
    }

    fn hide_workspace(&mut self, cx: &mut Context<Self>) {
        self.kickoff_status = SharedString::default();
        self.kickoff_in_progress = false;
        cx.notify();
    }

    /// Open the interview for `node_id` + base `phase`, creating its session if needed.
    pub fn open_or_kickoff_for_entity(
        &mut self,
        node_id: Uuid,
        phase: &str,
        entity_label: &str,
        phase_label: &str,
        task_list_context: Option<TaskListProceedContext>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let wanted_base = base_interview_phase(phase).to_string();
        self.task_list_context = task_list_context;

        if let Some(mut session) = self.find_session_for_node(node_id, &wanted_base) {
            if session.status == InterviewSessionStatus::Archived {
                if let Ok(updated) = self
                    .store
                    .set_status(session.id, InterviewSessionStatus::Active)
                {
                    session = updated;
                }
            }
            self.kickoff_status = format!("Opened: {}", session.display_name).into();
            self.open_workspace(session, window, cx);
            cx.notify();
            return;
        }

        self.kickoff_status = "Provisioning interview workspace…".into();
        self.kickoff_in_progress = true;
        self.workspace = None;
        self._workspace_subscription = None;
        cx.notify();

        // Provisioning does blocking disk/SQLite work, so it runs on the
        // background executor; the spinner paints meanwhile.
        let paths = self.paths.clone();
        let fleet = self.fleet.clone();
        let phase_owned = phase.to_string();
        let display_name = format!("{entity_label} — {phase_label}");
        let entity = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let outcome: Result<InterviewSession, String> = cx
                .background_spawn(async move {
                    let settings = TodSettings::load(&paths).map_err(|e| e.to_string())?;
                    let agent_ctx = ensure_interview_agent_for_node(
                        &fleet,
                        &paths,
                        &settings,
                        &node_id.to_string(),
                    )
                    .map_err(|e| e.to_string())?;
                    SessionStore::open(fleet.clone())
                        .insert_session_with_metadata(
                            NewInterviewSession {
                                node_id,
                                agent_config_id: Some(agent_ctx.agent.id.clone()),
                                display_name,
                                phase: phase_owned,
                            },
                            InterviewSessionStatus::Active,
                            Some(agent_ctx.agent.id),
                        )
                        .map_err(|e| e.to_string())
                })
                .await;
            let _ = entity.update(cx, |this, cx| {
                this.kickoff_in_progress = false;
                match outcome {
                    Ok(session) => this.pending_opened_session = Some(session),
                    Err(err) => {
                        this.kickoff_status = format!("Failed to create session: {err}").into()
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The node's active session for the phase, else its most recent complete one.
    fn find_session_for_node(&self, node_id: Uuid, wanted_base: &str) -> Option<InterviewSession> {
        let sessions = self.store.list_for_node(node_id).unwrap_or_default();
        let matching = sessions
            .into_iter()
            .filter(|s| base_interview_phase(&s.phase) == wanted_base);
        let mut best: Option<InterviewSession> = None;
        for session in matching {
            let rank = |s: &InterviewSession| match s.status {
                InterviewSessionStatus::Active => 2,
                InterviewSessionStatus::Complete => 1,
                InterviewSessionStatus::Archived => 0,
            };
            if best
                .as_ref()
                .is_none_or(|b| (rank(&session), session.updated_at) > (rank(b), b.updated_at))
            {
                best = Some(session);
            }
        }
        best
    }

    /// Descriptions of interview agent turns currently in flight (question
    /// maker and/or answer processor), across every driver kept alive by this
    /// view — not just the one for the session currently open — used by the
    /// app shell to warn before closing the window while one is still
    /// running.
    pub fn running_interview_work(&self) -> Vec<String> {
        let mut items = Vec::new();
        for driver in self.drivers.values() {
            let Ok(driver) = driver.lock() else { continue };
            let status = driver.status();
            let node_title = &driver.config().node_title;
            if status.question_maker_running {
                items.push(format!("Question maker running: {node_title}"));
            }
            if status.answer_lanes_busy > 0 {
                items.push(format!("Answer processor running: {node_title}"));
            }
        }
        items
    }

    fn driver_for(&mut self, session: &InterviewSession) -> Result<Arc<Mutex<InterviewDriver>>, String> {
        if let Some(driver) = self.drivers.get(&session.id) {
            return Ok(driver.clone());
        }
        let settings = TodSettings::load(&self.paths).unwrap_or_default();
        let agent_ctx = ensure_interview_agent_for_node(
            &self.fleet,
            &self.paths,
            &settings,
            &session.node_id.to_string(),
        )
        .map_err(|e| format!("Interview agent setup failed: {e}"))?;
        let install = TodInstallPaths::discover().map_err(|e| format!("Process bundle: {e}"))?;
        let manifest = ProcessManifest::load(&install).map_err(|e| format!("Process bundle: {e}"))?;
        let prefix = |role| {
            interview_session_prefix(&manifest, role, &session.phase)
                .map_err(|e| format!("Process bundle: {e}"))
        };
        let node_title = self
            .fleet
            .get_node(&session.node_id.to_string())
            .ok()
            .flatten()
            .map(|n| n.title)
            .unwrap_or_else(|| session.display_name.clone());
        let driver = Arc::new(Mutex::new(InterviewDriver::new(DriverConfig {
            node_id: session.node_id,
            node_title,
            interview_session_id: session.id,
            phase_key: session.phase.clone(),
            agent_config_id: session
                .agent_config_id
                .clone()
                .unwrap_or_else(|| agent_ctx.agent.id.clone()),
            repo_cwd: agent_ctx.cwd,
            data_root: self.fleet.paths().root().to_path_buf(),
            tod_cli: tod_core::interview::tod_cli_path(),
            launch: settings.interview_launch_options(),
            replenish_threshold: settings.question_maker.replenish_threshold,
            context: settings.interview_context.clone(),
            question_maker_prefix: prefix(Role::QuestionMaker)?,
            answer_processor_prefix: prefix(Role::AnswerProcessor)?,
        })));
        self.drivers.insert(session.id, driver.clone());
        Ok(driver)
    }

    fn open_workspace(
        &mut self,
        session: InterviewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(existing) = self.workspace.as_ref() {
            if existing.read(cx).interview_session().id == session.id {
                self.kickoff_status = SharedString::default();
                existing.update(cx, |view, cx| {
                    view.set_task_list_proceed(self.task_list_context.clone());
                    cx.focus_self(window);
                });
                cx.notify();
                return;
            }
        }

        let driver = match self.driver_for(&session) {
            Ok(driver) => driver,
            Err(message) => {
                self.workspace = None;
                self._workspace_subscription = None;
                self.kickoff_status = message.into();
                cx.notify();
                return;
            }
        };
        let agent = self.agent.clone();
        let task_list_proceed = self.task_list_context.clone();
        let fleet = self.fleet.clone();
        let workspace = cx.new(|cx| {
            WorkspaceView::new(session, window, cx, agent, fleet, driver, task_list_proceed)
        });
        let subscription = cx.subscribe(&workspace, |this, _workspace, event, cx| match event {
            WorkspaceEvent::NavigateBack => {
                this.hide_workspace(cx);
                cx.emit(SessionsEvent::ReturnToTaskList);
            }
            WorkspaceEvent::ProceedToLifecycle => {
                if let Some(ctx) = this.task_list_context.clone() {
                    this.hide_workspace(cx);
                    cx.emit(SessionsEvent::ProceedToLifecycle {
                        task_id: ctx.task_id,
                        lifecycle: ctx.lifecycle,
                    });
                }
            }
            WorkspaceEvent::SessionComplete => cx.notify(),
            WorkspaceEvent::OpenAgentChat {
                node_id,
                obligation_id,
                config_id,
            } => {
                cx.emit(SessionsEvent::OpenAgentChat {
                    node_id: *node_id,
                    obligation_id: *obligation_id,
                    config_id: config_id.clone(),
                });
            }
        });
        self.workspace = Some(workspace.clone());
        self._workspace_subscription = Some(subscription);
        self.kickoff_status = SharedString::default();
        workspace.update(cx, |_, cx| {
            cx.focus_self(window);
        });
        cx.notify();
    }
}

impl EventEmitter<SessionsEvent> for SessionsView {}

impl crate::ui::app_nav::HasAppNav for SessionsView {
    fn app_nav_mut(&mut self) -> &mut AppNavMenu {
        &mut self.app_nav
    }

    fn app_nav_current(&self) -> Option<AppDestination> {
        None
    }

    fn app_nav_fallback_focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Focusable for SessionsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SessionsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(session) = self.pending_opened_session.take() {
            self.open_workspace(session, window, cx);
        }

        if let Some(workspace) = &self.workspace {
            // Absolute fill gives Workspace a definite width/height; without it the
            // column row sized to content and the response column was clipped.
            return div()
                .relative()
                .size_full()
                .min_w_0()
                .overflow_hidden()
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .min_w_0()
                        .overflow_hidden()
                        .child(workspace.clone()),
                )
                .into_any_element();
        }

        // No workspace yet: provisioning is in flight, or it failed. Either way
        // always offer a way back to the task list.
        let background = cx.theme().background;
        let muted_foreground = cx.theme().muted_foreground;
        div()
            .key_context(SESSIONS_CONTEXT)
            .track_focus(&self.focus_handle)
            .v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(background)
            .when(self.kickoff_in_progress, |el| {
                el.child(Spinner::new().with_size(Size::Large))
            })
            .child(
                div().text_sm().text_color(muted_foreground).child(
                    crate::ui::selectable_text::selectable_text(
                        "sessions-kickoff-status",
                        if self.kickoff_status.is_empty() {
                            SharedString::from("No interview open.")
                        } else {
                            self.kickoff_status.clone()
                        },
                        window,
                        cx,
                    )
                    .text_color(muted_foreground),
                ),
            )
            .child(
                Button::new("sessions-back-to-tasks")
                    .label("Back to Tasks")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.kickoff_status = SharedString::default();
                        this.kickoff_in_progress = false;
                        cx.emit(SessionsEvent::ReturnToTaskList);
                    })),
            )
            .into_any_element()
    }
}
