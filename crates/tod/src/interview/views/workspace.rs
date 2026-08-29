use crate::interview::agent::{AgentRunState, AnswerProcessorPoolStats, RunId, SharedAgent};
use crate::interview::config::{
    InterviewConfig, parse_interview_config, sync_scaffolding_from_disk,
};
use crate::interview::kickoff::{
    answer_processor_prompt, researcher_action_prompt, researcher_replenish_prompt,
};
use crate::interview::queue::{QueueQuestion, load_queue_dir};
use crate::interview::queue_watcher::QueueWatcher;
use crate::interview::replenishment::{researcher_starts_needed, retry_backoff_secs};
use crate::interview::settings::AnswerProcessorSettings;
use crate::interview::transcript::{
    ActionRecord, AnswerRecord, append_action, append_answer, format_action_payload,
    format_answer_payload,
};
use crate::interview::views::deep_dive::{DeepDiveEvent, DeepDiveView};
use crate::interview::views::question_list::QuestionListDelegate;
use crate::interview::{
    InterviewSession, InterviewSessionStatus, SessionStore, TaskListProceedContext, TodPaths,
    TodSettings,
};
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav, on_app_nav_toggle};
use crate::ui::list::{ListArrowDown, ListArrowUp};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Corner, DismissEvent, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, ParentElement, Pixels, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Timer, WeakEntity,
    Window, actions, anchored, div, px,
};
use gpui_component::IndexPath;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::list::{List, ListEvent, ListItem, ListState};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::{ActiveTheme, Disableable, Selectable, StyledExt, h_flex, v_flex};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const QUEUE_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Brief red flash before a queue row disappears so removals feel intentional.
const QUESTION_REMOVAL_FLASH: Duration = Duration::from_millis(500);

struct SubmitAnswerWork {
    question_id: String,
    question_path: PathBuf,
    question_body: String,
    /// Transcript answer text (notes and/or edited proposed text).
    answer_text: String,
    mc: Option<String>,
    text_changed: Option<bool>,
    /// Answer-processor body (notes when unchanged; full edited proposed text when changed).
    payload_body: String,
    transcript: PathBuf,
    config_path: PathBuf,
    cwd: PathBuf,
    settings: AnswerProcessorSettings,
}

struct SubmitAnswerOutcome {
    question_id: String,
    result: Result<(RunId, Option<String>), String>,
}

struct SubmitActionWork {
    question_id: String,
    question_path: PathBuf,
    action: String,
    notes: String,
    question_body: String,
    transcript: PathBuf,
    config_path: PathBuf,
    cwd: PathBuf,
}

struct SubmitActionOutcome {
    question_id: String,
    action: String,
    result: Result<(RunId, Option<String>), String>,
}

actions!(
    interview_workspace,
    [
        SubmitAnswer,
        McDigit1,
        McDigit2,
        McDigit3,
        McDigit4,
        McDigit5,
        McDigit6,
        McDigit7,
        McDigit8,
        McDigit9,
        QuestionMoveUp,
        QuestionMoveDown,
        FocusRight,
        FocusLeft,
        ActivateFocused,
        WorkspaceEscape,
        NavigateBack,
        FocusNotes,
    ]
);

const WORKSPACE_CONTEXT: &str = "InterviewWorkspace";
const MAX_RESEARCHER_RETRIES: u32 = 3;
const OTHER_ACTION_ITEMS: [(&str, &str); 4] = [
    ("reconsider", "Consider / Reconsider"),
    ("defer", "Defer"),
    ("more-options", "More options"),
    ("deep-dive", "Deep dive"),
];
const LIST_COLUMN_WIDTH: f32 = 160.;
const LIST_COLUMN_MIN: f32 = 120.;
/// Middle reading pane. Initial width; response column flexes for remaining width.
const BODY_COLUMN_WIDTH: f32 = 250.;
const BODY_COLUMN_MIN: f32 = 160.;
const RESPONSE_COLUMN_MIN: f32 = 200.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceFocus {
    QuestionList,
    /// Index into response interactive controls (MC options, optional proposed text, then Notes, actions, Submit).
    Response(usize),
}

#[derive(Debug, Clone)]
enum RunKind {
    AnswerProcessor { question_id: String },
    ResearcherReplenish,
    ResearcherAction { question_id: String },
}

/// Submitted / in-flight question state preserved when a workspace is torn down
/// (e.g. switching interviews) so reopen still shows those questions as pending.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceInFlightState {
    pending: HashSet<String>,
    pending_snapshots: HashMap<String, String>,
    runs: HashMap<RunId, RunKind>,
}

impl WorkspaceInFlightState {
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.runs.is_empty()
    }

    /// Keep only ids still present in the queue; drop pending whose file content
    /// already diverged from the submit-time snapshot (req 7 re-enable).
    fn pruned_for_queue(mut self, questions: &[QueueQuestion]) -> Self {
        let mut still_pending = HashSet::new();
        for id in self.pending.iter() {
            if let Some(q) = questions.iter().find(|q| &q.id == id) {
                if let Some(snapshot) = self.pending_snapshots.get(id) {
                    if file_contents(&q.path).as_deref() != Some(snapshot.as_str()) {
                        continue;
                    }
                }
                still_pending.insert(id.clone());
            }
        }
        self.pending = still_pending;
        self.pending_snapshots
            .retain(|id, _| self.pending.contains(id));
        // Drop answer/action runs whose question is no longer pending; keep replenish.
        self.runs.retain(|_, kind| match kind {
            RunKind::ResearcherReplenish => true,
            RunKind::AnswerProcessor { question_id }
            | RunKind::ResearcherAction { question_id } => self.pending.contains(question_id),
        });
        self
    }
}

#[derive(Debug, Default)]
struct ReplenishState {
    retry_count: u32,
    next_retry_at: Option<Instant>,
    manual_required: bool,
    /// After a successful replenishment that left the queue empty, treat as exhausted
    /// even if `researcher-status` still says `idle` (agent should set `complete`).
    exhausted: bool,
    /// When `researcher-status` first flipped to idle/complete during the current
    /// replenishment batch — grace period for ACP to exit after disk work is done.
    status_idle_since: Option<Instant>,
    /// Last observed researcher-status while replenishment is in flight.
    last_researcher_status: ResearcherStatusKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ResearcherStatusKind {
    Idle,
    Working,
    Complete,
    #[default]
    Unknown,
}

const HUNG_REPLENISH_SECS: u64 = 45;

fn researcher_status_is_idle(kind: ResearcherStatusKind) -> bool {
    matches!(
        kind,
        ResearcherStatusKind::Idle | ResearcherStatusKind::Complete
    )
}

/// Start or clear the idle grace timer when status changes during replenishment.
fn update_replenish_idle_since(
    last: ResearcherStatusKind,
    current: ResearcherStatusKind,
    idle_since: Option<Instant>,
) -> Option<Instant> {
    if researcher_status_is_idle(current) {
        if idle_since.is_some() {
            idle_since
        } else if researcher_status_is_idle(last) {
            // Stale idle from before this run (seeded at replenish start).
            None
        } else {
            Some(Instant::now())
        }
    } else {
        None
    }
}

/// If SQLite says Complete but the bound queue has open questions, reopen Active
/// so replenish / answers are allowed (H8 / req 18).
fn reopen_complete_with_bound_queue(
    session: &mut InterviewSession,
    store: &SessionStore,
    queue_nonempty: bool,
) -> bool {
    if session.status == InterviewSessionStatus::Complete && queue_nonempty {
        let _ = store.set_status(session.id, InterviewSessionStatus::Active);
        session.status = InterviewSessionStatus::Active;
        true
    } else {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceEvent {
    NavigateBack,
    SessionComplete,
    /// User chose **Proceed** on the in-place Complete state (task-list origin).
    ProceedToLifecycle,
    /// Scaffolding never bound and no bootstrap is running — return to list for setup prompt.
    NeedsBootstrap,
}

pub struct WorkspaceView {
    session: InterviewSession,
    config: InterviewConfig,
    settings: TodSettings,
    store: SessionStore,
    questions: Vec<QueueQuestion>,
    pending: HashSet<String>,
    pending_snapshots: HashMap<String, String>,
    /// Queue ids flashing red before drop; values are snapshots kept in `questions` until finalize.
    removing: HashSet<String>,
    selected_question_id: Option<String>,
    selected_mc: Option<String>,
    notes_input: Entity<InputState>,
    proposed_input: Entity<InputState>,
    /// Question id whose `proposed_text` is currently loaded into `proposed_input`.
    proposed_loaded_for: Option<String>,
    /// None until session `config_path` / queue is bound — never falls back to repo-root queue.
    queue_watcher: Option<QueueWatcher>,
    agent: SharedAgent,
    /// SQLite session ids with a kickoff bootstrap thread still running.
    bootstrap_sessions: Arc<Mutex<HashSet<i64>>>,
    runs: HashMap<RunId, RunKind>,
    replenish_state: ReplenishState,
    status_line: SharedString,
    error_banner: Option<SharedString>,
    mutations_blocked: bool,
    deep_dive: Option<Entity<DeepDiveView>>,
    _deep_dive_subscription: Option<Subscription>,
    pending_notes_paste: Option<String>,
    focus_handle: FocusHandle,
    question_list_state: Entity<ListState<QuestionListDelegate>>,
    _question_list_subscription: Subscription,
    workspace_focus: WorkspaceFocus,
    notes_editing: bool,
    proposed_editing: bool,
    /// Open state for the native PopupMenu (not a deferred Popover — menu must
    /// stay in the focus/dispatch tree for SelectUp/SelectDown).
    actions_menu_open: bool,
    /// Live PopupMenu entity while open (created eagerly so keyboard can drive it).
    actions_menu: Option<Entity<PopupMenu>>,
    _actions_menu_subscription: Option<Subscription>,
    scaffolding_pending: bool,
    needs_bootstrap_handoff: bool,
    _poll_task: Task<()>,
    app_nav: AppNavMenu,
    task_list_proceed: Option<TaskListProceedContext>,
}

impl WorkspaceView {
    pub fn close_app_nav(&mut self) {
        self.app_nav.close();
    }

    pub fn interview_session(&self) -> &InterviewSession {
        &self.session
    }

    /// Snapshot in-flight submit state for restore after this view is dropped.
    pub fn export_in_flight_state(&self) -> WorkspaceInFlightState {
        WorkspaceInFlightState {
            pending: self.pending.clone(),
            pending_snapshots: self.pending_snapshots.clone(),
            runs: self.runs.clone(),
        }
    }

    pub fn set_task_list_proceed(&mut self, context: Option<TaskListProceedContext>) {
        self.task_list_proceed = context;
    }

    pub fn new(
        session: InterviewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
        agent: SharedAgent,
        bootstrap_sessions: Arc<Mutex<HashSet<i64>>>,
        restored_in_flight: Option<WorkspaceInFlightState>,
        task_list_proceed: Option<TaskListProceedContext>,
    ) -> Self {
        register_workspace_keys(cx);
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let store = SessionStore::open(&paths).expect("failed to open session store");
        let settings = TodSettings::load(&paths).unwrap_or_default();

        let bound_config_path = session
            .config_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| p.exists());

        let (config, queue_watcher, questions, scaffolding_pending) =
            if let Some(config_path) = bound_config_path {
                match parse_interview_config(&config_path) {
                    Ok(config) => {
                        let watcher = QueueWatcher::new(config.queue.clone()).ok();
                        let questions = load_queue_dir(&config.queue).unwrap_or_default();
                        (config, watcher, questions, false)
                    }
                    Err(_) => (unbound_config(&session, &paths), None, Vec::new(), true),
                }
            } else {
                (unbound_config(&session, &paths), None, Vec::new(), true)
            };

        let restored = restored_in_flight
            .unwrap_or_default()
            .pruned_for_queue(&questions);
        let pending = restored.pending;
        let pending_snapshots = restored.pending_snapshots;
        let runs = restored.runs;

        let selected_question_id = questions
            .iter()
            .find(|q| !pending.contains(&q.id))
            .or_else(|| questions.first())
            .map(|q| q.id.clone());
        let notes_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(3)
                .placeholder("Notes (Enter to edit; Ctrl+Enter to submit)")
        });
        let proposed_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(5)
                .placeholder("Proposed durable text (Enter to edit)")
        });
        let proposed_loaded_for = selected_question_id.clone().filter(|_| {
            questions
                .iter()
                .find(|q| selected_question_id.as_ref() == Some(&q.id))
                .and_then(|q| q.proposed_text.as_ref())
                .is_some()
        });
        if let Some(id) = proposed_loaded_for.as_ref() {
            if let Some(text) = questions
                .iter()
                .find(|q| &q.id == id)
                .and_then(|q| q.proposed_text.clone())
            {
                proposed_input.update(cx, |input, cx| {
                    input.set_value(text, window, cx);
                });
            }
        }
        // H8 / req 18: Complete must not stick over a non-empty bound queue.
        // Reopen Active on open so can_replenish works (same as apply_queue_update /
        // try_bind_bootstrap_scaffolding).
        let mut session = session;
        let _ = reopen_complete_with_bound_queue(&mut session, &store, !questions.is_empty());
        // Archived always blocks; Complete only blocks while truly finished (no open Qs).
        let mutations_blocked = session.status == InterviewSessionStatus::Archived
            || (session.status == InterviewSessionStatus::Complete && questions.is_empty());

        let poll_task = cx.spawn(async move |this, cx| {
            loop {
                Timer::after(QUEUE_POLL_INTERVAL).await;
                let Ok(()) = this.update(cx, |this, cx| {
                    if this.poll_runs_and_queue(cx) {
                        cx.notify();
                    }
                }) else {
                    break;
                };
            }
        });

        let bootstrap_in_progress = bootstrap_sessions
            .lock()
            .map(|s| s.contains(&session.id))
            .unwrap_or(false);

        let initial_selected = selected_question_id.clone();
        let initial_pending = pending.clone();
        let delegate = QuestionListDelegate::new(questions.clone());
        let question_list_state =
            cx.new(|cx| ListState::new(delegate, window, cx).searchable(false));
        question_list_state.update(cx, |state, cx| {
            state.delegate_mut().set_pending(initial_pending);
            let ix = initial_selected
                .as_ref()
                .and_then(|id| state.delegate().index_of_id(id))
                .map(IndexPath::new);
            state.set_selected_index(ix, window, cx);
            if ix.is_some() {
                state.scroll_to_selected_item(window, cx);
            }
        });
        let _question_list_subscription =
            cx.subscribe(&question_list_state, |this, state, event, cx| match event {
                ListEvent::Select(ix) | ListEvent::Confirm(ix) => {
                    let id = state
                        .read(cx)
                        .delegate()
                        .items()
                        .get(ix.row)
                        .map(|q| q.id.clone());
                    if let Some(id) = id {
                        this.workspace_focus = WorkspaceFocus::QuestionList;
                        this.notes_editing = false;
                        this.proposed_editing = false;
                        this.select_question_without_window(&id, cx);
                    }
                }
                ListEvent::Cancel => {}
            });

        let status_line = if scaffolding_pending {
            if bootstrap_in_progress {
                "Researcher bootstrap in progress…".into()
            } else {
                "Waiting for researcher scaffolding…".into()
            }
        } else if !pending.is_empty() {
            "Waiting for in-flight answers to finish…".into()
        } else {
            SharedString::default()
        };

        let view = Self {
            session,
            config,
            settings,
            store,
            questions,
            pending,
            pending_snapshots,
            removing: HashSet::new(),
            selected_question_id,
            selected_mc: None,
            notes_input,
            proposed_input,
            proposed_loaded_for,
            queue_watcher,
            agent,
            bootstrap_sessions,
            runs,
            replenish_state: ReplenishState::default(),
            status_line,
            error_banner: None,
            mutations_blocked,
            deep_dive: None,
            _deep_dive_subscription: None,
            pending_notes_paste: None,
            focus_handle: cx.focus_handle().tab_stop(true),
            question_list_state,
            _question_list_subscription,
            workspace_focus: WorkspaceFocus::QuestionList,
            notes_editing: false,
            proposed_editing: false,
            actions_menu_open: false,
            actions_menu: None,
            _actions_menu_subscription: None,
            scaffolding_pending,
            needs_bootstrap_handoff: false,
            _poll_task: poll_task,
            app_nav: AppNavMenu::default(),
            task_list_proceed,
        };

        // Prefer List key context so ↑/↓ resolve to ListArrow* while in the question list.
        cx.defer_in(window, |this, window, cx| {
            if this.workspace_focus == WorkspaceFocus::QuestionList {
                this.question_list_state.update(cx, |state, cx| {
                    state.focus(window, cx);
                });
            }
        });

        view
    }

    fn researcher_in_flight(&self) -> usize {
        self.runs
            .values()
            .filter(|kind| {
                matches!(
                    kind,
                    RunKind::ResearcherReplenish | RunKind::ResearcherAction { .. }
                )
            })
            .count()
    }

    fn answer_in_flight(&self) -> bool {
        self.runs
            .values()
            .any(|kind| matches!(kind, RunKind::AnswerProcessor { .. }))
    }

    fn answer_pool_stats(&self) -> AnswerProcessorPoolStats {
        let settings = &self.settings.answer_processor;
        self.agent
            .try_lock()
            .map(|provider| provider.answer_processor_pool_stats(&self.config.entity, settings))
            .unwrap_or(AnswerProcessorPoolStats {
                active: 0,
                in_pool: 0,
                max: settings.session_pool_size,
            })
    }

    fn answer_pool_footer_text(&self) -> SharedString {
        let stats = self.answer_pool_stats();
        format!(
            "{} active / {} in pool / {} max",
            stats.active, stats.in_pool, stats.max
        )
        .into()
    }

    fn session_bootstrap_in_flight(&self) -> bool {
        self.bootstrap_sessions
            .lock()
            .map(|s| s.contains(&self.session.id))
            .unwrap_or(false)
    }

    fn can_replenish(&self) -> bool {
        self.session.status == InterviewSessionStatus::Active
            && !self.scaffolding_pending
            && !self.session_bootstrap_in_flight()
            && self.queue_watcher.is_some()
            && !self.is_complete()
            && !self.replenish_state.manual_required
            && !self.replenish_state.exhausted
    }

    fn notes_focused(&self, window: &Window, cx: &App) -> bool {
        self.notes_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    }

    fn proposed_focused(&self, window: &Window, cx: &App) -> bool {
        self.proposed_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    }

    fn response_text_editing(&self) -> bool {
        self.notes_editing || self.proposed_editing
    }

    fn has_proposed_editor(&self) -> bool {
        self.selected_question()
            .and_then(|q| q.proposed_text.as_ref())
            .is_some_and(|t| !t.trim().is_empty())
    }

    fn clear_validation_banner(&mut self) {
        if self
            .error_banner
            .as_ref()
            .is_some_and(|msg| msg.as_ref() == "Enter notes and/or select an MC option")
        {
            self.error_banner = None;
        }
    }

    /// If kickoff left paths NULL, pick up researcher scaffolding when it appears on disk.
    fn try_bind_bootstrap_scaffolding(&mut self, cx: &mut Context<Self>) -> bool {
        if self
            .session
            .config_path
            .as_ref()
            .is_some_and(|p| Path::new(p).exists())
        {
            return false;
        }
        let Ok(paths) = TodPaths::discover() else {
            return false;
        };
        match sync_scaffolding_from_disk(&self.store, paths.repo_root(), self.session.id) {
            Ok(true) => {}
            Ok(false) | Err(_) => return false,
        }
        let Ok(Some(session)) = self.store.get_session(self.session.id) else {
            return false;
        };
        let Some(config_path) = session
            .config_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| p.exists())
        else {
            return false;
        };
        let Ok(config) = parse_interview_config(&config_path) else {
            return false;
        };
        let queue_watcher = QueueWatcher::new(config.queue.clone()).ok();
        let questions = load_queue_dir(&config.queue).unwrap_or_default();
        let selected_question_id = questions.first().map(|q| q.id.clone());
        self.session = session;
        self.config = config;
        self.queue_watcher = queue_watcher;
        self.questions = questions;
        self.selected_question_id = selected_question_id;
        self.selected_mc = None;
        self.scaffolding_pending = false;
        self.workspace_focus = WorkspaceFocus::QuestionList;
        self.notes_editing = false;
        self.proposed_editing = false;
        self.proposed_loaded_for = None;
        self.sync_question_list_items(cx);
        if self.session.status == InterviewSessionStatus::Complete && !self.questions.is_empty() {
            if reopen_complete_with_bound_queue(&mut self.session, &self.store, true) {
                self.mutations_blocked = false;
            }
        }
        tracing::info!(
            event = "interview",
            action = "workspace_bound",
            session_id = self.session.id,
            questions = self.questions.len(),
            queue = %self.config.queue.display(),
            "workspace bound bootstrap scaffolding"
        );
        self.status_line = "Scaffolding bound from researcher bootstrap".into();
        true
    }

    fn sync_scaffolding_status_line(&mut self) {
        if !self.scaffolding_pending {
            return;
        }
        self.status_line = if self.session_bootstrap_in_flight() {
            "Researcher bootstrap in progress…".into()
        } else {
            "Waiting for researcher scaffolding…".into()
        };
    }

    fn poll_runs_and_queue(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;

        if self.try_bind_bootstrap_scaffolding(cx) {
            changed = true;
        }

        if self.scaffolding_pending {
            self.sync_scaffolding_status_line();
            changed = true;
            // Only hand off after THIS session's bootstrap thread exits unbound.
            // Do not use the shared gate — another session clearing it would close us
            // mid-flight, and agent_finished clears the gate before disk sync binds.
            if !self.session_bootstrap_in_flight() && !self.needs_bootstrap_handoff {
                self.needs_bootstrap_handoff = true;
                cx.emit(WorkspaceEvent::NeedsBootstrap);
                changed = true;
            }
        }

        let mut finished = Vec::new();
        if let Ok(mut agent) = self.agent.try_lock() {
            for (run_id, kind) in &self.runs {
                if let Some(state) = agent.poll_run(*run_id) {
                    match state {
                        AgentRunState::InFlight => {}
                        AgentRunState::Success(_) => {
                            finished.push((*run_id, kind.clone(), Ok(())));
                        }
                        AgentRunState::Failure(message) => {
                            finished.push((*run_id, kind.clone(), Err(message)));
                        }
                    }
                }
            }
        }
        for (run_id, kind, result) in finished {
            self.runs.remove(&run_id);
            self.handle_run_finished(kind, result, cx);
            changed = true;
        }

        if self.reconcile_hung_replenishment(cx) {
            changed = true;
        }

        if let Some(watcher) = self.queue_watcher.as_mut() {
            if let Ok(Some(questions)) = watcher.poll() {
                self.apply_queue_update(questions, cx);
                changed = true;
            }
        }

        if self.selected_question().is_none() {
            self.clear_validation_banner();
        }

        let runs_before = self.runs.len();
        let status_before = self.status_line.clone();
        let error_before = self.error_banner.clone();
        self.maybe_start_replenishment(cx);
        self.sync_status_line_hygiene();
        if self.runs.len() != runs_before
            || self.status_line != status_before
            || self.error_banner != error_before
        {
            changed = true;
        }

        if self.is_complete() && self.session.status == InterviewSessionStatus::Active {
            let _ = self
                .store
                .set_status(self.session.id, InterviewSessionStatus::Complete);
            self.session.status = InterviewSessionStatus::Complete;
            self.mutations_blocked = true;
            self.error_banner = None;
            self.status_line = "Interview complete".into();
            cx.emit(WorkspaceEvent::SessionComplete);
            changed = true;
        }

        changed
    }

    /// If ACP stays InFlight but researcher-status is idle/complete for long enough
    /// after flipping idle, cancel the hung replenish runs as failure (do not mark
    /// success / exhausted).
    fn reconcile_hung_replenishment(&mut self, cx: &mut Context<Self>) -> bool {
        if self.researcher_in_flight() == 0 {
            self.replenish_state.status_idle_since = None;
            return false;
        }
        let current = researcher_status_kind(self.config.researcher_status.as_deref());
        let last = self.replenish_state.last_researcher_status;
        self.replenish_state.status_idle_since =
            update_replenish_idle_since(last, current, self.replenish_state.status_idle_since);
        self.replenish_state.last_researcher_status = current;

        let Some(idle_since) = self.replenish_state.status_idle_since else {
            return false;
        };
        if idle_since.elapsed() < Duration::from_secs(HUNG_REPLENISH_SECS) {
            return false;
        }
        let hung: Vec<_> = self
            .runs
            .iter()
            .filter(|(_, k)| matches!(k, RunKind::ResearcherReplenish))
            .map(|(id, k)| (*id, k.clone()))
            .collect();
        if hung.is_empty() {
            return false;
        }
        for (run_id, kind) in hung {
            if let Ok(mut agent) = self.agent.try_lock() {
                let _ = agent.cancel_run(run_id);
            }
            self.runs.remove(&run_id);
            self.handle_run_finished(
                kind,
                Err("Researcher replenishment timed out (cancelled hung run)".into()),
                cx,
            );
        }
        true
    }

    fn sync_status_line_hygiene(&mut self) {
        if self.researcher_in_flight() == 0
            && self
                .status_line
                .as_ref()
                .contains("replenishment in progress")
        {
            self.status_line = if self.replenish_state.exhausted || self.is_complete() {
                "Interview complete".into()
            } else {
                "Ready".into()
            };
            self.replenish_state.status_idle_since = None;
        }
    }

    fn reset_replenish_hung_tracking(&mut self) {
        self.replenish_state.status_idle_since = None;
        self.replenish_state.last_researcher_status =
            researcher_status_kind(self.config.researcher_status.as_deref());
    }

    fn handle_run_finished(
        &mut self,
        kind: RunKind,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        match (&kind, result) {
            (_, Ok(())) => {
                self.error_banner = None;
                match kind {
                    RunKind::AnswerProcessor { question_id } => {
                        self.status_line = format!("Answer processed for {question_id}").into();
                    }
                    RunKind::ResearcherReplenish => {
                        self.replenish_state.retry_count = 0;
                        self.replenish_state.next_retry_at = None;
                        self.replenish_state.status_idle_since = None;
                        if self.live_question_count() == 0 && self.removing.is_empty() {
                            // AP may wipe the queue while the researcher is still mid-refill
                            // (`status: working`). Do not treat that as interview exhausted.
                            let status =
                                researcher_status_kind(self.config.researcher_status.as_deref());
                            if matches!(status, ResearcherStatusKind::Working) {
                                self.replenish_state.exhausted = false;
                                self.status_line = "Researcher still filling the queue…".into();
                            } else {
                                self.replenish_state.exhausted = true;
                                self.status_line =
                                    "Researcher returned no further questions".into();
                            }
                        } else {
                            self.replenish_state.exhausted = false;
                            self.status_line = "Researcher replenishment succeeded".into();
                        }
                    }
                    RunKind::ResearcherAction { question_id } => {
                        self.status_line =
                            format!("Researcher action completed for {question_id}").into();
                    }
                }
            }
            (RunKind::AnswerProcessor { question_id }, Err(message)) => {
                self.error_banner = Some(message.into());
                self.status_line = "Answer processor failed".into();
                self.pending.remove(question_id);
                self.pending_snapshots.remove(question_id);
            }
            (RunKind::ResearcherReplenish, Err(message)) => {
                self.error_banner = Some(message.clone().into());
                self.status_line = "Researcher replenishment failed".into();
                self.replenish_state.status_idle_since = None;
                self.replenish_state.retry_count += 1;
                if self.replenish_state.retry_count >= MAX_RESEARCHER_RETRIES {
                    self.replenish_state.manual_required = true;
                    self.status_line = "Researcher failed — use Kickoff researcher to retry".into();
                } else {
                    let delay = retry_backoff_secs(self.replenish_state.retry_count - 1);
                    self.replenish_state.next_retry_at =
                        Some(Instant::now() + std::time::Duration::from_secs(delay));
                    self.status_line = format!("Researcher retry in {delay}s…").into();
                }
            }
            (RunKind::ResearcherAction { question_id }, Err(message)) => {
                self.error_banner = Some(message.into());
                self.status_line = "Researcher action failed".into();
                self.pending.remove(question_id);
                self.pending_snapshots.remove(question_id);
            }
        }
        let _ = cx;
    }

    fn maybe_start_replenishment(&mut self, cx: &mut Context<Self>) {
        if !self.can_replenish() {
            return;
        }
        if let Some(retry_at) = self.replenish_state.next_retry_at {
            if Instant::now() < retry_at {
                return;
            }
            self.replenish_state.next_retry_at = None;
        }
        let open_count = self.live_question_count();
        let in_flight = self.researcher_in_flight();
        let needed = researcher_starts_needed(open_count, in_flight, &self.settings.researcher);
        for _ in 0..needed {
            self.start_researcher_replenishment(cx);
        }
    }

    fn start_researcher_replenishment(&mut self, cx: &mut Context<Self>) {
        if self.researcher_in_flight() >= 2 {
            return;
        }
        let queue_target = self
            .config
            .queue_target
            .unwrap_or(self.settings.researcher.replenish_threshold);
        let prompt = researcher_replenish_prompt(&self.config.config_path, queue_target);
        let cwd = self.config.entity.clone();
        let start_result = match self.agent.try_lock() {
            Ok(mut agent) => agent.start_researcher_replenishment(cwd, prompt),
            Err(_) => {
                self.status_line = "Waiting for agent (bootstrap in progress)…".into();
                cx.notify();
                return;
            }
        };
        match start_result {
            Ok(handle) => {
                let first_replenish = !self
                    .runs
                    .values()
                    .any(|kind| matches!(kind, RunKind::ResearcherReplenish));
                if first_replenish {
                    self.reset_replenish_hung_tracking();
                }
                self.runs.insert(handle.id, RunKind::ResearcherReplenish);
                if self.status_line.is_empty() || self.replenish_state.retry_count == 0 {
                    self.status_line = "Researcher replenishment in progress…".into();
                }
                self.error_banner = None;
            }
            Err(err) => {
                self.error_banner = Some(format!("Failed to start researcher: {err}").into());
            }
        }
        cx.notify();
    }

    fn manual_researcher_kickoff(&mut self, cx: &mut Context<Self>) {
        self.replenish_state.manual_required = false;
        self.replenish_state.retry_count = 0;
        self.replenish_state.next_retry_at = None;
        self.replenish_state.exhausted = false;
        self.start_researcher_replenishment(cx);
    }

    fn apply_queue_update(&mut self, questions: Vec<QueueQuestion>, cx: &mut Context<Self>) {
        let live_before = self.live_question_count();
        let new_ids: HashSet<String> = questions.iter().map(|q| q.id.clone()).collect();
        let disappearing: HashSet<String> = self
            .questions
            .iter()
            .filter(|q| !new_ids.contains(&q.id))
            .map(|q| q.id.clone())
            .collect();

        // Resurrect any flash-pending rows that reappeared on disk.
        self.removing.retain(|id| !new_ids.contains(id));

        let mut still_pending = HashSet::new();
        for id in self.pending.iter() {
            if let Some(q) = questions.iter().find(|q| &q.id == id) {
                if let Some(snapshot) = self.pending_snapshots.get(id) {
                    if file_contents(&q.path).as_deref() != Some(snapshot.as_str()) {
                        continue;
                    }
                }
                still_pending.insert(id.clone());
            }
            // Keep pending while the row is flashing after disk delete.
            if self.removing.contains(id) || disappearing.contains(id) {
                still_pending.insert(id.clone());
            }
        }
        self.pending = still_pending;
        self.pending_snapshots
            .retain(|id, _| self.pending.contains(id));

        let previous = std::mem::take(&mut self.questions);
        let mut merged = Vec::with_capacity(previous.len().max(questions.len()));
        let mut used_new = HashSet::new();
        let mut newly_removing = Vec::new();

        for old in previous {
            if let Some(fresh) = questions.iter().find(|q| q.id == old.id) {
                used_new.insert(old.id.clone());
                merged.push(fresh.clone());
            } else if self.removing.contains(&old.id) {
                // Still flashing from an earlier update.
                merged.push(old);
            } else {
                // Just disappeared from disk — flash, then drop.
                newly_removing.push(old.id.clone());
                self.removing.insert(old.id.clone());
                merged.push(old);
            }
        }
        for q in &questions {
            if !used_new.contains(&q.id) {
                merged.push(q.clone());
            }
        }
        self.questions = merged;

        let live_after = self.live_question_count();
        if live_after > 0 {
            self.replenish_state.exhausted = false;
            if reopen_complete_with_bound_queue(&mut self.session, &self.store, true) {
                self.mutations_blocked = false;
            }
        } else if live_before > 0 && live_after == 0 {
            // Queue drained under us (typically AP invalidated remaining questions).
            // Clear exhausted/manual so maybe_start_replenishment can refill.
            self.replenish_state.exhausted = false;
            self.replenish_state.manual_required = false;
            self.replenish_state.retry_count = 0;
            self.replenish_state.next_retry_at = None;
            self.status_line = "Queue cleared — requesting researcher refill…".into();
        }

        // Selected row departing: lock interaction immediately; keep selection for the flash.
        if self
            .selected_question_id
            .as_ref()
            .is_some_and(|id| newly_removing.iter().any(|rid| rid == id))
        {
            self.notes_editing = false;
            self.proposed_editing = false;
            self.actions_menu_open = false;
            self.actions_menu = None;
            self._actions_menu_subscription = None;
        }

        // Keep selection on a departing row so the user sees the red flash + disabled detail.
        if self
            .selected_question_id
            .as_ref()
            .is_none_or(|id| !self.questions.iter().any(|q| &q.id == id))
        {
            self.selected_question_id = self
                .questions
                .iter()
                .find(|q| !self.pending.contains(&q.id) && !self.removing.contains(&q.id))
                .map(|q| q.id.clone());
            self.reset_response_fields(None, cx);
            if self.selected_question_id.is_none() {
                self.clear_validation_banner();
            }
        }

        self.sync_question_list_items(cx);

        for id in newly_removing {
            self.schedule_question_removal_finalize(id, cx);
        }
    }

    fn live_question_count(&self) -> usize {
        self.questions
            .iter()
            .filter(|q| !self.removing.contains(&q.id))
            .count()
    }

    fn schedule_question_removal_finalize(&mut self, question_id: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            Timer::after(QUESTION_REMOVAL_FLASH).await;
            let _ = this.update(cx, |this, cx| {
                this.finalize_question_removal(&question_id, cx);
            });
        })
        .detach();
    }

    fn finalize_question_removal(&mut self, question_id: &str, cx: &mut Context<Self>) {
        if !self.removing.remove(question_id) {
            return;
        }
        self.pending.remove(question_id);
        self.pending_snapshots.remove(question_id);
        self.questions.retain(|q| q.id != question_id);

        if self
            .selected_question_id
            .as_ref()
            .is_some_and(|id| id == question_id)
        {
            self.selected_question_id = self
                .questions
                .iter()
                .find(|q| !self.pending.contains(&q.id) && !self.removing.contains(&q.id))
                .map(|q| q.id.clone());
            self.reset_response_fields(None, cx);
            if self.selected_question_id.is_none() {
                self.clear_validation_banner();
            }
        }

        if self.live_question_count() == 0 && self.removing.is_empty() {
            self.replenish_state.exhausted = false;
            self.replenish_state.manual_required = false;
            self.replenish_state.retry_count = 0;
            self.replenish_state.next_retry_at = None;
            if self.status_line.is_empty() {
                self.status_line = "Queue cleared — requesting researcher refill…".into();
            }
        }

        self.sync_question_list_items(cx);
        self.maybe_start_replenishment(cx);
        cx.notify();
    }

    fn sync_question_list_items(&mut self, cx: &mut Context<Self>) {
        let questions = self.questions.clone();
        let pending = self.pending.clone();
        let removing = self.removing.clone();
        let selected_id = self.selected_question_id.clone();
        self.question_list_state.update(cx, |state, cx| {
            state.delegate_mut().set_items(questions);
            state.delegate_mut().set_pending(pending);
            state.delegate_mut().set_removing(removing);
            if let Some(id) = selected_id.as_deref() {
                let _ = state.delegate_mut().select_by_id(id);
            } else {
                state.delegate_mut().clear_selected_index();
            }
            cx.notify();
        });
    }

    /// Keep ListState selection aligned with workspace selection (needs a Window).
    fn sync_question_list_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        scroll: bool,
    ) {
        let questions = self.questions.clone();
        let pending = self.pending.clone();
        let removing = self.removing.clone();
        let selected_id = self.selected_question_id.clone();
        self.question_list_state.update(cx, |state, cx| {
            state.delegate_mut().set_items(questions);
            state.delegate_mut().set_pending(pending);
            state.delegate_mut().set_removing(removing);
            let ix = selected_id
                .as_deref()
                .and_then(|id| state.delegate().index_of_id(id))
                .map(IndexPath::new);
            state.set_selected_index(ix, window, cx);
            if scroll && ix.is_some() {
                state.scroll_to_selected_item(window, cx);
            }
        });
    }

    fn is_complete(&self) -> bool {
        // Bound open questions always win over SQLite `complete` (H8 / req 18).
        if !self.questions.is_empty() || self.answer_in_flight() || self.researcher_in_flight() > 0
        {
            return false;
        }
        if self.scaffolding_pending {
            return false;
        }
        if self.replenish_state.exhausted {
            return true;
        }
        if self.session.status == InterviewSessionStatus::Complete {
            return true;
        }
        matches!(
            researcher_status_kind(self.config.researcher_status.as_deref()),
            ResearcherStatusKind::Complete
        )
    }

    fn selected_question(&self) -> Option<&QueueQuestion> {
        self.selected_question_id
            .as_ref()
            .and_then(|id| self.questions.iter().find(|q| &q.id == id))
    }

    fn select_question(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .selected_question_id
            .as_ref()
            .is_some_and(|current| current == id)
        {
            return;
        }
        self.selected_question_id = Some(id.to_string());
        self.reset_response_fields(Some(window), cx);
        self.clear_validation_banner();
        self.sync_question_list_selection(window, cx, true);
        cx.notify();
    }

    fn select_question_without_window(&mut self, id: &str, cx: &mut Context<Self>) {
        if self
            .selected_question_id
            .as_ref()
            .is_some_and(|current| current == id)
        {
            return;
        }
        self.selected_question_id = Some(id.to_string());
        self.reset_response_fields(None, cx);
        self.clear_validation_banner();
        self.sync_question_list_items(cx);
        cx.notify();
    }

    fn move_question_list_by(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.questions.is_empty() {
            return;
        }
        let current = self
            .question_list_state
            .read(cx)
            .selected_index()
            .map(|ix| ix.row)
            .or_else(|| {
                self.selected_question_id
                    .as_ref()
                    .and_then(|id| self.questions.iter().position(|q| &q.id == id))
            })
            .unwrap_or(0);
        let new_idx = if delta < 0 {
            current.saturating_sub((-delta) as usize)
        } else {
            (current + delta as usize).min(self.questions.len() - 1)
        };
        if new_idx == current {
            return;
        }
        let id = self.questions[new_idx].id.clone();
        self.select_question(&id, window, cx);
    }

    fn reset_response_fields(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.selected_mc = None;
        self.actions_menu_open = false;
        self.actions_menu = None;
        self._actions_menu_subscription = None;
        let should_unfocus = window.as_ref().is_some_and(|window| {
            self.response_text_editing()
                || self.notes_focused(window, cx)
                || self.proposed_focused(window, cx)
        });
        self.notes_editing = false;
        self.proposed_editing = false;
        self.proposed_loaded_for = None;
        if let Some(window) = window {
            self.notes_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
            self.sync_proposed_input(window, cx);
            if should_unfocus {
                self.focus_handle.focus(window);
            }
        }
        if matches!(self.workspace_focus, WorkspaceFocus::Response(_)) {
            self.workspace_focus = WorkspaceFocus::Response(0);
        }
    }

    /// Load `proposed_text` for the selected question into the editor (or clear it).
    fn sync_proposed_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.selected_question_id.clone();
        if self.proposed_loaded_for == id {
            return;
        }
        let text = self
            .selected_question()
            .and_then(|q| q.proposed_text.clone())
            .unwrap_or_default();
        self.proposed_input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
        });
        self.proposed_loaded_for = id;
    }

    fn is_question_pending(&self, id: &str) -> bool {
        self.pending.contains(id)
    }

    fn is_question_removing(&self, id: &str) -> bool {
        self.removing.contains(id)
    }

    /// Pending or departing — response controls must not accept input.
    fn is_question_locked(&self, id: &str) -> bool {
        self.is_question_pending(id) || self.is_question_removing(id)
    }

    fn can_mutate(&self) -> bool {
        !self.mutations_blocked
    }

    fn can_edit_notes(&self) -> bool {
        !self.mutations_blocked
    }

    fn submit_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_mutate() {
            return;
        }
        let Some(question) = self.selected_question().cloned() else {
            self.clear_validation_banner();
            cx.notify();
            return;
        };
        if self.is_question_locked(&question.id) {
            return;
        }
        let notes = self.notes_input.read(cx).value().to_string();
        let proposed_edited = self.proposed_input.read(cx).value().to_string();
        let mc = self.selected_mc.clone();
        if notes.trim().is_empty() && mc.is_none() {
            self.error_banner = Some("Enter notes and/or select an MC option".into());
            cx.notify();
            return;
        }

        let (text_changed, payload_body, answer_text) = build_proposed_answer_parts(
            question.proposed_text.as_deref(),
            &proposed_edited,
            &notes,
        );

        let work = SubmitAnswerWork {
            question_id: question.id.clone(),
            question_path: question.path.clone(),
            question_body: question.display_body(),
            answer_text,
            mc,
            text_changed,
            payload_body,
            transcript: self.config.transcript.clone(),
            config_path: self.config.config_path.clone(),
            cwd: self.config.entity.clone(),
            settings: self.settings.answer_processor.clone(),
        };
        let agent = self.agent.clone();

        self.error_banner = None;
        self.status_line = format!("Processing answer for {}", work.question_id).into();
        self.pending.insert(work.question_id.clone());
        self.select_next_question(Some(window), cx);
        cx.notify();

        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let outcome = run_submit_answer_work(work, agent);
            let _ = tx.send_blocking(outcome);
        });
        cx.spawn(async move |this, cx| {
            if let Ok(outcome) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.finish_submit_answer(outcome, cx);
                });
            }
        })
        .detach();
    }

    fn finish_submit_answer(&mut self, outcome: SubmitAnswerOutcome, cx: &mut Context<Self>) {
        match outcome.result {
            Ok((run_id, snapshot)) => {
                self.runs.insert(
                    run_id,
                    RunKind::AnswerProcessor {
                        question_id: outcome.question_id.clone(),
                    },
                );
                if let Some(contents) = snapshot {
                    self.pending_snapshots
                        .insert(outcome.question_id.clone(), contents);
                }
            }
            Err(message) => {
                self.pending.remove(&outcome.question_id);
                self.pending_snapshots.remove(&outcome.question_id);
                self.error_banner = Some(message.into());
                self.status_line = "Answer submit failed".into();
            }
        }
        cx.notify();
    }

    fn submit_action(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        if action == "deep-dive" {
            if !self.mutations_blocked {
                self.open_deep_dive(window, cx);
            }
            return;
        }
        if !self.can_mutate() {
            return;
        }
        let Some(question) = self.selected_question().cloned() else {
            return;
        };
        if self.is_question_locked(&question.id) {
            return;
        }
        let notes = self.notes_input.read(cx).value().to_string();
        let work = SubmitActionWork {
            question_id: question.id.clone(),
            question_path: question.path.clone(),
            action: action.to_string(),
            notes: notes.trim().to_string(),
            question_body: question.display_body(),
            transcript: self.config.transcript.clone(),
            config_path: self.config.config_path.clone(),
            cwd: self.config.entity.clone(),
        };
        let agent = self.agent.clone();

        self.error_banner = None;
        self.status_line = format!("Researcher action {action} for {}", work.question_id).into();
        self.pending.insert(work.question_id.clone());
        self.select_next_question(Some(window), cx);
        cx.notify();

        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let outcome = run_submit_action_work(work, agent);
            let _ = tx.send_blocking(outcome);
        });
        cx.spawn(async move |this, cx| {
            if let Ok(outcome) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.finish_submit_action(outcome, cx);
                });
            }
        })
        .detach();
    }

    fn finish_submit_action(&mut self, outcome: SubmitActionOutcome, cx: &mut Context<Self>) {
        match outcome.result {
            Ok((run_id, snapshot)) => {
                self.runs.insert(
                    run_id,
                    RunKind::ResearcherAction {
                        question_id: outcome.question_id.clone(),
                    },
                );
                if let Some(contents) = snapshot {
                    self.pending_snapshots
                        .insert(outcome.question_id.clone(), contents);
                }
            }
            Err(message) => {
                self.pending.remove(&outcome.question_id);
                self.pending_snapshots.remove(&outcome.question_id);
                self.error_banner = Some(message.into());
                self.status_line = "Researcher action submit failed".into();
            }
        }
        cx.notify();
    }

    fn select_next_question(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let current_idx = self
            .selected_question_id
            .as_ref()
            .and_then(|id| self.questions.iter().position(|q| &q.id == id));
        let start = current_idx.map(|i| i + 1).unwrap_or(0);
        for offset in 0..self.questions.len() {
            let idx = (start + offset) % self.questions.len();
            let q = &self.questions[idx];
            if !self.pending.contains(&q.id) && !self.removing.contains(&q.id) {
                self.selected_question_id = Some(q.id.clone());
                self.reset_response_fields(window, cx);
                self.clear_validation_banner();
                self.sync_question_list_items(cx);
                return;
            }
        }
        // No non-pending question left — clear selection so we don't keep a stale banner.
        self.selected_question_id = None;
        self.reset_response_fields(window, cx);
        self.clear_validation_banner();
        self.sync_question_list_items(cx);
    }

    fn on_digit_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Text edit mode suppresses digit MC submit (req 21.6 / 22.7a) — not mere focus.
        if self.response_text_editing() {
            cx.propagate();
            return;
        }
        let _ = window;
        self.submit_mc_option(key, window, cx);
    }

    fn submit_mc_option(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_mutate() {
            return;
        }
        let Some(q) = self.selected_question() else {
            return;
        };
        if !q.options.iter().any(|o| o.key == key) {
            return;
        }
        self.selected_mc = Some(key.to_string());
        self.clear_validation_banner();
        self.submit_answer(window, cx);
    }

    fn response_stop_count(&self) -> usize {
        let mc = self
            .selected_question()
            .map(|q| q.options.len())
            .unwrap_or(0);
        let proposed = if self.has_proposed_editor() { 1 } else { 0 };
        mc + proposed + 3 // Notes, Other action, Submit
    }

    fn proposed_stop_index(&self) -> Option<usize> {
        if !self.has_proposed_editor() {
            return None;
        }
        Some(
            self.selected_question()
                .map(|q| q.options.len())
                .unwrap_or(0),
        )
    }

    fn notes_stop_index(&self) -> usize {
        let mc = self
            .selected_question()
            .map(|q| q.options.len())
            .unwrap_or(0);
        mc + if self.has_proposed_editor() { 1 } else { 0 }
    }

    fn actions_stop_index(&self) -> usize {
        self.notes_stop_index() + 1
    }

    fn submit_stop_index(&self) -> usize {
        self.actions_stop_index() + 1
    }

    fn actions_disabled(&self) -> bool {
        !self.can_mutate()
            || self
                .selected_question_id
                .as_deref()
                .is_some_and(|id| self.is_question_locked(id))
            || self.selected_question().is_none()
    }

    fn set_actions_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.actions_menu_open = open;
        if !open {
            self.actions_menu = None;
            self._actions_menu_subscription = None;
        }
        cx.notify();
    }

    fn ensure_actions_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.actions_menu.is_some() {
            return;
        }
        let view = cx.weak_entity();
        let workspace_focus = self.focus_handle.clone();
        let menu = PopupMenu::build(window, cx, move |menu, _window, _cx| {
            populate_action_menu(menu.action_context(workspace_focus), view)
        });
        self._actions_menu_subscription =
            Some(cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
                this.set_actions_menu_open(false, cx);
            }));
        self.actions_menu = Some(menu);
    }

    /// While the menu is open and focused, let native PopupMenu SelectUp/SelectDown/
    /// Confirm/Cancel run — do not stop_propagation.
    fn actions_menu_focused(&self, window: &Window, cx: &App) -> bool {
        self.actions_menu
            .as_ref()
            .is_some_and(|menu| menu.read(cx).focus_handle(cx).is_focused(window))
    }

    fn focus_actions_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.actions_menu.clone() {
            menu.update(cx, |menu, cx| {
                menu.focus_handle(cx).focus(window);
            });
        }
    }

    fn open_actions_menu_from_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.actions_disabled() || self.actions_menu_open {
            return;
        }
        self.ensure_actions_menu(window, cx);
        self.actions_menu_open = true;
        cx.notify();
        // Focus after the non-deferred menu is in the tree so PopupMenu key context wins.
        cx.on_next_frame(window, |this, window, cx| {
            this.focus_actions_menu(window, cx);
            cx.notify();
        });
    }

    fn toggle_actions_menu_from_pointer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.actions_disabled() {
            return;
        }
        if self.actions_menu_open {
            self.set_actions_menu_open(false, cx);
            self.focus_handle.focus(window);
            return;
        }
        self.ensure_actions_menu(window, cx);
        self.actions_menu_open = true;
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.focus_actions_menu(window, cx);
            cx.notify();
        });
    }

    fn close_actions_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.actions_menu_open {
            self.set_actions_menu_open(false, cx);
            self.focus_handle.focus(window);
        }
    }

    fn enter_notes_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit_notes() {
            return;
        }
        if self
            .selected_question_id
            .as_ref()
            .is_some_and(|id| self.is_question_locked(id))
        {
            return;
        }
        self.proposed_editing = false;
        self.workspace_focus = WorkspaceFocus::Response(self.notes_stop_index());
        self.notes_editing = true;
        cx.notify();
        // Focus after Input re-renders enabled (disabled when !notes_editing).
        cx.on_next_frame(window, |this, window, cx| {
            this.notes_input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    fn exit_notes_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.notes_editing {
            return;
        }
        self.notes_editing = false;
        self.workspace_focus = WorkspaceFocus::Response(self.notes_stop_index());
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn enter_proposed_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit_notes() || !self.has_proposed_editor() {
            return;
        }
        if self
            .selected_question_id
            .as_ref()
            .is_some_and(|id| self.is_question_locked(id))
        {
            return;
        }
        let Some(idx) = self.proposed_stop_index() else {
            return;
        };
        self.notes_editing = false;
        self.workspace_focus = WorkspaceFocus::Response(idx);
        self.proposed_editing = true;
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.proposed_input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    fn exit_proposed_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.proposed_editing {
            return;
        }
        self.proposed_editing = false;
        if let Some(idx) = self.proposed_stop_index() {
            self.workspace_focus = WorkspaceFocus::Response(idx);
        }
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn focus_response_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.response_text_editing() {
            return;
        }
        self.workspace_focus = WorkspaceFocus::Response(0);
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn focus_list_left(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.response_text_editing() {
            return;
        }
        self.workspace_focus = WorkspaceFocus::QuestionList;
        self.question_list_state.update(cx, |state, cx| {
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn move_response_focus(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.response_text_editing() || self.actions_menu_open {
            return;
        }
        let count = self.response_stop_count().max(1);
        let current = match self.workspace_focus {
            WorkspaceFocus::Response(i) => i,
            WorkspaceFocus::QuestionList => 0,
        };
        let new_idx = if delta < 0 {
            current.saturating_sub((-delta) as usize)
        } else {
            (current + delta as usize).min(count - 1)
        };
        self.workspace_focus = WorkspaceFocus::Response(new_idx);
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn activate_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.response_text_editing() {
            return;
        }
        let WorkspaceFocus::Response(idx) = self.workspace_focus else {
            return;
        };
        let mc_count = self
            .selected_question()
            .map(|q| q.options.len())
            .unwrap_or(0);
        if idx < mc_count {
            if let Some(key) = self
                .selected_question()
                .and_then(|q| q.options.get(idx).map(|o| o.key.clone()))
            {
                self.submit_mc_option(&key, window, cx);
            }
            return;
        }
        if self.proposed_stop_index() == Some(idx) {
            self.enter_proposed_edit(window, cx);
            return;
        }
        let notes_idx = self.notes_stop_index();
        let actions_idx = self.actions_stop_index();
        let submit_idx = self.submit_stop_index();
        if idx == notes_idx {
            self.enter_notes_edit(window, cx);
        } else if idx == submit_idx {
            self.submit_answer(window, cx);
        } else if idx == actions_idx && !self.actions_disabled() {
            self.open_actions_menu_from_keyboard(window, cx);
        }
    }

    fn focus_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.enter_notes_edit(window, cx);
    }

    fn handle_workspace_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.proposed_editing {
            self.exit_proposed_edit(window, cx);
            return;
        }
        if self.notes_editing {
            self.exit_notes_edit(window, cx);
            return;
        }
        if self.actions_menu_open {
            self.close_actions_menu(window, cx);
            return;
        }
        self.navigate_back(cx);
    }

    fn navigate_back(&mut self, cx: &mut Context<Self>) {
        cx.emit(WorkspaceEvent::NavigateBack);
    }

    fn open_deep_dive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(question) = self.selected_question().cloned() else {
            return;
        };
        if self.is_question_locked(&question.id) {
            return;
        }
        let agent = self.agent.clone();
        let config = self.config.clone();
        let session = self.session.clone();
        let deep_dive =
            cx.new(|cx| DeepDiveView::new(question, config, session, window, cx, agent));
        let subscription = cx.subscribe(&deep_dive, |this, _, event, cx| match event {
            DeepDiveEvent::Back => {
                this.deep_dive = None;
                this._deep_dive_subscription = None;
                cx.notify();
            }
            DeepDiveEvent::UseThis(text) => {
                this.pending_notes_paste = Some(text.clone());
                cx.notify();
            }
        });
        self.deep_dive = Some(deep_dive);
        self._deep_dive_subscription = Some(subscription);
        cx.notify();
    }

    fn render_workspace_header(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        border: gpui::Hsla,
        muted: gpui::Hsla,
    ) -> impl IntoElement {
        let entity_label: SharedString = self
            .session
            .entity_path
            .clone()
            .unwrap_or_else(|| "—".to_string())
            .into();
        h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .overflow_hidden()
            .border_b_1()
            .border_color(border)
            .child(self.render_app_nav(window, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_3()
                    .overflow_hidden()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .flex_shrink_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(self.session.display_name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(muted)
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_right()
                            .child(entity_label),
                    ),
            )
    }
}

fn register_workspace_keys(cx: &mut App) {
    let context = Some(WORKSPACE_CONTEXT);
    let input = Some("Input");
    cx.bind_keys([
        KeyBinding::new("ctrl-enter", SubmitAnswer, context),
        KeyBinding::new("ctrl-enter", SubmitAnswer, input),
        KeyBinding::new("escape", WorkspaceEscape, input),
        KeyBinding::new("ctrl-shift-n", FocusNotes, context),
        KeyBinding::new("1", McDigit1, context),
        KeyBinding::new("1", McDigit1, input),
        KeyBinding::new("2", McDigit2, context),
        KeyBinding::new("2", McDigit2, input),
        KeyBinding::new("3", McDigit3, context),
        KeyBinding::new("3", McDigit3, input),
        KeyBinding::new("4", McDigit4, context),
        KeyBinding::new("4", McDigit4, input),
        KeyBinding::new("5", McDigit5, context),
        KeyBinding::new("5", McDigit5, input),
        KeyBinding::new("6", McDigit6, context),
        KeyBinding::new("6", McDigit6, input),
        KeyBinding::new("7", McDigit7, context),
        KeyBinding::new("7", McDigit7, input),
        KeyBinding::new("8", McDigit8, context),
        KeyBinding::new("8", McDigit8, input),
        KeyBinding::new("9", McDigit9, context),
        KeyBinding::new("9", McDigit9, input),
        KeyBinding::new("up", QuestionMoveUp, context),
        KeyBinding::new("down", QuestionMoveDown, context),
        KeyBinding::new("right", FocusRight, context),
        KeyBinding::new("left", FocusLeft, context),
        KeyBinding::new("enter", ActivateFocused, context),
        KeyBinding::new("space", ActivateFocused, context),
        KeyBinding::new("escape", WorkspaceEscape, context),
        KeyBinding::new("alt-left", NavigateBack, context),
    ]);
}

fn unbound_config(session: &InterviewSession, paths: &TodPaths) -> InterviewConfig {
    InterviewConfig {
        session_id: session.session_id.clone().unwrap_or_default(),
        entity: session
            .entity_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| paths.repo_root().to_path_buf()),
        phase: session.phase.clone().unwrap_or_default(),
        transcript: session
            .transcript_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_default(),
        scratchpad: session
            .scratchpad_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_default(),
        // Sentinel — never watch repo-root queue (F5).
        queue: PathBuf::from("__unbound_queue__"),
        config_path: PathBuf::from("__unbound_config__"),
        queue_target: None,
        to_process: None,
        researcher_status: None,
        answer_processor_status: None,
        scope: Vec::new(),
        state_agent: None,
    }
}

fn file_contents(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn run_submit_answer_work(work: SubmitAnswerWork, agent: SharedAgent) -> SubmitAnswerOutcome {
    let question_id = work.question_id.clone();
    let result = (|| -> Result<(RunId, Option<String>), String> {
        append_answer(
            &work.transcript,
            &work.question_id,
            &work.question_body,
            &work.answer_text,
            work.mc.as_deref(),
        )
        .map_err(|err| format!("Transcript write failed: {err}"))?;

        let record = AnswerRecord {
            id: work.question_id.clone(),
            option: work.mc.clone(),
            text_changed: work.text_changed,
            body: work.payload_body.clone(),
        };
        let payload =
            format_answer_payload(&[record]).map_err(|err| format!("Payload error: {err}"))?;
        let prompt = answer_processor_prompt(&work.config_path, &payload);
        let snapshot = file_contents(&work.question_path);

        let mut provider = agent
            .lock()
            .map_err(|_| "Agent busy (bootstrap in progress) — try again shortly".to_string())?;
        let handle = provider
            .start_answer_processor(work.cwd, prompt, &work.settings)
            .map_err(|err| format!("Failed to start answer processor: {err}"))?;
        Ok((handle.id, snapshot))
    })();
    SubmitAnswerOutcome {
        question_id,
        result,
    }
}

/// Build answer-processor `text_changed` / body and transcript answer text (req 4 / 4a).
fn build_proposed_answer_parts(
    original_proposed: Option<&str>,
    edited_proposed: &str,
    notes: &str,
) -> (Option<bool>, String, String) {
    let notes = notes.trim().to_string();
    let Some(original) = original_proposed.map(str::trim).filter(|s| !s.is_empty()) else {
        return (None, notes.clone(), notes);
    };
    let edited = edited_proposed.trim();
    if edited == original {
        // Unedited Accept — do not resend proposed_text as the body.
        let answer = if notes.is_empty() {
            "Accepted proposed text as written.".to_string()
        } else {
            notes.clone()
        };
        (Some(false), notes, answer)
    } else {
        let edited = edited.to_string();
        let answer = if notes.is_empty() {
            edited.clone()
        } else {
            format!("{edited}\n\nNotes: {notes}")
        };
        (Some(true), edited, answer)
    }
}

fn run_submit_action_work(work: SubmitActionWork, agent: SharedAgent) -> SubmitActionOutcome {
    let question_id = work.question_id.clone();
    let action = work.action.clone();
    let result = (|| -> Result<(RunId, Option<String>), String> {
        append_action(
            &work.transcript,
            &work.question_id,
            &work.action,
            Some(&work.notes),
            Some(&work.question_body),
        )
        .map_err(|err| format!("Transcript write failed: {err}"))?;

        let record = ActionRecord {
            action: work.action.clone(),
            id: work.question_id.clone(),
            body: work.notes.clone(),
        };
        let payload =
            format_action_payload(&[record]).map_err(|err| format!("Payload error: {err}"))?;
        let prompt = researcher_action_prompt(&work.config_path, &payload);
        let snapshot = file_contents(&work.question_path);

        let mut provider = agent
            .lock()
            .map_err(|_| "Agent busy (bootstrap in progress) — try again shortly".to_string())?;
        let handle = provider
            .start_researcher_replenishment(work.cwd, prompt)
            .map_err(|err| format!("Failed to start researcher: {err}"))?;
        Ok((handle.id, snapshot))
    })();
    SubmitActionOutcome {
        question_id,
        action,
        result,
    }
}

fn researcher_status_kind(path: Option<&Path>) -> ResearcherStatusKind {
    let Some(path) = path else {
        return ResearcherStatusKind::Unknown;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return ResearcherStatusKind::Unknown;
    };
    researcher_status_from_text(&text)
}

fn researcher_status_from_text(text: &str) -> ResearcherStatusKind {
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("status:") {
            let value = rest.trim().to_ascii_lowercase();
            return if value.contains("complete") {
                ResearcherStatusKind::Complete
            } else if value.contains("working") {
                ResearcherStatusKind::Working
            } else if value.contains("idle") {
                ResearcherStatusKind::Idle
            } else {
                ResearcherStatusKind::Unknown
            };
        }
    }
    ResearcherStatusKind::Unknown
}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl HasAppNav for WorkspaceView {
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

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Align ListState after queue polls that only had App context (no Window).
        self.sync_question_list_selection(window, cx, false);
        self.sync_proposed_input(window, cx);

        if let Some(text) = self.pending_notes_paste.take() {
            self.notes_input.update(cx, |input, cx| {
                input.set_value(text, window, cx);
            });
        }

        if let Some(deep_dive) = &self.deep_dive {
            return div()
                .size_full()
                .min_w_0()
                .overflow_hidden()
                .child(deep_dive.clone());
        }

        let background = cx.theme().background;
        let border = cx.theme().border;
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let archived = self.session.status == InterviewSessionStatus::Archived;

        div()
            .key_context(WORKSPACE_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(background)
            .v_flex()
            .on_action(cx.listener(|this, _: &SubmitAnswer, window, cx| {
                this.submit_answer(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusNotes, window, cx| {
                if this.can_edit_notes() {
                    this.focus_notes(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &McDigit1, window, cx| {
                this.on_digit_key("1", window, cx);
            }))
            .on_action(cx.listener(|this, _: &McDigit2, window, cx| {
                this.on_digit_key("2", window, cx);
            }))
            .on_action(cx.listener(|this, _: &McDigit3, window, cx| {
                this.on_digit_key("3", window, cx);
            }))
            .on_action(cx.listener(|this, _: &McDigit4, window, cx| {
                this.on_digit_key("4", window, cx);
            }))
            .on_action(cx.listener(|this, _: &McDigit5, window, cx| {
                this.on_digit_key("5", window, cx);
            }))
            .on_action(cx.listener(|this, _: &McDigit6, window, cx| {
                this.on_digit_key("6", window, cx);
            }))
            .on_action(cx.listener(|this, _: &McDigit7, window, cx| {
                this.on_digit_key("7", window, cx);
            }))
            .on_action(cx.listener(|this, _: &McDigit8, window, cx| {
                this.on_digit_key("8", window, cx);
            }))
            .on_action(cx.listener(|this, _: &McDigit9, window, cx| {
                this.on_digit_key("9", window, cx);
            }))
            .on_action(cx.listener(|this, _: &QuestionMoveUp, window, cx| {
                if this.actions_menu_focused(window, cx) {
                    cx.propagate();
                    return;
                }
                if this.response_text_editing() {
                    cx.propagate();
                    return;
                }
                match this.workspace_focus {
                    WorkspaceFocus::QuestionList => {
                        this.move_question_list_by(-1, window, cx);
                        cx.stop_propagation();
                    }
                    WorkspaceFocus::Response(_) => {
                        this.move_response_focus(-1, window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &QuestionMoveDown, window, cx| {
                if this.actions_menu_focused(window, cx) {
                    cx.propagate();
                    return;
                }
                if this.response_text_editing() {
                    cx.propagate();
                    return;
                }
                match this.workspace_focus {
                    WorkspaceFocus::QuestionList => {
                        this.move_question_list_by(1, window, cx);
                        cx.stop_propagation();
                    }
                    WorkspaceFocus::Response(_) => {
                        this.move_response_focus(1, window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &ListArrowUp, window, cx| {
                if this.workspace_focus == WorkspaceFocus::QuestionList {
                    this.move_question_list_by(-1, window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &ListArrowDown, window, cx| {
                if this.workspace_focus == WorkspaceFocus::QuestionList {
                    this.move_question_list_by(1, window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &FocusRight, window, cx| {
                if this.response_text_editing() {
                    cx.propagate();
                    return;
                }
                if this.workspace_focus == WorkspaceFocus::QuestionList {
                    this.focus_response_right(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &FocusLeft, window, cx| {
                if this.response_text_editing() {
                    cx.propagate();
                    return;
                }
                if matches!(this.workspace_focus, WorkspaceFocus::Response(_)) {
                    this.focus_list_left(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &ActivateFocused, window, cx| {
                if this.actions_menu_focused(window, cx) {
                    cx.propagate();
                    return;
                }
                if this.response_text_editing() {
                    cx.propagate();
                    return;
                }
                this.activate_focused(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(on_app_nav_toggle::<Self>))
            .on_action(cx.listener(|this, _: &NavigateBack, _, cx| {
                this.navigate_back(cx);
            }))
            .on_action(cx.listener(|this, _: &WorkspaceEscape, window, cx| {
                if this.actions_menu_focused(window, cx) {
                    cx.propagate();
                    return;
                }
                this.handle_workspace_escape(window, cx);
                cx.stop_propagation();
            }))
            .child(self.render_workspace_header(window, cx, border, muted))
            .when(archived, |el| el.child(archived_banner(border, muted)))
            .when_some(self.error_banner.clone(), |el, msg| {
                el.child(error_banner(msg, border))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .overflow_hidden()
                    .child(
                        h_resizable("workspace-columns")
                            .child(
                                resizable_panel()
                                    .size(px(LIST_COLUMN_WIDTH))
                                    .size_range(px(LIST_COLUMN_MIN)..Pixels::MAX)
                                    .child(question_list_column(&self.question_list_state, muted)),
                            )
                            .child(
                                resizable_panel()
                                    .size(px(BODY_COLUMN_WIDTH))
                                    .size_range(px(BODY_COLUMN_MIN)..Pixels::MAX)
                                    .child(body_column(
                                        cx,
                                        self.is_complete(),
                                        self.researcher_in_flight() > 0 || self.scaffolding_pending,
                                        self.selected_question(),
                                        &self.session,
                                        self.task_list_proceed.is_some(),
                                        foreground,
                                        muted,
                                    )),
                            )
                            .child(
                                resizable_panel()
                                    .size_range(px(RESPONSE_COLUMN_MIN)..Pixels::MAX)
                                    .child(response_column(
                                        cx,
                                        window,
                                        self.selected_question(),
                                        self.is_question_pending(
                                            self.selected_question_id.as_deref().unwrap_or(""),
                                        ),
                                        self.is_question_removing(
                                            self.selected_question_id.as_deref().unwrap_or(""),
                                        ),
                                        &self.selected_mc,
                                        &self.proposed_input,
                                        &self.notes_input,
                                        self.can_mutate(),
                                        self.can_edit_notes(),
                                        self.has_proposed_editor(),
                                        self.workspace_focus,
                                        self.proposed_editing,
                                        self.notes_editing,
                                        self.actions_menu_open,
                                        self.actions_menu.clone(),
                                        &self.focus_handle,
                                        muted,
                                    )),
                            ),
                    ),
            )
            .child(status_footer(
                cx,
                &self.status_line,
                &self.answer_pool_footer_text(),
                border,
                muted,
                self.replenish_state.manual_required,
            ))
    }
}

fn archived_banner(border: gpui::Hsla, muted: gpui::Hsla) -> impl IntoElement {
    div()
        .px_4()
        .py_2()
        .border_b_1()
        .border_color(border)
        .text_sm()
        .text_color(muted)
        .child("Archived — answer submit and replenishment are blocked")
}

fn error_banner(message: SharedString, border: gpui::Hsla) -> impl IntoElement {
    div()
        .px_4()
        .py_2()
        .bg(gpui::red())
        .text_color(gpui::white())
        .border_b_1()
        .border_color(border)
        .child(message)
}

fn question_list_column(
    list_state: &Entity<ListState<QuestionListDelegate>>,
    muted: gpui::Hsla,
) -> impl IntoElement {
    v_flex()
        .size_full()
        .min_w_0()
        .overflow_hidden()
        .child(
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(muted)
                .child("Open questions"),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .size_full()
                .child(List::new(list_state).size_full()),
        )
}

fn body_column(
    cx: &mut Context<WorkspaceView>,
    complete: bool,
    researcher_waiting: bool,
    question: Option<&QueueQuestion>,
    session: &InterviewSession,
    show_proceed: bool,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let body = if complete {
        complete_body(cx, session, show_proceed, foreground, muted).into_any_element()
    } else if let Some(q) = question {
        question_body_view(q, foreground, muted).into_any_element()
    } else if researcher_waiting {
        div()
            .text_sm()
            .text_color(muted)
            .child("Waiting for researcher…")
            .into_any_element()
    } else {
        div()
            .text_sm()
            .text_color(muted)
            .child("No open questions")
            .into_any_element()
    };
    v_flex()
        .size_full()
        .min_w_0()
        .overflow_hidden()
        .p_4()
        .child(
            div()
                .id("question-body-scroll")
                .size_full()
                .min_w_0()
                .overflow_hidden()
                .overflow_y_scroll()
                .child(body),
        )
}

fn question_body_view(
    q: &QueueQuestion,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let has_structured = q.context.is_some() || q.question.is_some();
    let mut col = v_flex().w_full().min_w_0().gap_3();

    if let Some(context) = q
        .context
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        col = col.child(
            div()
                .w_full()
                .min_w_0()
                .text_sm()
                .text_color(muted)
                .child(context.to_string()),
        );
    }

    if let Some(question) = q
        .question
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        col = col.child(
            div()
                .w_full()
                .min_w_0()
                .text_sm()
                .font_semibold()
                .text_color(foreground)
                .child(question.to_string()),
        );
    } else if !has_structured {
        let legacy = crate::interview::queue::strip_mc_option_lines(&q.body, &q.options);
        let (legacy, _) = crate::interview::queue::split_recommend_from_body(&legacy);
        if !legacy.trim().is_empty() {
            col = col.child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_sm()
                    .text_color(foreground)
                    .child(legacy),
            );
        }
    }

    if let Some(recommend) = q
        .recommend
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        col = col.child(
            div()
                .w_full()
                .min_w_0()
                .text_sm()
                .text_color(muted)
                .child(format!("Recommend: {recommend}")),
        );
    }

    col
}

fn complete_body(
    cx: &mut Context<WorkspaceView>,
    session: &InterviewSession,
    show_proceed: bool,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let mut col = v_flex()
        .gap_3()
        .child(
            div()
                .text_lg()
                .font_semibold()
                .text_color(foreground)
                .child("Complete"),
        )
        .child(div().text_sm().text_color(muted).child(format!(
            "No open questions remain for \"{}\".",
            session.display_name
        )));

    if show_proceed {
        col = col.child(
            Button::new("proceed-lifecycle")
                .primary()
                .label("Proceed")
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.emit(WorkspaceEvent::ProceedToLifecycle);
                })),
        );
    }

    col
}

fn response_column(
    cx: &mut Context<WorkspaceView>,
    _window: &mut Window,
    question: Option<&QueueQuestion>,
    pending: bool,
    removing: bool,
    selected_mc: &Option<String>,
    proposed_input: &Entity<InputState>,
    notes_input: &Entity<InputState>,
    can_mutate: bool,
    can_edit_notes: bool,
    show_proposed: bool,
    workspace_focus: WorkspaceFocus,
    proposed_editing: bool,
    notes_editing: bool,
    actions_menu_open: bool,
    actions_menu: Option<Entity<PopupMenu>>,
    _workspace_focus_handle: &FocusHandle,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let locked = pending || removing;
    let disabled = !can_mutate || locked || question.is_none();
    let proposed_input_disabled =
        !can_edit_notes || locked || question.is_none() || !proposed_editing;
    let notes_input_disabled = !can_edit_notes || locked || question.is_none() || !notes_editing;
    let focused_idx = match workspace_focus {
        WorkspaceFocus::Response(i) => Some(i),
        WorkspaceFocus::QuestionList => None,
    };
    let mut col = v_flex()
        .id("response-column")
        .size_full()
        .min_w_0()
        .overflow_hidden()
        .p_3()
        .gap_2()
        .child(div().text_xs().text_color(muted).child(if removing {
            "Removing…"
        } else if pending {
            "Pending — waiting for agent"
        } else {
            "Response"
        }));
    let mut stop_idx = 0usize;
    if let Some(q) = question {
        for (idx, opt) in q.options.iter().enumerate() {
            let key = opt.key.clone();
            let focused = focused_idx == Some(stop_idx);
            col = col.child(mc_option_row(
                cx,
                idx,
                opt.clone(),
                selected_mc.as_ref().is_some_and(|k| k == &key),
                focused,
                muted,
                disabled,
            ));
            stop_idx += 1;
        }
    }
    let proposed_focused = show_proposed && focused_idx == Some(stop_idx);
    if show_proposed {
        stop_idx += 1;
    }
    let notes_focused = focused_idx == Some(stop_idx);
    stop_idx += 1;
    let actions_focused = focused_idx == Some(stop_idx);
    stop_idx += 1;
    let submit_focused = focused_idx == Some(stop_idx);
    let notes_view = cx.entity();
    let proposed_view = cx.entity();

    let mut footer = v_flex()
        .id("response-footer")
        .w_full()
        .min_w_0()
        .flex_none()
        .flex_shrink_0()
        .gap_2();

    if show_proposed {
        footer = footer
            .child(div().text_xs().text_color(muted).child("Proposed text"))
            .child(
                ListItem::new("proposed-field")
                    .selected(proposed_focused)
                    .w_full()
                    .h(px(100.))
                    .overflow_hidden()
                    .on_click(move |_, window, app| {
                        proposed_view.update(app, |this, cx| {
                            if this.can_edit_notes() {
                                this.enter_proposed_edit(window, cx);
                            }
                        });
                    })
                    .child(
                        Input::new(proposed_input)
                            .disabled(proposed_input_disabled)
                            .w_full()
                            .h(px(100.)),
                    ),
            );
    }

    footer = footer
        .child(
            ListItem::new("notes-field")
                .selected(notes_focused)
                .w_full()
                .h(px(80.))
                .overflow_hidden()
                .on_click(move |_, window, app| {
                    notes_view.update(app, |this, cx| {
                        if this.can_edit_notes() {
                            this.enter_notes_edit(window, cx);
                        }
                    });
                })
                .child(
                    Input::new(notes_input)
                        .disabled(notes_input_disabled)
                        .w_full()
                        .h(px(80.)),
                ),
        )
        .child(
            h_flex()
                .id("response-actions")
                .w_full()
                .min_w_0()
                .flex_none()
                .justify_between()
                .items_center()
                .gap_1()
                .child(
                    ListItem::new("other-actions-focus")
                        .selected(actions_focused && !actions_menu_open)
                        .child(action_dropdown(
                            cx,
                            disabled,
                            actions_menu_open,
                            actions_focused,
                            actions_menu,
                        )),
                )
                .child(
                    ListItem::new("submit-focus")
                        .selected(submit_focused)
                        .child(
                            Button::new("submit-answer")
                                .label("Submit")
                                .primary()
                                .compact()
                                .disabled(disabled)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        this.submit_answer(window, cx);
                                        cx.stop_propagation();
                                    }),
                                ),
                        ),
                ),
        );

    col.child(footer)
}

fn populate_action_menu(menu: PopupMenu, view: WeakEntity<WorkspaceView>) -> PopupMenu {
    OTHER_ACTION_ITEMS
        .iter()
        .fold(menu, |menu, (action, label)| {
            let view = view.clone();
            let action = (*action).to_string();
            menu.item(PopupMenuItem::new(*label).on_click(move |_, window, cx| {
                if let Some(entity) = view.upgrade() {
                    entity.update(cx, |this, cx| {
                        this.set_actions_menu_open(false, cx);
                        this.submit_action(&action, window, cx);
                    });
                }
            }))
        })
}

/// Native `PopupMenu` anchored under the trigger.
///
/// Stock `Button::dropdown_menu` wraps the menu in a deferred `Popover`, which
/// drops the menu out of the focus/dispatch tree (keyboard SelectUp/Down no-op).
/// Eager anchored `PopupMenu` keeps native menu keyboard + MenuItemElement
/// `.selected` chrome (theme accent bumped toward `list_active_border` at init).
fn action_dropdown(
    cx: &mut Context<WorkspaceView>,
    disabled: bool,
    menu_open: bool,
    actions_focused: bool,
    actions_menu: Option<Entity<PopupMenu>>,
) -> impl IntoElement {
    div()
        .id("question-actions-dropdown")
        .relative()
        .child(
            Button::new("actions-trigger")
                .label("Other action")
                .dropdown_caret(true)
                .compact()
                .disabled(disabled)
                .selected(actions_focused || menu_open)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_actions_menu_from_pointer(window, cx);
                })),
        )
        .when(menu_open, |el| {
            el.when_some(actions_menu, |el, menu| {
                el.child(
                    anchored()
                        .anchor(Corner::TopLeft)
                        .snap_to_window_with_margin(px(8.))
                        .child(div().occlude().mt_1().child(menu)),
                )
            })
        })
}

fn mc_option_row(
    cx: &mut Context<WorkspaceView>,
    idx: usize,
    opt: crate::interview::queue::McOption,
    selected: bool,
    focused: bool,
    muted: gpui::Hsla,
    disabled: bool,
) -> impl IntoElement {
    let key = opt.key.clone();
    let submit_key = key.clone();
    let label: SharedString = format!("{}. {}", opt.key, opt.label).into();
    let _ = muted;

    ListItem::new(("mc-option", idx))
        .selected(focused || selected)
        .disabled(disabled)
        .w_full()
        .min_w_0()
        .child(
            Button::new(("mc-option-btn", idx))
                .label(label)
                .ghost()
                .compact()
                .w_full()
                .disabled(disabled)
                .selected(focused || selected)
                .on_click(cx.listener(move |this, _, window, cx| {
                    if !disabled {
                        this.submit_mc_option(&submit_key, window, cx);
                    }
                })),
        )
}

fn status_footer(
    cx: &mut Context<WorkspaceView>,
    status: &SharedString,
    pool_stats: &SharedString,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    show_manual_kickoff: bool,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .px_4()
        .py_2()
        .border_t_1()
        .border_color(border)
        .justify_between()
        .items_center()
        .gap_3()
        .child(
            div()
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .text_xs()
                .text_color(muted)
                .child(if status.is_empty() {
                    "Ready".into()
                } else {
                    status.clone()
                }),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(muted)
                .child(pool_stats.clone()),
        )
        .when(show_manual_kickoff, |el| {
            el.child(
                Button::new("manual-researcher-kickoff")
                    .label("Kickoff researcher")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.manual_researcher_kickoff(cx);
                    })),
            )
        })
}

impl gpui::EventEmitter<WorkspaceEvent> for WorkspaceView {}

#[cfg(test)]
mod tests {
    use super::{
        ResearcherStatusKind, RunKind, WorkspaceInFlightState, build_proposed_answer_parts,
        reopen_complete_with_bound_queue, researcher_status_from_text, update_replenish_idle_since,
    };
    use crate::interview::agent::RunId;
    use crate::interview::queue::QueueQuestion;
    use crate::interview::{InterviewSessionStatus, SessionStore, TodPaths};
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::time::Instant;

    #[test]
    fn idle_grace_starts_on_transition_from_working() {
        assert!(
            update_replenish_idle_since(
                ResearcherStatusKind::Working,
                ResearcherStatusKind::Idle,
                None,
            )
            .is_some()
        );
    }

    #[test]
    fn idle_grace_not_started_for_stale_idle_at_replenish_start() {
        assert!(
            update_replenish_idle_since(
                ResearcherStatusKind::Idle,
                ResearcherStatusKind::Idle,
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn idle_grace_cleared_when_agent_working() {
        assert!(
            update_replenish_idle_since(
                ResearcherStatusKind::Idle,
                ResearcherStatusKind::Working,
                Some(Instant::now()),
            )
            .is_none()
        );
    }

    #[test]
    fn idle_grace_continues_while_still_idle() {
        let since = Instant::now();
        assert_eq!(
            update_replenish_idle_since(
                ResearcherStatusKind::Idle,
                ResearcherStatusKind::Complete,
                Some(since),
            ),
            Some(since),
        );
    }

    #[test]
    fn parses_researcher_status_kinds() {
        assert_eq!(
            researcher_status_from_text("status: complete\n"),
            ResearcherStatusKind::Complete
        );
        assert_eq!(
            researcher_status_from_text("status: idle\n"),
            ResearcherStatusKind::Idle
        );
        assert_eq!(
            researcher_status_from_text("status: working\n"),
            ResearcherStatusKind::Working
        );
        assert_eq!(
            researcher_status_from_text("notes: only\n"),
            ResearcherStatusKind::Unknown
        );
    }

    #[test]
    fn reopen_complete_with_nonempty_queue_flips_active() {
        let dir = std::env::temp_dir().join(format!("tod-reopen-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let paths = TodPaths::from_repo_root(dir.clone());
        let store = SessionStore::open(&paths).unwrap();
        let mut session = store
            .insert_session("Complete with queue", InterviewSessionStatus::Complete)
            .unwrap();
        assert_eq!(session.status, InterviewSessionStatus::Complete);

        assert!(reopen_complete_with_bound_queue(&mut session, &store, true));
        assert_eq!(session.status, InterviewSessionStatus::Active);
        let reloaded = store.get_session(session.id).unwrap().unwrap();
        assert_eq!(reloaded.status, InterviewSessionStatus::Active);

        // Empty queue: leave Complete alone.
        let mut done = store
            .insert_session("Truly complete", InterviewSessionStatus::Complete)
            .unwrap();
        assert!(!reopen_complete_with_bound_queue(&mut done, &store, false));
        assert_eq!(done.status, InterviewSessionStatus::Complete);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn in_flight_prune_drops_removed_and_modified_questions() {
        let dir = std::env::temp_dir().join(format!("tod-inflight-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path_keep = dir.join("q-001.md");
        let path_mod = dir.join("q-002.md");
        fs::write(&path_keep, "same").unwrap();
        fs::write(&path_mod, "changed").unwrap();

        let questions = vec![
            QueueQuestion {
                id: "q-001".into(),
                path: path_keep.clone(),
                created: None,
                layer: None,
                kind: None,
                covers: Vec::new(),
                context: None,
                question: None,
                recommend: None,
                proposed_text: None,
                options: Vec::new(),
                body: "keep".into(),
                short_label: "keep".into(),
            },
            QueueQuestion {
                id: "q-002".into(),
                path: path_mod.clone(),
                created: None,
                layer: None,
                kind: None,
                covers: Vec::new(),
                context: None,
                question: None,
                recommend: None,
                proposed_text: None,
                options: Vec::new(),
                body: "mod".into(),
                short_label: "mod".into(),
            },
        ];

        let mut pending = HashSet::new();
        pending.insert("q-001".into());
        pending.insert("q-002".into());
        pending.insert("q-gone".into());
        let mut snapshots = HashMap::new();
        snapshots.insert("q-001".into(), "same".into());
        snapshots.insert("q-002".into(), "original".into());
        let mut runs = HashMap::new();
        runs.insert(
            RunId::new(),
            RunKind::AnswerProcessor {
                question_id: "q-001".into(),
            },
        );
        runs.insert(
            RunId::new(),
            RunKind::AnswerProcessor {
                question_id: "q-002".into(),
            },
        );
        runs.insert(
            RunId::new(),
            RunKind::AnswerProcessor {
                question_id: "q-gone".into(),
            },
        );
        runs.insert(RunId::new(), RunKind::ResearcherReplenish);

        let pruned = WorkspaceInFlightState {
            pending,
            pending_snapshots: snapshots,
            runs,
        }
        .pruned_for_queue(&questions);

        assert!(pruned.pending.contains("q-001"));
        assert!(!pruned.pending.contains("q-002"));
        assert!(!pruned.pending.contains("q-gone"));
        assert_eq!(pruned.runs.len(), 2); // q-001 answer + replenish
        assert!(pruned.runs.values().any(|k| matches!(
            k,
            RunKind::AnswerProcessor { question_id } if question_id == "q-001"
        )));
        assert!(
            pruned
                .runs
                .values()
                .any(|k| matches!(k, RunKind::ResearcherReplenish))
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn proposed_answer_parts_unchanged_omits_text_from_body() {
        let (changed, body, answer) =
            build_proposed_answer_parts(Some("Fleet uses SQLite."), "Fleet uses SQLite.", "");
        assert_eq!(changed, Some(false));
        assert!(body.is_empty());
        assert!(answer.contains("Accepted"));
    }

    #[test]
    fn proposed_answer_parts_edited_sends_full_text() {
        let (changed, body, answer) = build_proposed_answer_parts(
            Some("Fleet uses SQLite."),
            "Fleet uses SQLite under the storage root.",
            "extra note",
        );
        assert_eq!(changed, Some(true));
        assert_eq!(body, "Fleet uses SQLite under the storage root.");
        assert!(answer.contains("extra note"));
        assert!(answer.contains("storage root"));
    }

    #[test]
    fn proposed_answer_parts_absent_proposed_keeps_notes_only() {
        let (changed, body, answer) = build_proposed_answer_parts(None, "ignored", "just notes");
        assert_eq!(changed, None);
        assert_eq!(body, "just notes");
        assert_eq!(answer, "just notes");
    }
}
