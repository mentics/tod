use crate::interview::agent::{AgentRunState, BootstrapGate, SharedAgent};
use crate::interview::config::{
    sync_scaffolding_from_disk, sync_scaffolding_from_disk_after_bootstrap,
};
use crate::interview::views::workspace::{WorkspaceEvent, WorkspaceInFlightState, WorkspaceView};
use crate::interview::{
    InterviewSession, InterviewSessionStatus, NewInterviewSession, SessionStore,
    TaskListProceedContext, TodPaths, TodSettings,
};
use crate::ui::app_nav::{AppDestination, AppNavMenu};
use crate::ui::toast::confirm_toast;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::button::Button;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Sizable as _, Size, StyledExt};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tod_core::process_bundle::{AgentLaunchContext, ProcessManifest, TodInstallPaths};
use tod_store::fleet::{FleetStore, ensure_interview_agent_for_node};
use uuid::Uuid;

const SESSIONS_CONTEXT: &str = "InterviewSessions";

pub fn register_sessions_keyboard_bindings(_cx: &mut App) {
    // Navigation-mode / edit-mode bindings for the interview list were removed
    // along with the standalone session list UI — interviews now open directly
    // from a task node, so this view only ever hosts a `WorkspaceView`, which
    // registers its own keyboard bindings.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceReturnTarget {
    TaskList,
}

#[derive(Debug, Clone)]
pub enum SessionsEvent {
    ReturnToTaskList,
    ProceedToLifecycle { task_id: String, lifecycle: String },
}

/// Hosts the single active interview [`WorkspaceView`] for the shell's Interview
/// tab. Interviews are always opened for a specific outline node from the task
/// list (see `open_or_kickoff_for_entity`) — there is no standalone list or
/// "new interview" picker; an interview cannot be created without a node.
pub struct SessionsView {
    paths: TodPaths,
    fleet: Arc<FleetStore>,
    store: SessionStore,
    sessions: Vec<InterviewSession>,
    agent: SharedAgent,
    bootstrap_gate: BootstrapGate,
    /// SQLite session ids with a bootstrap thread already running.
    bootstrap_sessions: Arc<Mutex<HashSet<Uuid>>>,
    kickoff_status: SharedString,
    /// True while a kickoff/provisioning call is in flight, so the fallback
    /// view can show a spinner instead of leaving the user staring at static
    /// text with no sign anything is happening.
    kickoff_in_progress: bool,
    focus_handle: FocusHandle,
    workspace: Option<Entity<WorkspaceView>>,
    workspace_return_target: WorkspaceReturnTarget,
    /// Ids for **Proceed** → lifecycle panel, set for every open (always task-list-initiated).
    task_list_context: Option<TaskListProceedContext>,
    /// Pending submit state for sessions whose workspace was replaced (e.g. user
    /// opened a different interview). Restored on reopen.
    in_flight_by_session: HashMap<Uuid, WorkspaceInFlightState>,
    _workspace_subscription: Option<Subscription>,
    /// Deferred bootstrap prompt after workspace detects missing scaffolding.
    pending_bootstrap_prompt: Option<InterviewSession>,
    /// Session provisioned by a background kickoff, waiting to be opened on the
    /// next render (opening needs `&mut Window`, which the async completion
    /// callback doesn't have).
    pending_opened_session: Option<InterviewSession>,
    app_nav: AppNavMenu,
}

impl SessionsView {
    pub fn new(
        _window: &mut Window,
        cx: &mut Context<Self>,
        agent: SharedAgent,
        bootstrap_gate: BootstrapGate,
        fleet: Arc<FleetStore>,
    ) -> Self {
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let store = SessionStore::open(fleet.clone());
        let sessions = store.list_sessions().unwrap_or_default();

        Self {
            paths,
            fleet,
            store,
            sessions,
            agent,
            bootstrap_gate,
            bootstrap_sessions: Arc::new(Mutex::new(HashSet::new())),
            kickoff_status: SharedString::default(),
            kickoff_in_progress: false,
            focus_handle: cx.focus_handle(),
            workspace: None,
            workspace_return_target: WorkspaceReturnTarget::TaskList,
            task_list_context: None,
            in_flight_by_session: HashMap::new(),
            _workspace_subscription: None,
            pending_bootstrap_prompt: None,
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
        self.reload();
        self.kickoff_status = SharedString::default();
        self.kickoff_in_progress = false;
        cx.notify();
    }

    fn reload(&mut self) {
        self.sessions = self.store.list_sessions().unwrap_or_default();
    }

    /// Open an active interview for `node_id` + base `phase`, or insert a new session.
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
        let wanted_base = crate::interview::config::base_interview_phase(phase);

        self.reload();
        self.hide_workspace_if_other_node(node_id, cx);

        self.workspace_return_target = WorkspaceReturnTarget::TaskList;
        self.task_list_context = task_list_context;

        let existing = self.find_best_session_for_node(node_id, wanted_base);

        if let Some(session) = existing {
            if Self::session_is_mock_scaffold(&session) {
                let _ = self
                    .store
                    .set_status(session.id, InterviewSessionStatus::Archived);
                self.reload();
            } else {
                let mut session = session;
                if session.status != InterviewSessionStatus::Active {
                    let _ = self
                        .store
                        .set_status(session.id, InterviewSessionStatus::Active);
                    self.reload();
                    session = self
                        .sessions
                        .iter()
                        .find(|s| s.id == session.id)
                        .cloned()
                        .unwrap_or(session);
                }
                self.kickoff_status = format!("Opened: {}", session.display_name).into();
                if self.session_needs_bootstrap(&session) {
                    self.start_question_maker_bootstrap(session.clone());
                }
                self.open_workspace(session, window, cx);
                cx.notify();
                return;
            }
        }

        self.kickoff_status = "Provisioning interview workspace…".into();
        self.kickoff_in_progress = true;
        cx.notify();

        // Provisioning does blocking disk/SQLite work, so it runs on a background
        // thread — doing it inline here would finish before GPUI ever paints a
        // frame with `kickoff_in_progress` set, and the spinner would never
        // actually appear on screen.
        let paths = self.paths.clone();
        let fleet = self.fleet.clone();
        let node_id_str = node_id.to_string();
        let phase_owned = phase.to_string();
        let display_name = format!("{entity_label} — {phase_label}");
        let entity = cx.weak_entity();

        cx.spawn(async move |_, cx| {
            let display_name_for_thread = display_name.clone();
            // `cx.background_spawn` runs this on the background executor's thread
            // pool and returns a `Task` we can `.await` here without blocking the
            // foreground (UI) thread this `cx.spawn` future itself runs on — unlike
            // `std::thread::spawn(..).join()`, which would block that same UI
            // thread and prevent GPUI from ever painting the spinner frame.
            let outcome: Result<InterviewSession, String> = cx
                .background_spawn(async move {
                    let settings = TodSettings::load(&paths).map_err(|e| e.to_string())?;
                    let agent_ctx =
                        ensure_interview_agent_for_node(&fleet, &paths, &settings, &node_id_str)
                            .map_err(|e| e.to_string())?;
                    let store = SessionStore::open(fleet.clone());
                    store
                        .insert_session_with_metadata(
                            NewInterviewSession {
                                node_id,
                                agent_config_id: Some(agent_ctx.agent.id.clone()),
                                display_name: display_name_for_thread,
                                phase: phase_owned,
                            },
                            InterviewSessionStatus::Active,
                            Some(agent_ctx.agent.id),
                        )
                        .map_err(|e| e.to_string())
                })
                .await;

            let _ = entity.update(cx, |this, cx| match outcome {
                Ok(session) => {
                    this.kickoff_status = format!("Kickoff started: {display_name}").into();
                    this.kickoff_in_progress = false;
                    this.reload();
                    this.start_question_maker_bootstrap(session.clone());
                    this.pending_opened_session = Some(session);
                    cx.notify();
                }
                Err(err) => {
                    this.kickoff_status = format!("Failed to create session: {err}").into();
                    this.kickoff_in_progress = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn session_matches_node_phase(
        session: &InterviewSession,
        node_id: Uuid,
        wanted_base: &str,
    ) -> bool {
        if session.node_id != node_id {
            return false;
        }
        let session_base = crate::interview::config::base_interview_phase(&session.phase);
        wanted_base.is_empty() || session_base == wanted_base
    }

    /// Open question files still on disk for this session (0 if unbound / unreadable).
    fn session_open_question_count(session: &InterviewSession) -> usize {
        let Some(scratch) = session.scratchpad_path.as_ref() else {
            return 0;
        };
        let cfg_path = Path::new(scratch).join("interview-config.md");
        if !cfg_path.exists() {
            return 0;
        }
        let Ok(config) = crate::interview::config::parse_interview_config(&cfg_path) else {
            return 0;
        };
        crate::interview::queue::load_queue_dir(&config.queue)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    fn find_best_session_for_node(
        &self,
        node_id: Uuid,
        wanted_base: &str,
    ) -> Option<InterviewSession> {
        let matches: Vec<&InterviewSession> = self
            .sessions
            .iter()
            .filter(|s| Self::session_matches_node_phase(s, node_id, wanted_base))
            .collect();
        if matches.is_empty() {
            return None;
        }

        if let Some(session) = matches
            .iter()
            .filter(|s| Self::session_open_question_count(s) > 0)
            .max_by_key(|s| {
                (
                    s.status == InterviewSessionStatus::Active,
                    Self::session_open_question_count(s),
                    s.id,
                )
            })
        {
            return Some((*session).clone());
        }

        if let Some(session) = matches
            .iter()
            .filter(|s| {
                s.status == InterviewSessionStatus::Active && self.session_needs_bootstrap(s)
            })
            .max_by_key(|s| s.id)
        {
            return Some((*session).clone());
        }

        if let Some(session) = matches
            .iter()
            .filter(|s| s.status == InterviewSessionStatus::Active)
            .max_by_key(|s| s.id)
        {
            return Some((*session).clone());
        }

        matches
            .iter()
            .filter(|s| s.status == InterviewSessionStatus::Complete)
            .max_by_key(|s| s.id)
            .map(|s| (*s).clone())
    }

    fn hide_workspace_if_other_node(&mut self, node_id: Uuid, cx: &App) {
        let Some(workspace) = self.workspace.as_ref() else {
            return;
        };
        let other = {
            let session = workspace.read(cx).interview_session();
            session.node_id != node_id
        };
        if other {
            self.stash_workspace_in_flight(cx);
            self.workspace = None;
            self._workspace_subscription = None;
        }
    }

    fn session_needs_bootstrap(&self, session: &InterviewSession) -> bool {
        !tod_core::process_bundle::session_has_scaffolding(self.paths.data_root(), session)
    }

    /// True when scaffolding was produced by `--agent mock` (fixtures), not a real question maker.
    fn session_is_mock_scaffold(session: &InterviewSession) -> bool {
        if session
            .session_id
            .as_deref()
            .is_some_and(|s| s.contains("interview-interview"))
        {
            return true;
        }
        let Some(scratch) = session.scratchpad_path.as_ref() else {
            return false;
        };
        let cfg_path = Path::new(scratch).join("interview-config.md");
        let Ok(config) = crate::interview::config::parse_interview_config(&cfg_path) else {
            return false;
        };
        let Ok(entries) = std::fs::read_dir(&config.queue) else {
            return false;
        };
        entries.flatten().take(8).any(|entry| {
            std::fs::read_to_string(entry.path())
                .map(|c| c.contains("mock-bootstrap") || c.contains("Mock MC question"))
                .unwrap_or(false)
        })
    }

    fn bootstrap_in_flight(&self, session_id: Uuid) -> bool {
        self.bootstrap_sessions
            .lock()
            .expect("bootstrap sessions lock")
            .contains(&session_id)
    }

    fn should_prompt_bootstrap(&self, session: &InterviewSession) -> bool {
        self.session_needs_bootstrap(session) && !self.bootstrap_in_flight(session.id)
    }

    fn bootstrap_subject_label(session: &InterviewSession) -> SharedString {
        if let Some(prefix) = session.display_name.split('—').next() {
            let trimmed = prefix.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string().into();
            }
        }
        session.node_id.to_string().into()
    }

    fn prompt_bootstrap_setup(
        &mut self,
        session: InterviewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subject = Self::bootstrap_subject_label(&session);
        let message = format!("{subject} has not been set up yet. Do you want me to set it up?");
        let view = cx.entity().downgrade();
        let session_for_yes = session.clone();

        confirm_toast(
            window,
            cx,
            "Interview not set up",
            message,
            move |window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.accept_bootstrap_setup(session_for_yes.clone(), window, cx);
                });
            },
            |_window, _cx| {
                // Nothing to show yet — the caller (task list) stays where it is.
            },
        );
    }

    fn accept_bootstrap_setup(
        &mut self,
        session: InterviewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_question_maker_bootstrap(session.clone());
        self.open_workspace(session, window, cx);
    }

    fn start_question_maker_bootstrap(&self, session: InterviewSession) {
        let settings = TodSettings::load(&self.paths).unwrap_or_default();
        let agent_ctx = match ensure_interview_agent_for_node(
            &self.fleet,
            &self.paths,
            &settings,
            &session.node_id.to_string(),
        ) {
            Ok(ctx) => ctx,
            Err(err) => {
                tracing::error!("interview agent provision failed: {err:#}");
                return;
            }
        };
        let agent_config_id = session
            .agent_config_id
            .clone()
            .unwrap_or_else(|| agent_ctx.agent.id.clone());
        let workspace_cwd = agent_ctx.cwd;
        let install = match TodInstallPaths::discover() {
            Ok(p) => p,
            Err(err) => {
                tracing::error!("process bundle not found: {err:#}");
                return;
            }
        };
        let manifest = match ProcessManifest::load(&install) {
            Ok(m) => m,
            Err(err) => {
                tracing::error!("process manifest load failed: {err:#}");
                return;
            }
        };
        let scratchpad =
            tod_core::process_bundle::resolve_session_scratchpad(self.paths.data_root(), &session);
        let ctx = {
            let fleet_projection = self.fleet.projection();
            let guard = fleet_projection.lock().expect("fleet projection mutex");
            let conn = guard.connection();
            match AgentLaunchContext::question_maker_bootstrap(
                &conn,
                &install,
                &manifest,
                &self.paths,
                &session,
                &scratchpad,
            ) {
                Ok(c) => c,
                Err(err) => {
                    tracing::error!("bootstrap launch context failed: {err:#}");
                    return;
                }
            }
        };
        let prompt = ctx.prompt;
        let cwd = workspace_cwd;
        let question_maker_settings = settings.question_maker.clone();
        let launch_options = settings.interview_launch_options();
        let agent = self.agent.clone();
        let bootstrap_gate = self.bootstrap_gate.clone();
        let bootstrap_sessions = self.bootstrap_sessions.clone();
        let fleet = self.fleet.clone();
        let store_paths = self.paths.clone();
        let session_id = session.id;
        {
            let mut in_flight = bootstrap_sessions.lock().expect("bootstrap sessions lock");
            if !in_flight.insert(session_id) {
                tracing::debug!(
                    event = "interview",
                    action = "bootstrap_already_running",
                    session_id = %session_id.to_string(),
                    "bootstrap already in flight for session"
                );
                return;
            }
        }
        tracing::info!(
            event = "interview",
            action = "bootstrap_start",
            session_id = %session_id.to_string(),
            cwd = %cwd.display(),
            phase = %session.phase,
            node_id = %session.node_id.to_string(),
            prompt_chars = prompt.session_prefix.len() + prompt.turn.len(),
            "question maker bootstrap thread starting"
        );
        bootstrap_gate.store(true, Ordering::SeqCst);
        let agent_config_id_for_thread = agent_config_id.clone();
        std::thread::spawn(move || {
            struct BootstrapGuard {
                sessions: Arc<Mutex<HashSet<Uuid>>>,
                gate: BootstrapGate,
                session_id: Uuid,
            }
            impl Drop for BootstrapGuard {
                fn drop(&mut self) {
                    let remaining = {
                        let mut sessions = self.sessions.lock().expect("bootstrap sessions lock");
                        sessions.remove(&self.session_id);
                        sessions.len()
                    };
                    self.gate.store(remaining > 0, Ordering::SeqCst);
                }
            }
            let _bootstrap_guard = BootstrapGuard {
                sessions: bootstrap_sessions,
                gate: bootstrap_gate,
                session_id,
            };
            let data_root = store_paths.data_root().to_path_buf();
            let handle = {
                let mut provider = agent.lock().expect("agent lock");
                provider.start_question_maker_replenishment(
                    &agent_config_id_for_thread,
                    cwd,
                    prompt,
                    &tod_core::settings::question_maker_pool(&question_maker_settings),
                    launch_options,
                )
            };
            let Ok(handle) = handle else {
                tracing::error!(
                    event = "interview",
                    action = "bootstrap_start_failed",
                    session_id = %session_id.to_string(),
                    "question maker bootstrap failed to start"
                );
                eprintln!("tod: question maker bootstrap failed to start for session {session_id}");
                return;
            };

            // Poll disk for interview-config while ACP runs, and keep trying after ACP
            // finishes until paths bind (or timeout). One-shot sync after ACP alone races
            // when the agent returns slightly before files are visible, or SQLITE_BUSY
            // swallows a single update attempt.
            let deadline = Instant::now() + Duration::from_secs(360);
            let mut agent_finished = false;
            let mut agent_failed = false;
            let mut synced = false;
            let mut last_sync_log = Instant::now() - Duration::from_secs(10);
            while Instant::now() < deadline {
                if !agent_finished {
                    let finished = {
                        let mut provider = agent.lock().expect("agent lock");
                        provider
                            .poll_run(handle.id)
                            .is_some_and(|state| !matches!(state, AgentRunState::InFlight))
                    };
                    if finished {
                        agent_finished = true;
                        let state = {
                            let mut provider = agent.lock().expect("agent lock");
                            provider.poll_run(handle.id)
                        };
                        agent_failed = matches!(state, Some(AgentRunState::Failure(_)));
                        tracing::info!(
                            event = "interview",
                            action = "bootstrap_agent_finished",
                            session_id = %session_id.to_string(),
                            ?state,
                            "bootstrap ACP run left InFlight"
                        );
                        // Keep bootstrap_sessions membership until this thread exits so the
                        // workspace does not emit NeedsBootstrap before disk sync binds.
                    }
                }

                if agent_failed && !synced {
                    break;
                }

                if !synced {
                    let store = SessionStore::open(fleet.clone());
                    let sync_result = if agent_finished {
                        sync_scaffolding_from_disk_after_bootstrap(&store, &data_root, session_id)
                    } else {
                        sync_scaffolding_from_disk(&store, &data_root, session_id)
                    };
                    match sync_result {
                        Ok(true) => {
                            synced = true;
                            tracing::info!(
                                event = "interview",
                                action = "bootstrap_synced",
                                session_id = %session_id.to_string(),
                                agent_finished,
                                "scaffolding paths bound in SQLite"
                            );
                        }
                        Ok(false) => {
                            if last_sync_log.elapsed() >= Duration::from_secs(5) {
                                tracing::debug!(
                                    event = "interview",
                                    action = "bootstrap_sync_pending",
                                    session_id = %session_id.to_string(),
                                    agent_finished,
                                    "no matching interview-config yet"
                                );
                                last_sync_log = Instant::now();
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                event = "interview",
                                action = "bootstrap_sync_error",
                                session_id = %session_id.to_string(),
                                error = %err,
                                "scaffolding sync error"
                            );
                            eprintln!(
                                "tod: scaffolding sync error for session {session_id}: {err}"
                            );
                        }
                    }
                }

                if synced && agent_finished {
                    break;
                }
                // Keep polling until agent finishes even after sync, so the gate stays
                // held while bootstrap is still writing queue files.
                if synced && !agent_finished {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
                std::thread::sleep(Duration::from_millis(500));
            }

            if !synced {
                tracing::error!(
                    event = "interview",
                    action = "bootstrap_sync_timeout",
                    session_id = %session_id.to_string(),
                    agent_finished,
                    "scaffolding sync timed out"
                );
                eprintln!(
                    "tod: scaffolding sync timed out for session {session_id} (agent_finished={agent_finished})"
                );
            }
        });
    }

    fn open_workspace(
        &mut self,
        session: InterviewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reload();
        let session = self
            .sessions
            .iter()
            .find(|s| s.id == session.id)
            .cloned()
            .unwrap_or(session);
        if self.should_prompt_bootstrap(&session) {
            self.hide_workspace_if_other_node(session.node_id, cx);
            self.prompt_bootstrap_setup(session, window, cx);
            return;
        }

        // Reuse the cached workspace for this session so submitted-in-flight
        // questions stay pending across Back → Open (req 7).
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

        self.stash_workspace_in_flight(cx);

        let restored = self.in_flight_by_session.remove(&session.id);
        let agent = self.agent.clone();
        let bootstrap_sessions = self.bootstrap_sessions.clone();
        let task_list_proceed = self.task_list_context.clone();
        let fleet = self.fleet.clone();
        let workspace = cx.new(|cx| {
            WorkspaceView::new(
                session,
                window,
                cx,
                agent,
                fleet,
                bootstrap_sessions,
                restored,
                task_list_proceed,
            )
        });
        let subscription = cx.subscribe(&workspace, |this, workspace, event, cx| match event {
            WorkspaceEvent::NavigateBack => match this.workspace_return_target {
                WorkspaceReturnTarget::TaskList => {
                    this.hide_workspace(cx);
                    cx.emit(SessionsEvent::ReturnToTaskList);
                }
            },
            WorkspaceEvent::ProceedToLifecycle => {
                if let Some(ctx) = this.task_list_context.clone() {
                    this.hide_workspace(cx);
                    cx.emit(SessionsEvent::ProceedToLifecycle {
                        task_id: ctx.task_id,
                        lifecycle: ctx.lifecycle,
                    });
                }
            }
            WorkspaceEvent::SessionComplete => {
                this.reload();
                cx.notify();
            }
            WorkspaceEvent::NeedsBootstrap => {
                let session = workspace.read(cx).interview_session().clone();
                this.stash_workspace_in_flight(cx);
                this.workspace = None;
                this._workspace_subscription = None;
                this.reload();
                this.pending_bootstrap_prompt = Some(session);
                cx.notify();
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

    fn stash_workspace_in_flight(&mut self, cx: &App) {
        let Some(workspace) = self.workspace.take() else {
            return;
        };
        let (session_id, state) = {
            let view = workspace.read(cx);
            (view.interview_session().id, view.export_in_flight_state())
        };
        self._workspace_subscription = None;
        if !state.is_empty() {
            self.in_flight_by_session.insert(session_id, state);
        } else {
            self.in_flight_by_session.remove(&session_id);
        }
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
        if let Some(session) = self.pending_bootstrap_prompt.take() {
            if self.should_prompt_bootstrap(&session) {
                self.prompt_bootstrap_setup(session, window, cx);
            }
        }

        if let Some(session) = self.pending_opened_session.take() {
            self.open_workspace(session, window, cx);
        }

        if let Some(workspace) = &self.workspace {
            // Absolute fill gives Workspace a definite width/height. Without this,
            // percentage `w_full` on the three-column row stayed indefinite, the row
            // sized to content, and the response column was clipped on the right.
            return div()
                .relative()
                .size_full()
                .w_full()
                .h_full()
                .min_w_0()
                .overflow_hidden()
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .w_full()
                        .h_full()
                        .min_w_0()
                        .overflow_hidden()
                        .child(workspace.clone()),
                )
                .into_any_element();
        }

        // No workspace yet: this only happens transiently while an interview is
        // being provisioned/kicked off for a node picked from the task list, or
        // after that provisioning failed. Either way this view must never be a
        // dead end — always offer a way back to the task list.
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
