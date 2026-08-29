use crate::interview::agent::{AgentRunState, BootstrapGate, SharedAgent};
use crate::interview::config::{
    sync_scaffolding_from_disk, sync_scaffolding_from_disk_after_bootstrap,
};
use crate::interview::kickoff::researcher_bootstrap_prompt;
use crate::interview::views::session_list::SessionListDelegate;
use crate::interview::views::workspace::{WorkspaceEvent, WorkspaceInFlightState, WorkspaceView};
use crate::interview::{
    InterviewSession, InterviewSessionStatus, NewInterviewSession, SessionStore,
    TaskListProceedContext, TodPaths,
};
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav, on_app_nav_toggle};
use crate::ui::key_context::{self, INPUT};
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
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
        KeyBinding::new("escape", SessionCancelCompose, context),
        KeyBinding::new("shift-enter", SessionLaunch, context),
        KeyBinding::new("ctrl-shift-1", SessionFilterActive, context),
        KeyBinding::new("ctrl-shift-2", SessionFilterArchive, context),
        KeyBinding::new("escape", SessionCancelCompose, Some(INPUT)),
    ]);
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
struct TaskTarget {
    path: PathBuf,
    label: SharedString,
}

#[derive(Debug, Clone)]
struct ProjectTarget {
    path: PathBuf,
    label: SharedString,
    tasks: Vec<TaskTarget>,
}

#[derive(Debug, Clone)]
struct PurposeOption {
    phase: SharedString,
    label: SharedString,
}

pub struct SessionsView {
    paths: TodPaths,
    store: SessionStore,
    sessions: Vec<InterviewSession>,
    filter: SessionFilter,
    selected_id: Option<i64>,
    composing: bool,
    projects: Vec<ProjectTarget>,
    purpose_options: Vec<PurposeOption>,
    selected_project: usize,
    /// 0 = project-level interview; 1+ = task index + 1
    selected_task: usize,
    selected_purpose: usize,
    purpose_note: Entity<InputState>,
    agent: SharedAgent,
    bootstrap_gate: BootstrapGate,
    /// SQLite session ids with a bootstrap thread already running.
    bootstrap_sessions: Arc<Mutex<HashSet<i64>>>,
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
    in_flight_by_session: HashMap<i64, WorkspaceInFlightState>,
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
    ) -> Self {
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let store = SessionStore::open(&paths).expect("failed to open session store");
        let sessions = store.list_sessions().unwrap_or_default();
        let projects = discover_projects(&paths);
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
            store,
            sessions: sessions.clone(),
            filter: SessionFilter::Active,
            selected_id: initial_selected,
            composing: false,
            projects,
            purpose_options,
            selected_project: 0,
            selected_task: 1,
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

    fn visible_session_ids(&self) -> Vec<i64> {
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

    fn cycle_project(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.projects.is_empty() {
            return;
        }
        let len = self.projects.len();
        if delta < 0 {
            self.selected_project = (self.selected_project + len - 1) % len;
        } else {
            self.selected_project = (self.selected_project + 1) % len;
        }
        self.selected_task = self.selected_task.min(
            task_choices(&self.projects[self.selected_project])
                .len()
                .saturating_sub(1),
        );
        cx.notify();
    }

    fn cycle_task(&mut self, delta: i32, cx: &mut Context<Self>) {
        let choices = task_choices(self.projects.get(self.selected_project).expect("project"));
        if choices.is_empty() {
            return;
        }
        let len = choices.len();
        if delta < 0 {
            self.selected_task = (self.selected_task + len - 1) % len;
        } else {
            self.selected_task = (self.selected_task + 1) % len;
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

    fn launch_target(&self) -> (PathBuf, SharedString) {
        if let Some(project) = self.projects.get(self.selected_project) {
            let choices = task_choices(project);
            if let Some(choice) = choices.get(self.selected_task) {
                return (choice.path.clone(), choice.label.clone());
            }
        }
        (self.paths.repo_root().to_path_buf(), "repo root".into())
    }

    fn launch_interview(&mut self, cx: &mut Context<Self>) {
        let (entity_path, entity_label) = self.launch_target();
        let entity_path = entity_path
            .canonicalize()
            .unwrap_or_else(|_| entity_path.clone());
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

        match self.store.insert_session_with_metadata(
            NewInterviewSession {
                display_name: display_name.clone(),
                entity_path: crate::interview::config::path_for_storage(&entity_path),
                phase,
            },
            InterviewSessionStatus::Active,
        ) {
            Ok(session) => {
                self.kickoff_status = format!("Kickoff started: {display_name}").into();
                self.composing = false;
                self.reload();
                self.selected_id = Some(session.id);
                self.sync_session_list_items(cx);
                self.start_researcher_bootstrap(session, entity_path);
                cx.notify();
            }
            Err(err) => {
                self.kickoff_status = format!("Failed to create session: {err}").into();
                cx.notify();
            }
        }
    }

    /// Open an active interview for `entity_path` + base `phase`, or insert a new
    /// session, start researcher bootstrap, and open the workspace.
    pub fn open_or_kickoff_for_entity(
        &mut self,
        entity_path: PathBuf,
        phase: &str,
        entity_label: &str,
        phase_label: &str,
        task_list_context: Option<TaskListProceedContext>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity_path = entity_path
            .canonicalize()
            .unwrap_or_else(|_| entity_path.clone());
        let storage_key = crate::interview::config::path_for_storage(&entity_path);
        let wanted_base = crate::interview::config::base_interview_phase(phase);

        self.reload();

        // Never leave another task's interview on screen while we resolve this one.
        self.hide_workspace_if_other_entity(&entity_path, cx);

        self.workspace_return_target = WorkspaceReturnTarget::TaskList;
        self.task_list_context = task_list_context;

        let existing = self.find_best_session_for_entity(&storage_key, &entity_path, wanted_base);

        if let Some(session) = existing {
            // Discard mock-agent fixtures so task-list jump always talks to the real researcher.
            if Self::session_is_mock_scaffold(&session) {
                let _ = self
                    .store
                    .set_status(session.id, InterviewSessionStatus::Archived);
                self.reload();
                tracing::info!(
                    event = "interview",
                    action = "archive_mock_scaffold",
                    session_id = session.id,
                    "archived mock interview session; kicking off fresh"
                );
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
                // Task-list jump: auto-bootstrap unbound sessions (same as new kickoff).
                // Do not leave the user on the session list behind a setup toast.
                if Self::session_needs_bootstrap(&session) {
                    let Some(cwd) = Self::entity_cwd(&session) else {
                        self.kickoff_status =
                            "Cannot set up interview: the session has no project or task path."
                                .into();
                        cx.notify();
                        return;
                    };
                    self.start_researcher_bootstrap(session.clone(), cwd);
                }
                self.open_workspace(session, window, cx);
                cx.notify();
                return;
            }
        }

        let display_name = format!("{entity_label} — {phase_label}");
        match self.store.insert_session_with_metadata(
            NewInterviewSession {
                display_name: display_name.clone(),
                entity_path: storage_key,
                phase: phase.to_string(),
            },
            InterviewSessionStatus::Active,
        ) {
            Ok(session) => {
                self.kickoff_status = format!("Kickoff started: {display_name}").into();
                self.reload();
                self.selected_id = Some(session.id);
                self.sync_session_list_items(cx);
                self.start_researcher_bootstrap(session.clone(), entity_path);
                self.open_workspace(session, window, cx);
                cx.notify();
            }
            Err(err) => {
                self.kickoff_status = format!("Failed to create session: {err}").into();
                cx.notify();
            }
        }
    }

    fn entity_paths_equal(stored: &str, storage_key: &str, entity_path: &Path) -> bool {
        let stored_path = PathBuf::from(stored);
        stored.eq_ignore_ascii_case(storage_key)
            || crate::interview::config::paths_match(&stored_path, entity_path)
    }

    fn session_matches_entity_phase(
        session: &InterviewSession,
        storage_key: &str,
        entity_path: &Path,
        wanted_base: &str,
    ) -> bool {
        let Some(stored) = session.entity_path.as_deref() else {
            return false;
        };
        if !Self::entity_paths_equal(stored, storage_key, entity_path) {
            return false;
        }
        let session_base = session
            .phase
            .as_deref()
            .map(crate::interview::config::base_interview_phase)
            .unwrap_or("");
        wanted_base.is_empty() || session_base == wanted_base
    }

    /// Open question files still on disk for this session (0 if unbound / unreadable).
    fn session_open_question_count(session: &InterviewSession) -> usize {
        let Some(cfg_path) = session
            .config_path
            .as_ref()
            .map(Path::new)
            .filter(|p| p.exists())
        else {
            return 0;
        };
        let Ok(config) = crate::interview::config::parse_interview_config(cfg_path) else {
            return 0;
        };
        crate::interview::queue::load_queue_dir(&config.queue)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    /// Pick the interview to open for this task — never prefer an empty `complete`
    /// session over one that still has open questions (or over starting fresh).
    fn find_best_session_for_entity(
        &self,
        storage_key: &str,
        entity_path: &Path,
        wanted_base: &str,
    ) -> Option<InterviewSession> {
        let matches: Vec<&InterviewSession> = self
            .sessions
            .iter()
            .filter(|s| {
                Self::session_matches_entity_phase(s, storage_key, entity_path, wanted_base)
            })
            .collect();
        if matches.is_empty() {
            return None;
        }

        // 1) Any session (incl. archived) that still has open questions — newest wins.
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

        // 2) Active unbound — bootstrap still needed / in progress.
        if let Some(session) = matches
            .iter()
            .filter(|s| {
                s.status == InterviewSessionStatus::Active && Self::session_needs_bootstrap(s)
            })
            .max_by_key(|s| s.id)
        {
            return Some((*session).clone());
        }

        // 3) Other Active (bound but empty) — allow replenish.
        if let Some(session) = matches
            .iter()
            .filter(|s| s.status == InterviewSessionStatus::Active)
            .max_by_key(|s| s.id)
        {
            return Some((*session).clone());
        }

        // 4) Complete with empty queue — reopen finished workspace (do not re-kickoff).
        if let Some(session) = matches
            .iter()
            .filter(|s| {
                s.status == InterviewSessionStatus::Complete
                    && Self::session_open_question_count(s) == 0
            })
            .max_by_key(|s| s.id)
        {
            return Some((*session).clone());
        }

        // Archived / other terminal sessions with empty queues: start fresh kickoff.
        None
    }

    fn hide_workspace_if_other_entity(&mut self, entity_path: &Path, cx: &App) {
        let Some(workspace) = self.workspace.as_ref() else {
            return;
        };
        let other = {
            let session = workspace.read(cx).interview_session();
            let Some(stored) = session.entity_path.as_deref() else {
                return;
            };
            !crate::interview::config::paths_match(Path::new(stored), entity_path)
        };
        if other {
            self.stash_workspace_in_flight(cx);
            self.workspace = None;
            self.workspace_visible = false;
            self._workspace_subscription = None;
        }
    }

    fn session_needs_bootstrap(session: &InterviewSession) -> bool {
        !session
            .config_path
            .as_ref()
            .is_some_and(|p| Path::new(p).exists())
    }

    /// True when scaffolding was produced by `--agent mock` (fixtures), not a real researcher.
    fn session_is_mock_scaffold(session: &InterviewSession) -> bool {
        if session
            .session_id
            .as_deref()
            .is_some_and(|s| s.contains("interview-interview"))
        {
            return true;
        }
        let Some(cfg_path) = session.config_path.as_ref().map(Path::new) else {
            return false;
        };
        let Ok(config) = crate::interview::config::parse_interview_config(cfg_path) else {
            return false;
        };
        if std::fs::read_to_string(&config.transcript)
            .map(|t| t.contains("# Mock interview") || t.contains("mock-bootstrap"))
            .unwrap_or(false)
        {
            return true;
        }
        let Ok(entries) = std::fs::read_dir(&config.queue) else {
            return false;
        };
        entries.flatten().take(8).any(|entry| {
            std::fs::read_to_string(entry.path())
                .map(|c| c.contains("mock-bootstrap") || c.contains("Mock MC question"))
                .unwrap_or(false)
        })
    }

    fn bootstrap_in_flight(&self, session_id: i64) -> bool {
        self.bootstrap_sessions
            .lock()
            .expect("bootstrap sessions lock")
            .contains(&session_id)
    }

    fn should_prompt_bootstrap(&self, session: &InterviewSession) -> bool {
        Self::session_needs_bootstrap(session) && !self.bootstrap_in_flight(session.id)
    }

    fn entity_cwd(session: &InterviewSession) -> Option<PathBuf> {
        session
            .entity_path
            .as_ref()
            .filter(|p| !p.is_empty())
            .map(|p| {
                PathBuf::from(p.as_str())
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(p.as_str()))
            })
    }

    fn bootstrap_subject_label(session: &InterviewSession) -> SharedString {
        if let Some(prefix) = session.display_name.split('—').next() {
            let trimmed = prefix.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string().into();
            }
        }
        session
            .entity_path
            .as_ref()
            .and_then(|p| {
                Path::new(p.as_str())
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            })
            .map(SharedString::from)
            .unwrap_or_else(|| "This interview".into())
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
        let Some(cwd) = Self::entity_cwd(&session) else {
            tracing::warn!(
                event = "interview",
                action = "bootstrap_skipped_no_entity",
                session_id = session.id,
                "cannot bootstrap session without entity_path"
            );
            self.kickoff_status =
                "Cannot set up interview: the session has no project or task path.".into();
            cx.notify();
            return;
        };
        if session.status == InterviewSessionStatus::Complete {
            let _ = self
                .store
                .set_status(session.id, InterviewSessionStatus::Active);
        }
        tracing::info!(
            event = "interview",
            action = "bootstrap_accepted",
            session_id = session.id,
            entity = %cwd.display(),
            "user accepted bootstrap for unbound session"
        );
        self.reload();
        let session = self
            .sessions
            .iter()
            .find(|s| s.id == session.id)
            .cloned()
            .unwrap_or(session);
        self.start_researcher_bootstrap(session.clone(), cwd);
        self.open_workspace(session, window, cx);
    }

    fn start_researcher_bootstrap(&self, session: InterviewSession, cwd: PathBuf) {
        let prompt = researcher_bootstrap_prompt(&session);
        let agent = self.agent.clone();
        let bootstrap_gate = self.bootstrap_gate.clone();
        let bootstrap_sessions = self.bootstrap_sessions.clone();
        let store_paths = self.paths.clone();
        let session_id = session.id;
        {
            let mut in_flight = bootstrap_sessions.lock().expect("bootstrap sessions lock");
            if !in_flight.insert(session_id) {
                tracing::debug!(
                    event = "interview",
                    action = "bootstrap_already_running",
                    session_id,
                    "bootstrap already in flight for session"
                );
                return;
            }
        }
        tracing::info!(
            event = "interview",
            action = "bootstrap_start",
            session_id,
            cwd = %cwd.display(),
            phase = session.phase.as_deref().unwrap_or(""),
            entity = session.entity_path.as_deref().unwrap_or(""),
            prompt_chars = prompt.len(),
            "researcher bootstrap thread starting"
        );
        bootstrap_gate.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            struct BootstrapGuard {
                sessions: Arc<Mutex<HashSet<i64>>>,
                gate: BootstrapGate,
                session_id: i64,
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
            let repo_root = store_paths.repo_root().to_path_buf();
            let handle = {
                let mut provider = agent.lock().expect("agent lock");
                provider.start_researcher_replenishment(cwd, prompt)
            };
            let Ok(handle) = handle else {
                tracing::error!(
                    event = "interview",
                    action = "bootstrap_start_failed",
                    session_id,
                    "researcher bootstrap failed to start"
                );
                eprintln!("tod: researcher bootstrap failed to start for session {session_id}");
                return;
            };

            // Poll disk for interview-config while ACP runs, and keep trying after ACP
            // finishes until paths bind (or timeout). One-shot sync after ACP alone races
            // when the agent returns slightly before files are visible, or SQLITE_BUSY
            // swallows a single update attempt.
            let deadline = Instant::now() + Duration::from_secs(360);
            let mut agent_finished = false;
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
                        tracing::info!(
                            event = "interview",
                            action = "bootstrap_agent_finished",
                            session_id,
                            ?state,
                            "bootstrap ACP run left InFlight"
                        );
                        // Keep bootstrap_sessions membership until this thread exits so the
                        // workspace does not emit NeedsBootstrap before disk sync binds.
                    }
                }

                if !synced {
                    match SessionStore::open(&store_paths) {
                        Ok(store) => {
                            let sync_result = if agent_finished {
                                sync_scaffolding_from_disk_after_bootstrap(
                                    &store, &repo_root, session_id,
                                )
                            } else {
                                sync_scaffolding_from_disk(&store, &repo_root, session_id)
                            };
                            match sync_result {
                                Ok(true) => {
                                    synced = true;
                                    tracing::info!(
                                        event = "interview",
                                        action = "bootstrap_synced",
                                        session_id,
                                        agent_finished,
                                        "scaffolding paths bound in SQLite"
                                    );
                                }
                                Ok(false) => {
                                    if last_sync_log.elapsed() >= Duration::from_secs(5) {
                                        tracing::debug!(
                                            event = "interview",
                                            action = "bootstrap_sync_pending",
                                            session_id,
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
                                        session_id,
                                        error = %err,
                                        "scaffolding sync error"
                                    );
                                    eprintln!(
                                        "tod: scaffolding sync error for session {session_id}: {err}"
                                    );
                                }
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                event = "interview",
                                action = "bootstrap_store_open_failed",
                                session_id,
                                error = %err,
                                "scaffolding store open failed"
                            );
                            eprintln!(
                                "tod: scaffolding store open failed for session {session_id}: {err}"
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
                    session_id,
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
            // Do not leave a different task's workspace visible behind the toast.
            if let Some(entity) = session.entity_path.as_deref() {
                self.hide_workspace_if_other_entity(Path::new(entity), cx);
            } else if self.workspace.is_some() {
                self.stash_workspace_in_flight(cx);
                self.workspace = None;
                self.workspace_visible = false;
                self._workspace_subscription = None;
            }
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
        let workspace = cx.new(|cx| {
            WorkspaceView::new(
                session,
                window,
                cx,
                agent,
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
                &self.projects,
                &self.purpose_options,
                self.selected_project,
                self.selected_task,
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
    projects: &[ProjectTarget],
    purposes: &[PurposeOption],
    selected_project: usize,
    selected_task: usize,
    selected_purpose: usize,
    purpose_note: &Entity<InputState>,
) -> impl IntoElement {
    let project = projects.get(selected_project);
    let project_label = project
        .map(|p| p.label.clone())
        .unwrap_or_else(|| "—".into());
    let task_label = project
        .map(|p| task_choices(p).get(selected_task).map(|t| t.label.clone()))
        .flatten()
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
        .child(
            div()
                .text_sm()
                .font_semibold()
                .child("New interview"),
        )
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child("Choose a process project and optionally a task under it, then pick the interview phase."),
        )
        .child(picker_row(
            cx,
            "Project",
            project_label,
            "cycle-project-prev",
            "◀",
            |this, _, _, cx| this.cycle_project(-1, cx),
            "cycle-project-next",
            "▶",
            |this, _, _, cx| this.cycle_project(1, cx),
        ))
        .child(picker_row(
            cx,
            "Task",
            task_label,
            "cycle-task-prev",
            "◀",
            |this, _, _, cx| this.cycle_task(-1, cx),
            "cycle-task-next",
            "▶",
            |this, _, _, cx| this.cycle_task(1, cx),
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

fn task_choices(project: &ProjectTarget) -> Vec<TaskTarget> {
    let mut choices = vec![TaskTarget {
        path: project.path.clone(),
        label: format!("{} (project-level)", project.label).into(),
    }];
    choices.extend(project.tasks.clone());
    choices
}

fn discover_projects(paths: &TodPaths) -> Vec<ProjectTarget> {
    let mut options = Vec::new();
    let doc_process = paths
        .repo_root()
        .join("doc")
        .join("process")
        .join("projects");
    if doc_process.is_dir() {
        if let Ok(projects) = std::fs::read_dir(&doc_process) {
            for project in projects.flatten() {
                let project_path = project.path();
                if !project_path.is_dir() {
                    continue;
                }
                let project_name = project_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("project")
                    .to_string();
                let mut tasks = Vec::new();
                let tasks_dir = project_path.join("tasks");
                if tasks_dir.is_dir() {
                    if let Ok(task_entries) = std::fs::read_dir(tasks_dir) {
                        for task in task_entries.flatten() {
                            let task_path = task.path();
                            if task_path.is_dir() {
                                let task_name = task_path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("task")
                                    .to_string();
                                tasks.push(TaskTarget {
                                    path: task_path,
                                    label: task_name.into(),
                                });
                            }
                        }
                    }
                }
                tasks.sort_by(|a, b| a.label.cmp(&b.label));
                options.push(ProjectTarget {
                    path: project_path,
                    label: project_name.into(),
                    tasks,
                });
            }
        }
    }
    if options.is_empty() {
        options.push(ProjectTarget {
            path: paths.repo_root().to_path_buf(),
            label: "repo root".into(),
            tasks: Vec::new(),
        });
    }
    options.sort_by(|a, b| a.label.cmp(&b.label));
    options
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
