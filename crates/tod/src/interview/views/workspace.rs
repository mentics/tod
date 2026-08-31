use crate::fleet::FleetStore;
use crate::fleet::{ensure_interview_agent_for_node, workspace_cwd_for_interview_agent};
use crate::interview::agent::{AgentRunState, RunId, SharedAgent};
use crate::interview::config::{
    InterviewConfig, parse_interview_config, sync_scaffolding_from_disk,
};
use crate::interview::kickoff::{
    answer_processor_prompt, question_maker_action_prompt, question_maker_replenish_prompt,
};
use crate::interview::question_feedback::append_question_feedback;
use crate::interview::queue::{QueueQuestion, load_queue_dir};
use crate::interview::queue_watcher::QueueWatcher;
use crate::interview::replenishment::{question_maker_starts_needed, retry_backoff_secs};
use crate::interview::settings::{AnswerProcessorSettings, QuestionMakerSettings};
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
use crate::outline::repos::NodeRepo;
use crate::process::lifecycle_for_interview_phase;
use crate::process_bundle::{
    InterviewAgentPrompt, ProcessManifest, TodInstallPaths, session_scratchpad,
};
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav, on_app_nav_toggle};
use crate::ui::list::{ListArrowDown, ListArrowUp};
use crate::ui::selectable_text::selectable_text;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClipboardItem, Context, Corner, DismissEvent, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, ParentElement, Pixels, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Timer, WeakEntity,
    Window, actions, anchored, deferred, div, px,
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
use uuid::Uuid;

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
    prompt: InterviewAgentPrompt,
    agent_config_id: String,
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
    prompt: InterviewAgentPrompt,
    agent_config_id: String,
    cwd: PathBuf,
    question_maker_settings: QuestionMakerSettings,
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
    /// Index into response interactive controls (MC options, optional proposed text, Notes,
    /// Other action, Submit, feedback field, Submit feedback).
    Response(usize),
}

#[derive(Debug, Clone)]
enum RunKind {
    AnswerProcessor { question_id: String },
    QuestionMakerReplenish,
    QuestionMakerAction { question_id: String },
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
            RunKind::QuestionMakerReplenish => true,
            RunKind::AnswerProcessor { question_id }
            | RunKind::QuestionMakerAction { question_id } => self.pending.contains(question_id),
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
    /// even if question-maker status still says `idle` (agent should set `complete`).
    exhausted: bool,
    /// When question-maker status first flipped to idle/complete during the current
    /// replenishment batch — grace period for ACP to exit after disk work is done.
    status_idle_since: Option<Instant>,
    /// Last observed question-maker status while replenishment is in flight.
    last_question_maker_status: QuestionMakerStatusKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum QuestionMakerStatusKind {
    Idle,
    Working,
    Complete,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default)]
struct QuestionMakerStatusSnapshot {
    kind: QuestionMakerStatusKind,
    notes: Option<String>,
    queue_depth: Option<u32>,
    queue_target: Option<u32>,
}

/// Rich waiting UI while bootstrap or replenishment is in flight.
#[derive(Debug, Clone)]
struct QuestionMakerWaitUi {
    headline: SharedString,
    detail: Option<SharedString>,
    queue_depth: Option<u32>,
    queue_target: Option<u32>,
    elapsed_secs: u64,
    animate_dots: usize,
}

impl QuestionMakerWaitUi {
    fn status_line(&self) -> SharedString {
        let mut line = self.headline.to_string();
        if self.elapsed_secs >= 5 {
            line.push_str(&format!(" ({})", format_elapsed(self.elapsed_secs)));
        }
        if let Some(detail) = &self.detail {
            if !detail.is_empty() {
                line.push_str(" — ");
                line.push_str(detail);
            }
        }
        line.into()
    }
}

const HUNG_REPLENISH_SECS: u64 = 45;

fn question_maker_status_is_idle(kind: QuestionMakerStatusKind) -> bool {
    matches!(
        kind,
        QuestionMakerStatusKind::Idle | QuestionMakerStatusKind::Complete
    )
}

/// Start or clear the idle grace timer when status changes during replenishment.
fn update_replenish_idle_since(
    last: QuestionMakerStatusKind,
    current: QuestionMakerStatusKind,
    idle_since: Option<Instant>,
) -> Option<Instant> {
    if question_maker_status_is_idle(current) {
        if idle_since.is_some() {
            idle_since
        } else if question_maker_status_is_idle(last) {
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
    fleet: Arc<FleetStore>,
    agent_config_id: String,
    workspace_cwd: PathBuf,
    questions: Vec<QueueQuestion>,
    pending: HashSet<String>,
    pending_snapshots: HashMap<String, String>,
    /// Queue ids flashing red before drop; values are snapshots kept in `questions` until finalize.
    removing: HashSet<String>,
    selected_question_id: Option<String>,
    selected_mc: Option<String>,
    notes_input: Entity<InputState>,
    proposed_input: Entity<InputState>,
    feedback_input: Entity<InputState>,
    /// Question id whose `proposed_text` is currently loaded into `proposed_input`.
    proposed_loaded_for: Option<String>,
    /// None until session scratchpad / queue is bound — never falls back to repo-root queue.
    queue_watcher: Option<QueueWatcher>,
    agent: SharedAgent,
    /// Session ids with a kickoff bootstrap thread still running.
    bootstrap_sessions: Arc<Mutex<HashSet<Uuid>>>,
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
    feedback_editing: bool,
    /// Open state for the native PopupMenu. Menu entity is eager (keyboard);
    /// paint uses `deferred` so it stacks above the bottom feedback panel.
    actions_menu_open: bool,
    /// Live PopupMenu entity while open (created eagerly so keyboard can drive it).
    actions_menu: Option<Entity<PopupMenu>>,
    _actions_menu_subscription: Option<Subscription>,
    scaffolding_pending: bool,
    needs_bootstrap_handoff: bool,
    /// When the current question-maker wait began (bootstrap or replenish).
    question_maker_wait_started: Option<Instant>,
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
        fleet: Arc<FleetStore>,
        bootstrap_sessions: Arc<Mutex<HashSet<Uuid>>>,
        restored_in_flight: Option<WorkspaceInFlightState>,
        task_list_proceed: Option<TaskListProceedContext>,
    ) -> Self {
        register_workspace_keys(cx);
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let store = SessionStore::open(fleet.clone());
        let settings = TodSettings::load(&paths).unwrap_or_default();
        let (agent_config_id, workspace_cwd) = {
            if let Some(ref id) = session.agent_config_id {
                let cwd = workspace_cwd_for_interview_agent(&fleet, id, &paths, session.node_id)
                    .unwrap_or_else(|_| paths.repo_root().to_path_buf());
                (id.clone(), cwd)
            } else {
                match ensure_interview_agent_for_node(
                    &fleet,
                    &paths,
                    &settings,
                    &session.node_id.to_string(),
                ) {
                    Ok(ctx) => (ctx.agent.id, ctx.cwd),
                    Err(_) => (
                        format!("interview-{}", session.node_id),
                        paths.repo_root().to_path_buf(),
                    ),
                }
            }
        };

        let bound_config_path = session_config_path(&session);

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
        let feedback_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(3)
                .placeholder("Feedback on this question (e.g. not useful, too meta)")
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
                        this.feedback_editing = false;
                        this.select_question_without_window(&id, cx);
                    }
                }
                ListEvent::Cancel => {}
            });

        let status_line = if scaffolding_pending {
            if bootstrap_in_progress {
                "Question maker bootstrap in progress…".into()
            } else {
                "Waiting for question maker scaffolding…".into()
            }
        } else if !pending.is_empty() {
            "Waiting for in-flight answers to finish…".into()
        } else {
            SharedString::default()
        };

        let question_maker_wait_started = if scaffolding_pending {
            Some(Instant::now())
        } else {
            None
        };

        let view = Self {
            session,
            config,
            settings,
            store,
            fleet,
            agent_config_id,
            workspace_cwd,
            questions,
            pending,
            pending_snapshots,
            removing: HashSet::new(),
            selected_question_id,
            selected_mc: None,
            notes_input,
            proposed_input,
            feedback_input,
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
            feedback_editing: false,
            actions_menu_open: false,
            actions_menu: None,
            _actions_menu_subscription: None,
            scaffolding_pending,
            needs_bootstrap_handoff: false,
            question_maker_wait_started,
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

    fn question_maker_waiting(&self) -> bool {
        self.scaffolding_pending || self.question_maker_in_flight() > 0
    }

    fn session_scratchpad_dir(&self) -> PathBuf {
        if !self.config.scratchpad.as_os_str().is_empty() {
            return self.config.scratchpad.clone();
        }
        if let Some(p) = self.session.scratchpad_path.as_ref() {
            return PathBuf::from(p);
        }
        let root = TodPaths::discover()
            .map(|p| p.repo_root().to_path_buf())
            .unwrap_or_else(|_| PathBuf::from("."));
        session_scratchpad(&root, self.session.node_id, &self.session.id.to_string())
    }

    fn read_question_maker_status_snapshot(&self) -> QuestionMakerStatusSnapshot {
        let scratchpad = self.session_scratchpad_dir();
        let primary = scratchpad.join("question-maker-status.md");
        let legacy = scratchpad.join("researcher-status.md");
        let path = if primary.is_file() {
            Some(primary)
        } else if legacy.is_file() {
            Some(legacy)
        } else {
            None
        };
        question_maker_status_snapshot(path.as_deref())
    }

    fn bootstrap_stage_detail(&self) -> Option<String> {
        let scratchpad = self.session_scratchpad_dir();
        if !scratchpad.is_dir() {
            return Some("Starting question maker agent…".into());
        }
        let config_path = scratchpad.join("interview-config.md");
        if !config_path.is_file() {
            return Some("Creating session files…".into());
        }
        let queue_dir = scratchpad.join("queue");
        let count = count_queue_files(&queue_dir);
        if count == 0 {
            return Some("Generating initial questions…".into());
        }
        Some(format!(
            "Generated {count} question{}…",
            if count == 1 { "" } else { "s" }
        ))
    }

    fn build_question_maker_wait_ui(&self) -> QuestionMakerWaitUi {
        let started = self
            .question_maker_wait_started
            .unwrap_or_else(Instant::now);
        let elapsed_secs = started.elapsed().as_secs();
        let animate_dots = ((started.elapsed().as_millis() / 500) % 4) as usize;
        let snapshot = self.read_question_maker_status_snapshot();

        if self.scaffolding_pending {
            let bootstrap_running = self.session_bootstrap_in_flight();
            let headline = if bootstrap_running {
                "Question maker is setting up this interview"
            } else {
                "Waiting for question maker to start"
            };
            let detail = snapshot
                .notes
                .clone()
                .or_else(|| self.bootstrap_stage_detail());
            return QuestionMakerWaitUi {
                headline: headline.into(),
                detail: detail.map(Into::into),
                queue_depth: snapshot.queue_depth,
                queue_target: snapshot.queue_target.or(Some(8)),
                elapsed_secs,
                animate_dots,
            };
        }

        let headline = match snapshot.kind {
            QuestionMakerStatusKind::Working => "Question maker is working",
            QuestionMakerStatusKind::Complete => "Question maker finishing up",
            _ => "Question maker is preparing questions",
        };
        let detail = snapshot.notes.or_else(|| {
            Some(match snapshot.kind {
                QuestionMakerStatusKind::Working => "Updating the question queue…".into(),
                _ => "Agent run in progress…".into(),
            })
        });
        QuestionMakerWaitUi {
            headline: headline.into(),
            detail: detail.map(Into::into),
            queue_depth: snapshot.queue_depth,
            queue_target: snapshot
                .queue_target
                .or(self.config.queue_target)
                .or(Some(self.settings.question_maker.replenish_threshold)),
            elapsed_secs,
            animate_dots,
        }
    }

    fn sync_question_maker_wait_display(&mut self) {
        if self.question_maker_waiting() {
            if self.question_maker_wait_started.is_none() {
                self.question_maker_wait_started = Some(Instant::now());
            }
            let ui = self.build_question_maker_wait_ui();
            self.status_line = ui.status_line();
        } else {
            self.question_maker_wait_started = None;
        }
    }

    fn question_maker_in_flight(&self) -> usize {
        self.runs
            .values()
            .filter(|kind| {
                matches!(
                    kind,
                    RunKind::QuestionMakerReplenish | RunKind::QuestionMakerAction { .. }
                )
            })
            .count()
    }

    fn answer_in_flight(&self) -> bool {
        self.runs
            .values()
            .any(|kind| matches!(kind, RunKind::AnswerProcessor { .. }))
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

    fn question_maker_status_path(&self) -> PathBuf {
        self.session_scratchpad_dir()
            .join("question-maker-status.md")
    }

    fn current_question_maker_status(&self) -> QuestionMakerStatusKind {
        let primary = self.question_maker_status_path();
        let legacy = self.session_scratchpad_dir().join("researcher-status.md");
        let path = if primary.is_file() {
            Some(primary.as_path())
        } else if legacy.is_file() {
            Some(legacy.as_path())
        } else {
            None
        };
        question_maker_status_kind(path)
    }

    fn build_answer_processor_prompt(
        &self,
        payload: &str,
    ) -> Result<(InterviewAgentPrompt, PathBuf), String> {
        let paths = TodPaths::discover().map_err(|e| e.to_string())?;
        let install = TodInstallPaths::discover().map_err(|e| e.to_string())?;
        let manifest = ProcessManifest::load(&install).map_err(|e| e.to_string())?;
        let prompt = answer_processor_prompt(
            &self.fleet,
            &install,
            &manifest,
            &paths,
            self.config.node_id,
            &self.config.phase,
            &self.config.scratchpad,
            &self.config.config_path,
            payload,
            Some(&self.agent_config_id),
        )
        .map_err(|e| e.to_string())?;
        Ok((prompt, self.workspace_cwd.clone()))
    }

    fn build_question_maker_action_prompt(
        &self,
        payload: &str,
    ) -> Result<(InterviewAgentPrompt, PathBuf), String> {
        let paths = TodPaths::discover().map_err(|e| e.to_string())?;
        let install = TodInstallPaths::discover().map_err(|e| e.to_string())?;
        let manifest = ProcessManifest::load(&install).map_err(|e| e.to_string())?;
        let prompt = question_maker_action_prompt(
            &self.fleet,
            &install,
            &manifest,
            &paths,
            self.config.node_id,
            &self.config.phase,
            &self.config.scratchpad,
            &self.config.config_path,
            payload,
            Some(&self.agent_config_id),
        )
        .map_err(|e| e.to_string())?;
        Ok((prompt, self.workspace_cwd.clone()))
    }

    fn interview_node_context(&self) -> (String, String) {
        let fleet_projection = self.fleet.projection();
        let guard = fleet_projection.lock().expect("fleet projection mutex");
        let conn = guard.connection();
        let node_repo = NodeRepo::new(&conn);
        let title = node_repo
            .get(self.session.node_id)
            .ok()
            .flatten()
            .map(|n| n.title)
            .unwrap_or_else(|| self.session.display_name.clone());
        let lifecycle = node_repo
            .get_lifecycle(self.session.node_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| lifecycle_for_interview_phase(&self.config.phase).into());
        (title, lifecycle)
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

    fn feedback_focused(&self, window: &Window, cx: &App) -> bool {
        self.feedback_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    }

    fn response_text_editing(&self) -> bool {
        self.notes_editing || self.proposed_editing || self.feedback_editing
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

    /// If kickoff left paths NULL, pick up question maker scaffolding when it appears on disk.
    fn try_bind_bootstrap_scaffolding(&mut self, cx: &mut Context<Self>) -> bool {
        if session_config_path(&self.session).is_some() {
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
        let Some(config_path) = session_config_path(&session) else {
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
            session_id = %self.session.id.to_string(),
            questions = self.questions.len(),
            queue = %self.config.queue.display(),
            "workspace bound bootstrap scaffolding"
        );
        self.status_line = "Scaffolding bound from question maker bootstrap".into();
        true
    }

    fn sync_scaffolding_status_line(&mut self) {
        if self.scaffolding_pending {
            self.sync_question_maker_wait_display();
        }
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
            self.question_maker_wait_started = None;
            cx.emit(WorkspaceEvent::SessionComplete);
            changed = true;
        }

        if self.question_maker_waiting() {
            self.sync_question_maker_wait_display();
            changed = true;
        }

        changed
    }

    /// If ACP stays InFlight but question-maker status is idle/complete for long enough
    /// after flipping idle, cancel the hung replenish runs as failure (do not mark
    /// success / exhausted).
    fn reconcile_hung_replenishment(&mut self, cx: &mut Context<Self>) -> bool {
        if self.question_maker_in_flight() == 0 {
            self.replenish_state.status_idle_since = None;
            return false;
        }
        let current = self.current_question_maker_status();
        let last = self.replenish_state.last_question_maker_status;
        self.replenish_state.status_idle_since =
            update_replenish_idle_since(last, current, self.replenish_state.status_idle_since);
        self.replenish_state.last_question_maker_status = current;

        let Some(idle_since) = self.replenish_state.status_idle_since else {
            return false;
        };
        if idle_since.elapsed() < Duration::from_secs(HUNG_REPLENISH_SECS) {
            return false;
        }
        let hung: Vec<_> = self
            .runs
            .iter()
            .filter(|(_, k)| matches!(k, RunKind::QuestionMakerReplenish))
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
                Err("Question maker replenishment timed out (cancelled hung run)".into()),
                cx,
            );
        }
        true
    }

    fn sync_status_line_hygiene(&mut self) {
        if self.question_maker_in_flight() == 0
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
        self.replenish_state.last_question_maker_status = self.current_question_maker_status();
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
                    RunKind::QuestionMakerReplenish => {
                        self.replenish_state.retry_count = 0;
                        self.replenish_state.next_retry_at = None;
                        self.replenish_state.status_idle_since = None;
                        if self.live_question_count() == 0 && self.removing.is_empty() {
                            // AP may wipe the queue while the question maker is still mid-refill
                            // (`status: working`). Do not treat that as interview exhausted.
                            let status = self.current_question_maker_status();
                            if matches!(status, QuestionMakerStatusKind::Working) {
                                self.replenish_state.exhausted = false;
                                self.status_line = "Question maker still filling the queue…".into();
                            } else {
                                self.replenish_state.exhausted = true;
                                self.status_line =
                                    "Question maker returned no further questions".into();
                            }
                        } else {
                            self.replenish_state.exhausted = false;
                            self.status_line = "Question maker replenishment succeeded".into();
                        }
                    }
                    RunKind::QuestionMakerAction { question_id } => {
                        self.status_line =
                            format!("Question maker action completed for {question_id}").into();
                    }
                }
            }
            (RunKind::AnswerProcessor { question_id }, Err(message)) => {
                self.error_banner = Some(message.into());
                self.status_line = "Answer processor failed".into();
                self.pending.remove(question_id);
                self.pending_snapshots.remove(question_id);
            }
            (RunKind::QuestionMakerReplenish, Err(message)) => {
                self.error_banner = Some(message.clone().into());
                self.status_line = "Question maker replenishment failed".into();
                self.replenish_state.status_idle_since = None;
                self.replenish_state.retry_count += 1;
                if self.replenish_state.retry_count >= MAX_RESEARCHER_RETRIES {
                    self.replenish_state.manual_required = true;
                    self.status_line =
                        "Question maker failed — use Kickoff question maker to retry".into();
                } else {
                    let delay = retry_backoff_secs(self.replenish_state.retry_count - 1);
                    self.replenish_state.next_retry_at =
                        Some(Instant::now() + std::time::Duration::from_secs(delay));
                    self.status_line = format!("Question maker retry in {delay}s…").into();
                }
            }
            (RunKind::QuestionMakerAction { question_id }, Err(message)) => {
                self.error_banner = Some(message.into());
                self.status_line = "Question maker action failed".into();
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
        let in_flight = self.question_maker_in_flight();
        let needed =
            question_maker_starts_needed(open_count, in_flight, &self.settings.question_maker);
        for _ in 0..needed {
            self.start_question_maker_replenishment(cx);
        }
    }

    fn start_question_maker_replenishment(&mut self, cx: &mut Context<Self>) {
        if self.question_maker_in_flight() >= 2 {
            return;
        }
        let queue_target = self
            .config
            .queue_target
            .unwrap_or(self.settings.question_maker.replenish_threshold);
        let paths = match TodPaths::discover() {
            Ok(p) => p,
            Err(err) => {
                self.error_banner = Some(format!("Paths error: {err}").into());
                cx.notify();
                return;
            }
        };
        let (prompt, cwd) = match (|| -> Result<(InterviewAgentPrompt, PathBuf), String> {
            let install = TodInstallPaths::discover().map_err(|e| e.to_string())?;
            let manifest = ProcessManifest::load(&install).map_err(|e| e.to_string())?;
            let prompt = question_maker_replenish_prompt(
                &self.fleet,
                &install,
                &manifest,
                &paths,
                self.config.node_id,
                &self.config.phase,
                &self.config.scratchpad,
                &self.config.config_path,
                queue_target,
                Some(&self.agent_config_id),
            )
            .map_err(|e| e.to_string())?;
            Ok((prompt, self.workspace_cwd.clone()))
        })() {
            Ok(v) => v,
            Err(message) => {
                self.error_banner = Some(message.into());
                cx.notify();
                return;
            }
        };
        let start_result = match self.agent.try_lock() {
            Ok(mut agent) => agent.start_question_maker_replenishment(
                &self.agent_config_id,
                cwd,
                prompt,
                &self.settings.question_maker,
            ),
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
                    .any(|kind| matches!(kind, RunKind::QuestionMakerReplenish));
                if first_replenish {
                    self.reset_replenish_hung_tracking();
                }
                self.runs.insert(handle.id, RunKind::QuestionMakerReplenish);
                if self.status_line.is_empty() || self.replenish_state.retry_count == 0 {
                    self.status_line = "Question maker replenishment in progress…".into();
                }
                self.error_banner = None;
            }
            Err(err) => {
                self.error_banner = Some(format!("Failed to start question maker: {err}").into());
            }
        }
        cx.notify();
    }

    fn manual_question_maker_kickoff(&mut self, cx: &mut Context<Self>) {
        self.replenish_state.manual_required = false;
        self.replenish_state.retry_count = 0;
        self.replenish_state.next_retry_at = None;
        self.replenish_state.exhausted = false;
        self.start_question_maker_replenishment(cx);
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
            self.status_line = "Queue cleared — requesting question maker refill…".into();
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
                self.status_line = "Queue cleared — requesting question maker refill…".into();
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
        if !self.questions.is_empty()
            || self.answer_in_flight()
            || self.question_maker_in_flight() > 0
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
            self.current_question_maker_status(),
            QuestionMakerStatusKind::Complete
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
                || self.feedback_focused(window, cx)
        });
        self.notes_editing = false;
        self.proposed_editing = false;
        self.feedback_editing = false;
        self.proposed_loaded_for = None;
        if let Some(window) = window {
            self.notes_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
            self.feedback_input.update(cx, |input, cx| {
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

        let record = AnswerRecord {
            id: question.id.clone(),
            option: mc.clone(),
            text_changed,
            body: payload_body.clone(),
        };
        let payload = match format_answer_payload(&[record]) {
            Ok(p) => p,
            Err(err) => {
                self.error_banner = Some(format!("Payload error: {err}").into());
                cx.notify();
                return;
            }
        };

        let transcript = transcript_path_for(&self.config);
        let (prompt, cwd) = match self.build_answer_processor_prompt(&payload) {
            Ok(v) => v,
            Err(message) => {
                self.error_banner = Some(message.into());
                cx.notify();
                return;
            }
        };

        let work = SubmitAnswerWork {
            question_id: question.id.clone(),
            question_path: question.path.clone(),
            question_body: question.display_body(),
            answer_text,
            mc,
            text_changed,
            payload_body,
            transcript,
            prompt,
            agent_config_id: self.agent_config_id.clone(),
            cwd,
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
        let record = ActionRecord {
            action: action.to_string(),
            id: question.id.clone(),
            body: notes.trim().to_string(),
        };
        let payload = match format_action_payload(&[record]) {
            Ok(p) => p,
            Err(err) => {
                self.error_banner = Some(format!("Payload error: {err}").into());
                cx.notify();
                return;
            }
        };
        let transcript = transcript_path_for(&self.config);
        let (prompt, cwd) = match self.build_question_maker_action_prompt(&payload) {
            Ok(v) => v,
            Err(message) => {
                self.error_banner = Some(message.into());
                cx.notify();
                return;
            }
        };
        let work = SubmitActionWork {
            question_id: question.id.clone(),
            question_path: question.path.clone(),
            action: action.to_string(),
            notes: notes.trim().to_string(),
            question_body: question.display_body(),
            transcript,
            prompt,
            agent_config_id: self.agent_config_id.clone(),
            cwd,
            question_maker_settings: self.settings.question_maker.clone(),
        };
        let agent = self.agent.clone();

        self.error_banner = None;
        self.status_line =
            format!("Question maker action {action} for {}", work.question_id).into();
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
                    RunKind::QuestionMakerAction {
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
                self.status_line = "Question maker action submit failed".into();
            }
        }
        cx.notify();
    }

    fn submit_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_mutate() {
            return;
        }
        let Some(question) = self.selected_question().cloned() else {
            return;
        };
        let feedback = self.feedback_input.read(cx).value().trim().to_string();
        if feedback.is_empty() {
            self.error_banner = Some("Enter feedback before submitting".into());
            cx.notify();
            return;
        }
        let raw_source = file_contents(&question.path).unwrap_or_default();
        let paths = match TodPaths::discover() {
            Ok(p) => p,
            Err(err) => {
                self.error_banner = Some(format!("Paths error: {err}").into());
                cx.notify();
                return;
            }
        };
        let (node_title, lifecycle_state) = self.interview_node_context();
        if let Err(err) = append_question_feedback(
            &paths,
            &question.id,
            &node_title,
            &lifecycle_state,
            &feedback,
            &raw_source,
        ) {
            self.error_banner = Some(format!("Feedback write failed: {err}").into());
            self.status_line = "Question feedback submit failed".into();
            cx.notify();
            return;
        }

        self.error_banner = None;
        self.status_line = format!("Feedback saved for {}", question.id).into();
        self.feedback_editing = false;
        self.feedback_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        cx.notify();
    }

    fn copy_question_source(&mut self, cx: &mut Context<Self>) {
        let (question_id, question_path) = match self.selected_question() {
            Some(q) => (q.id.clone(), q.path.clone()),
            None => {
                self.error_banner = Some("No question selected".into());
                cx.notify();
                return;
            }
        };
        let Some(raw) = file_contents(&question_path) else {
            self.error_banner =
                Some(format!("Could not read question file {}", question_path.display()).into());
            cx.notify();
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(raw));
        self.error_banner = None;
        self.status_line = format!("Copied raw source for {question_id}").into();
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
        mc + proposed + 5 // Notes, Other action, Submit, feedback field, Submit feedback
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

    fn feedback_stop_index(&self) -> usize {
        self.submit_stop_index() + 1
    }

    fn feedback_submit_stop_index(&self) -> usize {
        self.feedback_stop_index() + 1
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

    fn enter_feedback_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.notes_editing = false;
        self.proposed_editing = false;
        self.workspace_focus = WorkspaceFocus::Response(self.feedback_stop_index());
        self.feedback_editing = true;
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.feedback_input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    fn exit_feedback_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.feedback_editing {
            return;
        }
        self.feedback_editing = false;
        self.workspace_focus = WorkspaceFocus::Response(self.feedback_stop_index());
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
        let feedback_idx = self.feedback_stop_index();
        let feedback_submit_idx = self.feedback_submit_stop_index();
        if idx == notes_idx {
            self.enter_notes_edit(window, cx);
        } else if idx == submit_idx {
            self.submit_answer(window, cx);
        } else if idx == actions_idx && !self.actions_disabled() {
            self.open_actions_menu_from_keyboard(window, cx);
        } else if idx == feedback_idx {
            self.enter_feedback_edit(window, cx);
        } else if idx == feedback_submit_idx {
            self.submit_feedback(window, cx);
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
        if self.feedback_editing {
            self.exit_feedback_edit(window, cx);
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
        let workspace_cwd = self.workspace_cwd.clone();
        let agent_config_id = self.agent_config_id.clone();
        let deep_dive = cx.new(|cx| {
            DeepDiveView::new(
                question,
                config,
                session,
                agent_config_id,
                workspace_cwd,
                window,
                cx,
                agent,
            )
        });
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
        let entity_label: SharedString = self.session.node_id.to_string().into();
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

fn session_config_path(session: &InterviewSession) -> Option<PathBuf> {
    session.scratchpad_path.as_ref().and_then(|p| {
        let path = PathBuf::from(p).join("interview-config.md");
        path.exists().then_some(path)
    })
}

fn transcript_path_for(config: &InterviewConfig) -> PathBuf {
    config.scratchpad.join("transcript.md")
}

fn unbound_config(session: &InterviewSession, _paths: &TodPaths) -> InterviewConfig {
    InterviewConfig {
        session_id: session.session_id.clone().unwrap_or_default(),
        node_id: session.node_id,
        phase: session.phase.clone(),
        scratchpad: session
            .scratchpad_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_default(),
        // Sentinel — never watch repo-root queue (F5).
        queue: PathBuf::from("__unbound_queue__"),
        config_path: PathBuf::from("__unbound_config__"),
        queue_target: None,
        role_doc: None,
        scope: Vec::new(),
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

        let prompt = work.prompt;
        let snapshot = file_contents(&work.question_path);

        let mut provider = agent
            .lock()
            .map_err(|_| "Agent busy (bootstrap in progress) — try again shortly".to_string())?;
        let handle = provider
            .start_answer_processor(&work.agent_config_id, work.cwd, prompt, &work.settings)
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

        let prompt = work.prompt;
        let snapshot = file_contents(&work.question_path);

        let mut provider = agent
            .lock()
            .map_err(|_| "Agent busy (bootstrap in progress) — try again shortly".to_string())?;
        let handle = provider
            .start_question_maker_replenishment(
                &work.agent_config_id,
                work.cwd,
                prompt,
                &work.question_maker_settings,
            )
            .map_err(|err| format!("Failed to start question maker: {err}"))?;
        Ok((handle.id, snapshot))
    })();
    SubmitActionOutcome {
        question_id,
        action,
        result,
    }
}

fn question_maker_status_snapshot(path: Option<&Path>) -> QuestionMakerStatusSnapshot {
    let Some(path) = path else {
        return QuestionMakerStatusSnapshot::default();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return QuestionMakerStatusSnapshot::default();
    };
    question_maker_status_from_text(&text)
}

fn question_maker_status_kind(path: Option<&Path>) -> QuestionMakerStatusKind {
    question_maker_status_snapshot(path).kind
}

fn question_maker_status_from_text(text: &str) -> QuestionMakerStatusSnapshot {
    let mut snapshot = QuestionMakerStatusSnapshot::default();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("status:") {
            let value = rest.trim().to_ascii_lowercase();
            snapshot.kind = if value.contains("complete") {
                QuestionMakerStatusKind::Complete
            } else if value.contains("working") {
                QuestionMakerStatusKind::Working
            } else if value.contains("idle") {
                QuestionMakerStatusKind::Idle
            } else {
                QuestionMakerStatusKind::Unknown
            };
        } else if let Some(rest) = t.strip_prefix("notes:") {
            let notes = rest.trim();
            if !notes.is_empty() {
                snapshot.notes = Some(notes.to_string());
            }
        } else if let Some(rest) = t.strip_prefix("queue_depth:") {
            snapshot.queue_depth = rest.trim().parse().ok();
        } else if let Some(rest) = t.strip_prefix("queue_target:") {
            snapshot.queue_target = rest.trim().parse().ok();
        }
    }
    snapshot
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
                el.child(error_banner(msg, border, window, cx))
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
                                        window,
                                        self.is_complete(),
                                        if self.question_maker_waiting() {
                                            Some(self.build_question_maker_wait_ui())
                                        } else {
                                            None
                                        },
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
                                        &self.feedback_input,
                                        self.can_mutate(),
                                        self.can_edit_notes(),
                                        self.has_proposed_editor(),
                                        self.workspace_focus,
                                        self.proposed_editing,
                                        self.notes_editing,
                                        self.feedback_editing,
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
                window,
                &self.status_line,
                border,
                muted,
                self.replenish_state.manual_required,
                self.question_maker_waiting(),
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

fn error_banner(
    message: SharedString,
    border: gpui::Hsla,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    div()
        .px_4()
        .py_2()
        .bg(gpui::red())
        .border_b_1()
        .border_color(border)
        .child(
            selectable_text("workspace-error-banner", message, window, cx)
                .text_sm()
                .text_color(gpui::white()),
        )
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
    window: &mut Window,
    complete: bool,
    question_maker_wait: Option<QuestionMakerWaitUi>,
    question: Option<&QueueQuestion>,
    session: &InterviewSession,
    show_proceed: bool,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let body = if complete {
        complete_body(cx, session, show_proceed, foreground, muted).into_any_element()
    } else if let Some(q) = question {
        question_body_view(q, foreground, muted, window, cx).into_any_element()
    } else if let Some(wait) = question_maker_wait {
        question_maker_waiting_body(wait, foreground, muted).into_any_element()
    } else {
        div()
            .text_sm()
            .text_color(muted)
            .child("No open questions")
            .into_any_element()
    };
    let copy_disabled = question.is_none();
    v_flex()
        .size_full()
        .min_w_0()
        .overflow_hidden()
        .p_4()
        .child(
            div()
                .id("question-body-scroll")
                .flex_1()
                .min_h_0()
                .size_full()
                .min_w_0()
                .overflow_hidden()
                .overflow_y_scroll()
                .child(body),
        )
        .child(
            div().flex_none().pt_2().child(
                Button::new("copy-question-source")
                    .label("Copy raw source")
                    .compact()
                    .disabled(copy_disabled)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.copy_question_source(cx);
                    })),
            ),
        )
}

fn question_body_view(
    q: &QueueQuestion,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    window: &mut Window,
    cx: &mut App,
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
            selectable_text(
                SharedString::from(format!("question-context-{}", q.id)),
                SharedString::from(context.to_string()),
                window,
                cx,
            )
            .w_full()
            .min_w_0()
            .text_sm()
            .text_color(muted),
        );
    }

    if let Some(question) = q
        .question
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        col = col.child(
            selectable_text(
                SharedString::from(format!("question-text-{}", q.id)),
                SharedString::from(question.to_string()),
                window,
                cx,
            )
            .w_full()
            .min_w_0()
            .text_sm()
            .font_semibold()
            .text_color(foreground),
        );
    } else if !has_structured {
        let legacy = crate::interview::queue::strip_mc_option_lines(&q.body, &q.options);
        let (legacy, _) = crate::interview::queue::split_recommend_from_body(&legacy);
        if !legacy.trim().is_empty() {
            col = col.child(
                selectable_text(
                    SharedString::from(format!("question-legacy-body-{}", q.id)),
                    SharedString::from(legacy),
                    window,
                    cx,
                )
                .w_full()
                .min_w_0()
                .text_sm()
                .text_color(foreground),
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
            selectable_text(
                SharedString::from(format!("question-recommend-{}", q.id)),
                format!("Recommend: {recommend}"),
                window,
                cx,
            )
            .w_full()
            .min_w_0()
            .text_sm()
            .text_color(muted),
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
    feedback_input: &Entity<InputState>,
    can_mutate: bool,
    can_edit_notes: bool,
    show_proposed: bool,
    workspace_focus: WorkspaceFocus,
    proposed_editing: bool,
    notes_editing: bool,
    feedback_editing: bool,
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
    let feedback_input_disabled =
        !can_edit_notes || locked || question.is_none() || !feedback_editing;
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
    let mut scroll_body = v_flex()
        .id("response-scroll-body")
        .w_full()
        .min_w_0()
        .gap_2();
    let mut stop_idx = 0usize;
    if let Some(q) = question {
        for (idx, opt) in q.options.iter().enumerate() {
            let key = opt.key.clone();
            let focused = focused_idx == Some(stop_idx);
            scroll_body = scroll_body.child(mc_option_row(
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
    stop_idx += 1;
    let feedback_focused = focused_idx == Some(stop_idx);
    stop_idx += 1;
    let feedback_submit_focused = focused_idx == Some(stop_idx);
    let notes_view = cx.entity();
    let proposed_view = cx.entity();
    let feedback_view = cx.entity();

    let mut response_body = v_flex()
        .id("response-body")
        .w_full()
        .min_w_0()
        .flex_none()
        .flex_shrink_0()
        .gap_2();

    if show_proposed {
        response_body = response_body
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

    response_body = response_body
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

    let feedback_panel = v_flex()
        .id("response-feedback-panel")
        .w_full()
        .min_w_0()
        .flex_none()
        .flex_shrink_0()
        .gap_2()
        .pt_2()
        .border_t_1()
        .border_color(muted.opacity(0.25))
        .child(div().text_xs().text_color(muted).child("Question feedback"))
        .child(
            ListItem::new("feedback-field")
                .selected(feedback_focused)
                .w_full()
                .h(px(72.))
                .overflow_hidden()
                .on_click(move |_, window, app| {
                    feedback_view.update(app, |this, cx| {
                        if this.can_edit_notes() {
                            this.enter_feedback_edit(window, cx);
                        }
                    });
                })
                .child(
                    Input::new(feedback_input)
                        .disabled(feedback_input_disabled)
                        .w_full()
                        .h(px(72.)),
                ),
        )
        .child(
            ListItem::new("feedback-submit-focus")
                .selected(feedback_submit_focused)
                .child(
                    Button::new("submit-feedback")
                        .label("Submit feedback")
                        .compact()
                        .disabled(disabled)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                this.submit_feedback(window, cx);
                                cx.stop_propagation();
                            }),
                        ),
                ),
        );

    col.child(
        div()
            .id("response-scroll")
            .flex_1()
            .min_h_0()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .overflow_y_scroll()
            .child(scroll_body.child(response_body)),
    )
    .child(feedback_panel)
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
/// Uses `deferred` so the menu paints above later siblings (e.g. the feedback
/// panel anchored at the bottom of this column). Keyboard focus stays on the
/// eager `PopupMenu` entity — not `Button::dropdown_menu`'s deferred Popover.
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
                    deferred(
                        anchored()
                            .anchor(Corner::TopLeft)
                            .snap_to_window_with_margin(px(8.))
                            .child(div().occlude().mt_1().child(menu)),
                    )
                    .with_priority(1),
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
    window: &mut Window,
    status: &SharedString,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    show_manual_kickoff: bool,
    show_activity_indicator: bool,
) -> impl IntoElement {
    let status_text = if status.is_empty() {
        SharedString::from("Ready")
    } else {
        status.clone()
    };
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
            h_flex()
                .min_w_0()
                .flex_1()
                .gap_2()
                .items_center()
                .when(show_activity_indicator, |el| {
                    el.child(div().flex_shrink_0().text_xs().text_color(muted).child("●"))
                })
                .child(
                    div().min_w_0().flex_1().overflow_hidden().child(
                        selectable_text("workspace-status", status_text, window, cx)
                            .text_xs()
                            .text_color(muted)
                            .text_ellipsis(),
                    ),
                ),
        )
        .when(show_manual_kickoff, |el| {
            el.child(
                Button::new("manual-question-maker-kickoff")
                    .label("Kickoff question maker")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.manual_question_maker_kickoff(cx);
                    })),
            )
        })
}

fn question_maker_waiting_body(
    wait: QuestionMakerWaitUi,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let dots = ".".repeat(wait.animate_dots.max(1));
    let mut col = v_flex().w_full().min_w_0().gap_3();

    col = col.child(
        div()
            .text_sm()
            .font_semibold()
            .text_color(foreground)
            .child(format!("{}{dots}", wait.headline)),
    );

    if let Some(detail) = wait.detail {
        col = col.child(div().text_sm().text_color(muted).child(detail));
    }

    if wait.elapsed_secs >= 3 {
        col = col.child(
            div()
                .text_xs()
                .text_color(muted)
                .child(format!("Elapsed {}", format_elapsed(wait.elapsed_secs))),
        );
    }

    if let (Some(depth), Some(target)) = (wait.queue_depth, wait.queue_target) {
        if target > 0 {
            let pct = (depth as f32 / target as f32).clamp(0., 1.);
            col = col.child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("Queue: {depth} / {target}")),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(4.))
                            .rounded(px(2.))
                            .bg(muted.opacity(0.2))
                            .child(
                                div()
                                    .h_full()
                                    .rounded(px(2.))
                                    .bg(muted.opacity(0.55))
                                    .w(gpui::relative(pct)),
                            ),
                    ),
            );
        }
    }

    col.child(
        div()
            .text_xs()
            .text_color(muted)
            .child("This can take a minute or two on first setup. The status bar below updates as work progresses."),
    )
}

fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

fn count_queue_files(queue_dir: &Path) -> usize {
    if !queue_dir.is_dir() {
        return 0;
    }
    std::fs::read_dir(queue_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
                .count()
        })
        .unwrap_or(0)
}

impl gpui::EventEmitter<WorkspaceEvent> for WorkspaceView {}

#[cfg(test)]
mod tests {
    use super::{
        QuestionMakerStatusKind, RunKind, WorkspaceInFlightState, build_proposed_answer_parts,
        question_maker_status_from_text, reopen_complete_with_bound_queue,
        update_replenish_idle_since,
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
                QuestionMakerStatusKind::Working,
                QuestionMakerStatusKind::Idle,
                None,
            )
            .is_some()
        );
    }

    #[test]
    fn idle_grace_not_started_for_stale_idle_at_replenish_start() {
        assert!(
            update_replenish_idle_since(
                QuestionMakerStatusKind::Idle,
                QuestionMakerStatusKind::Idle,
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn idle_grace_cleared_when_agent_working() {
        assert!(
            update_replenish_idle_since(
                QuestionMakerStatusKind::Idle,
                QuestionMakerStatusKind::Working,
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
                QuestionMakerStatusKind::Idle,
                QuestionMakerStatusKind::Complete,
                Some(since),
            ),
            Some(since),
        );
    }

    #[test]
    fn parses_question_maker_status_kinds() {
        assert_eq!(
            question_maker_status_from_text("status: complete\n").kind,
            QuestionMakerStatusKind::Complete
        );
        assert_eq!(
            question_maker_status_from_text("status: idle\n").kind,
            QuestionMakerStatusKind::Idle
        );
        assert_eq!(
            question_maker_status_from_text("status: working\n").kind,
            QuestionMakerStatusKind::Working
        );
        assert_eq!(
            question_maker_status_from_text("notes: only\n").kind,
            QuestionMakerStatusKind::Unknown
        );
    }

    #[test]
    fn parses_question_maker_status_notes_and_queue() {
        let snap = question_maker_status_from_text(
            "status: working\nqueue_depth: 3\nqueue_target: 8\nnotes: Drafting questions\n",
        );
        assert_eq!(snap.kind, QuestionMakerStatusKind::Working);
        assert_eq!(snap.queue_depth, Some(3));
        assert_eq!(snap.queue_target, Some(8));
        assert_eq!(snap.notes.as_deref(), Some("Drafting questions"));
    }

    #[test]
    fn reopen_complete_with_nonempty_queue_flips_active() {
        use crate::fleet::FleetStore;
        use crate::interview::db::{NewInterviewSession, SessionStore};
        use crate::outline::OutlineMutation;
        use std::sync::Arc;

        let dir = std::env::temp_dir().join(format!("tod-reopen-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let fleet = Arc::new(FleetStore::open(&dir).unwrap());
        fleet
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "t".into(),
                title: "T".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: None,
                position: crate::outline::CreatePosition::Below,
                title: "Node".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        let node_id = fleet.flatten_outline(list_id).unwrap()[0].node.id;

        let store = SessionStore::open(fleet.clone());
        let mut session = store
            .insert_session_with_metadata(
                NewInterviewSession {
                    node_id,
                    agent_config_id: None,
                    display_name: "Complete with queue".into(),
                    phase: "design-interview".into(),
                },
                InterviewSessionStatus::Complete,
                None,
            )
            .unwrap();
        assert_eq!(session.status, InterviewSessionStatus::Complete);

        assert!(reopen_complete_with_bound_queue(&mut session, &store, true));
        assert_eq!(session.status, InterviewSessionStatus::Active);
        let reloaded = store.get_session(session.id).unwrap().unwrap();
        assert_eq!(reloaded.status, InterviewSessionStatus::Active);

        let mut done = store
            .insert_session_with_metadata(
                NewInterviewSession {
                    node_id,
                    agent_config_id: None,
                    display_name: "Truly complete".into(),
                    phase: "design-interview".into(),
                },
                InterviewSessionStatus::Complete,
                None,
            )
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
        runs.insert(RunId::new(), RunKind::QuestionMakerReplenish);

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
                .any(|k| matches!(k, RunKind::QuestionMakerReplenish))
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
