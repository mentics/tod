use crate::interview::agent::{AgentRunState, BootstrapGate, SharedAgent};
use crate::interview::config::{
    sync_scaffolding_from_disk, sync_scaffolding_from_disk_after_bootstrap,
};
use crate::interview::views::session_list::SessionListDelegate;
use crate::interview::views::workspace::{WorkspaceEvent, WorkspaceInFlightState, WorkspaceView};
use crate::interview::{
    InterviewSession, InterviewSessionStatus, NewInterviewSession, SessionStore,
    TaskListProceedContext, TodPaths, TodSettings,
};
use crate::process_bundle::{AgentLaunchContext, ProcessManifest, TodInstallPaths};
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav, on_app_nav_toggle};
use crate::ui::key_context;
use crate::ui::list::{ListArrowDown, ListArrowUp};
use crate::ui::toast::confirm_toast;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Window, actions, div, px,
};
use gpui_component::IndexPath;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::list::{List, ListEvent, ListState};
use gpui_component::{ActiveTheme, Disableable, StyledExt, h_flex, v_flex};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tod_store::fleet::{FleetStore, ensure_interview_agent_for_node};
use uuid::Uuid;

const SESSIONS_CONTEXT: &str = "InterviewSessions";

actions!(
    interview_sessions,
    [
        SessionOpen,
        SessionToggleNew,
        SessionLaunch,
        SessionCancelCompose,
        SessionFilterActive,
        SessionFilterArchive
    ]
);

pub fn register_sessions_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(SESSIONS_CONTEXT));
    // ↑/↓ use ListArrow* (same as Tasks / question list) — bound here because
    // SessionsView keeps InterviewSessions focus context for N/filter/Enter.
    cx.bind_keys([
        KeyBinding::new("up", ListArrowUp, context),
        KeyBinding::new("down", ListArrowDown, context),
        KeyBinding::new("enter", SessionOpen, context),
        KeyBinding::new("n", SessionToggleNew, context),
        KeyBinding::new("shift-enter", SessionLaunch, context),
        KeyBinding::new("ctrl-shift-1", SessionFilterActive, context),
        KeyBinding::new("ctrl-shift-2", SessionFilterArchive, context),
    ]);
    key_context::bind_panel_escape(cx, SessionCancelCompose, SESSIONS_CONTEXT);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionFilter {
    Active,
    Archive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceReturnTarget {
    TaskList,
    SessionsList,
}

#[derive(Debug, Clone)]
pub enum SessionsEvent {
    ReturnToTaskList,
    ProceedToLifecycle { task_id: String, lifecycle: String },
}

#[derive(Debug, Clone)]
struct NodeTarget {
    node_id: Uuid,
    label: SharedString,
}

#[derive(Debug, Clone)]
struct PurposeOption {
    phase: SharedString,
    label: SharedString,
}

pub struct SessionsView {
    paths: TodPaths,
    fleet: Arc<FleetStore>,
    store: SessionStore,
    sessions: Vec<InterviewSession>,
    filter: SessionFilter,
    selected_id: Option<Uuid>,
    composing: bool,
    work_nodes: Vec<NodeTarget>,
    purpose_options: Vec<PurposeOption>,
    selected_node: usize,
    selected_purpose: usize,
    purpose_note: Entity<InputState>,
    agent: SharedAgent,
    bootstrap_gate: BootstrapGate,
    /// SQLite session ids with a bootstrap thread already running.
    bootstrap_sessions: Arc<Mutex<HashSet<Uuid>>>,
    kickoff_status: SharedString,
    focus_handle: FocusHandle,
    session_list_state: Entity<ListState<SessionListDelegate>>,
    _session_list_subscription: Subscription,
    workspace: Option<Entity<WorkspaceView>>,
    /// When false, the session list is shown but an existing workspace entity is
    /// kept alive so in-flight pending questions / agent runs survive Back → Open.
    workspace_visible: bool,
    workspace_return_target: WorkspaceReturnTarget,
    /// When opened from the task list, holds ids for **Proceed** → lifecycle panel.
    task_list_context: Option<TaskListProceedContext>,
    /// Pending submit state for sessions whose workspace was replaced (e.g. user
    /// opened a different interview). Restored on reopen.
    in_flight_by_session: HashMap<Uuid, WorkspaceInFlightState>,
    _workspace_subscription: Option<Subscription>,
    /// Deferred bootstrap prompt after workspace detects missing scaffolding.
    pending_bootstrap_prompt: Option<InterviewSession>,
    app_nav: AppNavMenu,
}

impl SessionsView {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        agent: SharedAgent,
        bootstrap_gate: BootstrapGate,
        fleet: Arc<FleetStore>,
    ) -> Self {
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let store = SessionStore::open(fleet.clone());
        let sessions = store.list_sessions().unwrap_or_default();
        let work_nodes = discover_work_nodes(&fleet);
        let purpose_options = default_purposes();
        let purpose_note = cx.new(|cx| InputState::new(window, cx).placeholder("Optional note"));

        let initial_selected = sessions.first().map(|s| s.id);
        let visible = filter_sessions(&sessions, SessionFilter::Active);
        let delegate = SessionListDelegate::new(visible);
        let session_list_state =
            cx.new(|cx| ListState::new(delegate, window, cx).searchable(false));
        session_list_state.update(cx, |state, cx| {
            let ix = initial_selected
                .and_then(|id| state.delegate().index_of_id(id))
                .map(IndexPath::new);
            state.set_selected_index(ix, window, cx);
            if ix.is_some() {
                state.scroll_to_selected_item(window, cx);
            }
        });
        let _session_list_subscription =
            cx.subscribe(&session_list_state, |this, state, event, cx| {
                match event {
                    ListEvent::Select(ix) => {
                        let id = state.read(cx).delegate().items().get(ix.row).map(|s| s.id);
                        if let Some(id) = id {
                            this.selected_id = Some(id);
                            cx.notify();
                        }
                    }
                    ListEvent::Confirm(_) => {
                        // Window is not available in ListEvent; SessionOpen /
                        // Open button paths call open_selected with a Window.
                        // Mouse double-confirm on List still syncs selection above
                        // via Select before Confirm — open via SessionOpen key.
                    }
                    ListEvent::Cancel => {}
                }
            });

        Self {
            paths,
            fleet,
            store,
            sessions: sessions.clone(),
            filter: SessionFilter::Active,
            selected_id: initial_selected,
            composing: false,
            work_nodes,
            purpose_options,
            selected_node: 0,
            selected_purpose: 0,
            purpose_note,
            agent,
            bootstrap_gate,
            bootstrap_sessions: Arc::new(Mutex::new(HashSet::new())),
            kickoff_status: SharedString::default(),
            focus_handle: cx.focus_handle(),
            session_list_state,
            _session_list_subscription,
            workspace: None,
            workspace_visible: false,
            workspace_return_target: WorkspaceReturnTarget::SessionsList,
            task_list_context: None,
            in_flight_by_session: HashMap::new(),
            _workspace_subscription: None,
            pending_bootstrap_prompt: None,
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
        self.workspace_visible = false;
        self.reload();
        self.kickoff_status = SharedString::default();
        cx.notify();
    }

    fn reload(&mut self) {
        self.sessions = self.store.list_sessions().unwrap_or_default();
    }

    fn visible_sessions(&self) -> Vec<&InterviewSession> {
        self.sessions
            .iter()
            .filter(|session| match self.filter {
                SessionFilter::Active => session.status != InterviewSessionStatus::Archived,
                SessionFilter::Archive => session.status == InterviewSessionStatus::Archived,
            })
            .collect()
    }

    fn visible_session_ids(&self) -> Vec<Uuid> {
        self.visible_sessions().into_iter().map(|s| s.id).collect()
    }

    fn selected_session(&self) -> Option<&InterviewSession> {
        let visible = self.visible_sessions();
        visible
            .iter()
            .copied()
            .find(|s| Some(s.id) == self.selected_id)
            .or_else(|| visible.first().copied())
    }

    fn ensure_selection(&mut self) {
        let ids = self.visible_session_ids();
        if ids.is_empty() {
            self.selected_id = None;
            return;
        }
        if self.selected_id.is_none_or(|id| !ids.contains(&id)) {
            self.selected_id = Some(ids[0]);
        }
    }

    fn sync_session_list_items(&mut self, cx: &mut Context<Self>) {
        let visible = filter_sessions(&self.sessions, self.filter);
        let selected = self.selected_id;
        self.session_list_state.update(cx, |state, cx| {
            state.delegate_mut().set_items(visible);
            if let Some(id) = selected {
                let _ = state.delegate_mut().select_by_id(id);
            } else {
                state.delegate_mut().clear_selected_index();
            }
            cx.notify();
        });
    }

    fn sync_session_list_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        scroll: bool,
    ) {
        let visible = filter_sessions(&self.sessions, self.filter);
        let selected = self.selected_id;
        self.session_list_state.update(cx, |state, cx| {
            state.delegate_mut().set_items(visible);
            let ix = selected
                .and_then(|id| state.delegate().index_of_id(id))
                .map(IndexPath::new);
            state.set_selected_index(ix, window, cx);
            if scroll && ix.is_some() {
                state.scroll_to_selected_item(window, cx);
            }
        });
    }

    fn set_filter(&mut self, filter: SessionFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = filter;
        self.ensure_selection();
        self.sync_session_list_selection(window, cx, true);
        cx.notify();
    }

    fn move_selection(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.session_list_state.read(cx).delegate().items().len();
        if count == 0 {
            return;
        }
        let current = self
            .selected_id
            .and_then(|id| self.session_list_state.read(cx).delegate().index_of_id(id))
            .unwrap_or(0);
        let new_idx = if delta < 0 {
            current.saturating_sub((-delta) as usize)
        } else {
            (current + delta as usize).min(count - 1)
        };
        if new_idx == current {
            return;
        }
        let id = self
            .session_list_state
            .read(cx)
            .delegate()
            .items()
            .get(new_idx)
            .map(|s| s.id);
        if let Some(id) = id {
            self.selected_id = Some(id);
            self.session_list_state.update(cx, |state, cx| {
                state.set_selected_index(Some(IndexPath::new(new_idx)), window, cx);
                state.scroll_to_selected_item(window, cx);
            });
            cx.notify();
        }
    }

    fn toggle_compose(&mut self, cx: &mut Context<Self>) {
        self.composing = !self.composing;
        cx.notify();
    }

    fn cancel_compose_action(&mut self, cx: &mut Context<Self>) {
        if self.composing {
            self.composing = false;
            cx.notify();
        }
    }

    fn cycle_node(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.work_nodes.is_empty() {
            return;
        }
        let len = self.work_nodes.len();
        if delta < 0 {
            self.selected_node = (self.selected_node + len - 1) % len;
        } else {
            self.selected_node = (self.selected_node + 1) % len;
        }
        cx.notify();
    }

    fn cycle_purpose(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.purpose_options.is_empty() {
            return;
        }
        let len = self.purpose_options.len();
        if delta < 0 {
            self.selected_purpose = (self.selected_purpose + len - 1) % len;
        } else {
            self.selected_purpose = (self.selected_purpose + 1) % len;
        }
        cx.notify();
    }

    fn launch_target(&self) -> (Uuid, SharedString) {
        if let Some(node) = self.work_nodes.get(self.selected_node) {
            return (node.node_id, node.label.clone());
        }
        (Uuid::nil(), "no node".into())
    }

    fn provision_interview_agent(
        &self,
        node_id: &str,
    ) -> Result<tod_store::fleet::InterviewAgentContext, String> {
        let settings = TodSettings::load(&self.paths).map_err(|e| e.to_string())?;
        ensure_interview_agent_for_node(&self.fleet, &self.paths, &settings, node_id)
            .map_err(|e| e.to_string())
    }

    fn launch_interview(&mut self, cx: &mut Context<Self>) {
        let (node_id, entity_label) = self.launch_target();
        if node_id.is_nil() {
            self.kickoff_status = "No work node selected.".into();
            cx.notify();
            return;
        }
        let purpose = self
            .purpose_options
            .get(self.selected_purpose)
            .cloned()
            .unwrap_or_else(|| PurposeOption {
                phase: "design-interview".into(),
                label: "Design".into(),
            });
        let note = self.purpose_note.read(cx).value().to_string();
        let display_name = format!("{} — {}", entity_label, purpose.label);
        let phase = if note.trim().is_empty() {
            purpose.phase.to_string()
        } else {
            format!("{} ({})", purpose.phase, note.trim())
        };

        let node_id_str = node_id.to_string();
        self.kickoff_status = "Provisioning interview workspace…".into();
        cx.notify();
        let agent_ctx = match self.provision_interview_agent(&node_id_str) {
            Ok(ctx) => ctx,
            Err(err) => {
                self.kickoff_status = format!("Interview workspace: {err}").into();
                cx.notify();
                return;
            }
        };

        match self.store.insert_session_with_metadata(
            NewInterviewSession {
                node_id,
                agent_config_id: Some(agent_ctx.agent.id.clone()),
                display_name: display_name.clone(),
                phase,
            },
            InterviewSessionStatus::Active,
            Some(agent_ctx.agent.id),
        ) {
            Ok(session) => {
                self.kickoff_status = format!("Kickoff started: {display_name}").into();
                self.composing = false;
                self.reload();
                self.selected_id = Some(session.id);
                self.sync_session_list_items(cx);
                self.start_question_maker_bootstrap(session);
                cx.notify();
            }
            Err(err) => {
                self.kickoff_status = format!("Failed to create session: {err}").into();
                cx.notify();
            }
        }
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
                self.selected_id = Some(session.id);
                self.sync_session_list_items(cx);
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
        cx.notify();
        let agent_ctx = match self.provision_interview_agent(&node_id.to_string()) {
            Ok(ctx) => ctx,
            Err(err) => {
                self.kickoff_status = format!("Interview workspace: {err}").into();
                cx.notify();
                return;
            }
        };

        let display_name = format!("{entity_label} — {phase_label}");
        match self.store.insert_session_with_metadata(
            NewInterviewSession {
                node_id,
                agent_config_id: Some(agent_ctx.agent.id.clone()),
                display_name: display_name.clone(),
                phase: phase.to_string(),
            },
            InterviewSessionStatus::Active,
            Some(agent_ctx.agent.id),
        ) {
            Ok(session) => {
                self.kickoff_status = format!("Kickoff started: {display_name}").into();
                self.reload();
                self.selected_id = Some(session.id);
                self.sync_session_list_items(cx);
                self.start_question_maker_bootstrap(session.clone());
                self.open_workspace(session, window, cx);
                cx.notify();
            }
            Err(err) => {
                self.kickoff_status = format!("Failed to create session: {err}").into();
                cx.notify();
            }
        }
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
            self.workspace_visible = false;
            self._workspace_subscription = None;
        }
    }

    fn session_needs_bootstrap(&self, session: &InterviewSession) -> bool {
        !crate::process_bundle::session_has_scaffolding(self.paths.data_root(), session)
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
                // Stay on the session list — nothing to show in the workspace yet.
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
            crate::process_bundle::resolve_session_scratchpad(self.paths.data_root(), &session);
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
                    &question_maker_settings,
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

    fn archive_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session() {
            if session.status != InterviewSessionStatus::Archived {
                let _ = self
                    .store
                    .set_status(session.id, InterviewSessionStatus::Archived);
                self.reload();
                self.ensure_selection();
                self.sync_session_list_selection(window, cx, true);
                cx.notify();
            }
        }
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session().cloned() {
            self.workspace_return_target = WorkspaceReturnTarget::SessionsList;
            self.task_list_context = None;
            if self.should_prompt_bootstrap(&session) {
                self.prompt_bootstrap_setup(session, window, cx);
            } else {
                self.open_workspace(session, window, cx);
            }
        }
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
                self.workspace_visible = true;
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
                WorkspaceReturnTarget::SessionsList => {
                    this.hide_workspace(cx);
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
                this.workspace_visible = false;
                this._workspace_subscription = None;
                this.reload();
                this.pending_bootstrap_prompt = Some(session);
                cx.notify();
            }
        });
        self.workspace = Some(workspace.clone());
        self.workspace_visible = true;
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

impl HasAppNav for SessionsView {
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

        if self.workspace_visible {
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
                    );
            }
        }

        self.ensure_selection();
        self.sync_session_list_items(cx);

        let background = cx.theme().background;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let selected = self.selected_session().cloned();
        let archived_selected = selected
            .as_ref()
            .is_some_and(|s| s.status == InterviewSessionStatus::Archived);
        let none_selected = selected.is_none();

        let list = div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(List::new(&self.session_list_state).size_full());

        let mut root = div()
            .v_flex()
            .size_full()
            .bg(background)
            .key_context(SESSIONS_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &ListArrowUp, window, cx| {
                this.move_selection(-1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &ListArrowDown, window, cx| {
                this.move_selection(1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SessionOpen, window, cx| {
                this.open_selected(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SessionToggleNew, _, cx| {
                this.toggle_compose(cx);
            }))
            .on_action(cx.listener(|this, _: &SessionCancelCompose, _, cx| {
                this.cancel_compose_action(cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SessionFilterActive, window, cx| {
                this.set_filter(SessionFilter::Active, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SessionFilterArchive, window, cx| {
                this.set_filter(SessionFilter::Archive, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &SessionLaunch, _, cx| {
                if this.composing {
                    this.launch_interview(cx);
                }
            }))
            .on_action(cx.listener(on_app_nav_toggle::<Self>))
            .child(header_bar(self, window, cx, self.filter, self.composing))
            .child(list);

        if self.composing {
            root = root.child(compose_panel(
                cx,
                border,
                muted,
                &self.work_nodes,
                &self.purpose_options,
                self.selected_node,
                self.selected_purpose,
                &self.purpose_note,
            ));
        }

        root.child(footer_bar(
            cx,
            border,
            archived_selected,
            &self.kickoff_status,
            none_selected,
        ))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, window, _| {
                this.focus(window);
            }),
        )
    }
}

fn header_bar(
    view: &mut SessionsView,
    window: &mut Window,
    cx: &mut Context<SessionsView>,
    filter: SessionFilter,
    composing: bool,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let foreground = theme.foreground;
    // Same layout pattern as shell tab_bar (div().h_flex() + styled_tab) — the
    // gpui_component h_flex + min_w_0 + overflow_hidden combination painted an
    // empty strip with no Active/Archive/New controls.
    div()
        .id("sessions-header")
        .h_flex()
        .w_full()
        .flex_shrink_0()
        .gap_2()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.background)
        .child(view.render_app_nav(window, cx))
        .child(
            div()
                .px_2()
                .py_2()
                .text_sm()
                .font_semibold()
                .text_color(foreground)
                .child("Interview sessions"),
        )
        .child(filter_tab(
            cx,
            "Active",
            filter == SessionFilter::Active,
            |this, _, window, cx| this.set_filter(SessionFilter::Active, window, cx),
        ))
        .child(filter_tab(
            cx,
            "Archive",
            filter == SessionFilter::Archive,
            |this, _, window, cx| this.set_filter(SessionFilter::Archive, window, cx),
        ))
        .child(
            Button::new("new-interview")
                .label(if composing {
                    "Cancel new"
                } else {
                    "New interview (N)"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_compose(cx);
                })),
        )
}

fn footer_bar(
    cx: &mut Context<SessionsView>,
    border: gpui::Hsla,
    archived_selected: bool,
    kickoff_status: &SharedString,
    none_selected: bool,
) -> impl IntoElement {
    let theme = cx.theme();
    let status = if archived_selected {
        "Archived — mutations blocked".into()
    } else if !kickoff_status.is_empty() {
        kickoff_status.clone()
    } else {
        "↑↓ select · Enter open · N new interview".into()
    };

    h_flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_3()
        .border_t_1()
        .border_color(border)
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(status),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("archive-session")
                        .label("Archive")
                        .disabled(none_selected || archived_selected)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.archive_selected(window, cx);
                        })),
                )
                .child(
                    Button::new("open-session")
                        .label("Open (Enter)")
                        .disabled(none_selected)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_selected(window, cx);
                        })),
                ),
        )
}

fn compose_panel(
    cx: &mut Context<SessionsView>,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    work_nodes: &[NodeTarget],
    purposes: &[PurposeOption],
    selected_node: usize,
    selected_purpose: usize,
    purpose_note: &Entity<InputState>,
) -> impl IntoElement {
    let node_label = work_nodes
        .get(selected_node)
        .map(|n| n.label.clone())
        .unwrap_or_else(|| "—".into());
    let purpose_label = purposes
        .get(selected_purpose)
        .map(|p| p.label.clone())
        .unwrap_or_else(|| "—".into());

    v_flex()
        .gap_3()
        .p_4()
        .border_t_1()
        .border_color(border)
        .child(div().text_sm().font_semibold().child("New interview"))
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child("Choose a work node from the outline, then pick the interview phase."),
        )
        .child(picker_row(
            cx,
            "Node",
            node_label,
            "cycle-node-prev",
            "◀",
            |this, _, _, cx| this.cycle_node(-1, cx),
            "cycle-node-next",
            "▶",
            |this, _, _, cx| this.cycle_node(1, cx),
        ))
        .child(picker_row(
            cx,
            "Phase",
            purpose_label,
            "cycle-purpose-prev",
            "◀",
            |this, _, _, cx| this.cycle_purpose(-1, cx),
            "cycle-purpose-next",
            "▶",
            |this, _, _, cx| this.cycle_purpose(1, cx),
        ))
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .child(div().w(px(120.)).text_sm().child("Note"))
                .child(Input::new(purpose_note).w_full()),
        )
        .child(
            Button::new("launch-interview")
                .label("Launch (Shift+Enter)")
                .primary()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.launch_interview(cx);
                })),
        )
}

fn picker_row(
    cx: &mut Context<SessionsView>,
    label: &'static str,
    value: SharedString,
    prev_id: &'static str,
    prev_label: &'static str,
    prev_fn: impl Fn(&mut SessionsView, &gpui::ClickEvent, &mut Window, &mut Context<SessionsView>)
    + 'static,
    next_id: &'static str,
    next_label: &'static str,
    next_fn: impl Fn(&mut SessionsView, &gpui::ClickEvent, &mut Window, &mut Context<SessionsView>)
    + 'static,
) -> impl IntoElement {
    h_flex()
        .gap_3()
        .items_center()
        .child(div().w(px(120.)).text_sm().child(label))
        .child(
            h_flex()
                .flex_1()
                .gap_2()
                .items_center()
                .child(
                    Button::new(prev_id)
                        .label(prev_label)
                        .on_click(cx.listener(prev_fn)),
                )
                .child(div().flex_1().text_sm().child(value))
                .child(
                    Button::new(next_id)
                        .label(next_label)
                        .on_click(cx.listener(next_fn)),
                ),
        )
}

fn filter_sessions(sessions: &[InterviewSession], filter: SessionFilter) -> Vec<InterviewSession> {
    sessions
        .iter()
        .filter(|session| match filter {
            SessionFilter::Active => session.status != InterviewSessionStatus::Archived,
            SessionFilter::Archive => session.status == InterviewSessionStatus::Archived,
        })
        .cloned()
        .collect()
}

fn filter_tab(
    cx: &mut Context<SessionsView>,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&mut SessionsView, &gpui::ClickEvent, &mut Window, &mut Context<SessionsView>)
    + 'static,
) -> impl IntoElement {
    styled_tab(cx, label, active, on_click)
}

pub fn styled_tab<C>(
    cx: &mut Context<C>,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&mut C, &gpui::ClickEvent, &mut Window, &mut Context<C>) + 'static,
) -> impl IntoElement
where
    C: 'static,
{
    let theme = cx.theme();
    div()
        .id(label)
        .px_4()
        .py_2()
        .min_h(px(32.))
        .items_center()
        .rounded_sm()
        .cursor_pointer()
        .font_medium()
        .when(active, |el| el.bg(theme.accent.opacity(0.28)))
        .text_color(if active {
            theme.foreground
        } else {
            theme.muted_foreground
        })
        .on_click(cx.listener(on_click))
        .child(label)
}

fn discover_work_nodes(fleet: &FleetStore) -> Vec<NodeTarget> {
    let lists = fleet.list_outline_lists().unwrap_or_default();
    let Some(list) = lists.first() else {
        return Vec::new();
    };
    fleet
        .flatten_outline(list.id)
        .unwrap_or_default()
        .into_iter()
        .filter(|row| !row.capabilities.is_empty())
        .map(|row| NodeTarget {
            node_id: row.node.id,
            label: row.node.title.into(),
        })
        .collect()
}

fn default_purposes() -> Vec<PurposeOption> {
    vec![
        PurposeOption {
            phase: "project-defining".into(),
            label: "Initial / defining".into(),
        },
        PurposeOption {
            phase: "design-interview".into(),
            label: "Design phase".into(),
        },
        PurposeOption {
            phase: "planning-interview".into(),
            label: "Planning phase".into(),
        },
        PurposeOption {
            phase: "task-requirements-interview".into(),
            label: "Task requirements".into(),
        },
    ]
}
