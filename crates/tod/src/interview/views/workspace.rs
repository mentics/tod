use crate::interview::agent::{AgentRunState, RunId, SharedAgent};
use crate::interview::config::{
    InterviewConfig, parse_interview_config, sync_scaffolding_from_disk,
};
use crate::interview::kickoff::{
    answer_processor_prompt, researcher_action_prompt, researcher_replenish_prompt,
};
use crate::interview::queue::{QueueQuestion, load_queue_dir};
use crate::interview::queue_watcher::QueueWatcher;
use crate::interview::replenishment::{researcher_starts_needed, retry_backoff_secs};
use crate::interview::transcript::{
    ActionRecord, AnswerRecord, append_action, append_answer, format_action_payload,
    format_answer_payload,
};
use crate::interview::views::deep_dive::{DeepDiveEvent, DeepDiveView};
use crate::interview::{
    InterviewSession, InterviewSessionStatus, SessionStore, TodPaths, TodSettings,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, MouseButton, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, Timer, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants, DropdownButton};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, StyledExt, h_flex, v_flex};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const QUEUE_POLL_INTERVAL: Duration = Duration::from_millis(250);

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
        BackToSessions,
        FocusNotes,
    ]
);

const WORKSPACE_CONTEXT: &str = "InterviewWorkspace";
const MAX_RESEARCHER_RETRIES: u32 = 3;
const LIST_COLUMN_WIDTH: f32 = 160.;
/// Middle reading pane. Explicit width; response column flexes for remaining width.
const BODY_COLUMN_WIDTH: f32 = 250.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceFocus {
    QuestionList,
    /// Index into response interactive controls (MC options, then Notes, actions, Submit).
    Response(usize),
}

#[derive(Debug, Clone)]
enum RunKind {
    AnswerProcessor { question_id: String },
    ResearcherReplenish,
    ResearcherAction { question_id: String },
}

#[derive(Debug, Default)]
struct ReplenishState {
    retry_count: u32,
    next_retry_at: Option<Instant>,
    manual_required: bool,
    /// After a successful replenishment that left the queue empty, treat as exhausted
    /// even if `researcher-status` still says `idle` (agent should set `complete`).
    exhausted: bool,
    /// When the current replenishment run(s) were started — used to clear hung ACP.
    started_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResearcherStatusKind {
    Idle,
    Working,
    Complete,
    Unknown,
}

const HUNG_REPLENISH_SECS: u64 = 45;

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
    BackToSessions,
    SessionComplete,
}

pub struct WorkspaceView {
    session: InterviewSession,
    config: InterviewConfig,
    settings: TodSettings,
    store: SessionStore,
    questions: Vec<QueueQuestion>,
    pending: HashSet<String>,
    pending_snapshots: HashMap<String, String>,
    selected_question_id: Option<String>,
    selected_mc: Option<String>,
    notes_input: Entity<InputState>,
    /// None until session `config_path` / queue is bound — never falls back to repo-root queue.
    queue_watcher: Option<QueueWatcher>,
    agent: SharedAgent,
    runs: HashMap<RunId, RunKind>,
    replenish_state: ReplenishState,
    status_line: SharedString,
    error_banner: Option<SharedString>,
    mutations_blocked: bool,
    deep_dive: Option<Entity<DeepDiveView>>,
    _deep_dive_subscription: Option<Subscription>,
    pending_notes_paste: Option<String>,
    focus_handle: FocusHandle,
    question_list_scroll_handle: ScrollHandle,
    workspace_focus: WorkspaceFocus,
    notes_editing: bool,
    scaffolding_pending: bool,
    _poll_task: Task<()>,
}

impl WorkspaceView {
    pub fn new(
        session: InterviewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
        agent: SharedAgent,
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
                    Err(_) => (
                        unbound_config(&session, &paths),
                        None,
                        Vec::new(),
                        true,
                    ),
                }
            } else {
                (
                    unbound_config(&session, &paths),
                    None,
                    Vec::new(),
                    true,
                )
            };

        let selected_question_id = questions.first().map(|q| q.id.clone());
        let notes_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(3)
                .placeholder("Notes (Enter to edit; Ctrl+Enter to submit)")
        });
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

        Self {
            session,
            config,
            settings,
            store,
            questions,
            pending: HashSet::new(),
            pending_snapshots: HashMap::new(),
            selected_question_id,
            selected_mc: None,
            notes_input,
            queue_watcher,
            agent,
            runs: HashMap::new(),
            replenish_state: ReplenishState::default(),
            status_line: if scaffolding_pending {
                "Waiting for researcher scaffolding…".into()
            } else {
                SharedString::default()
            },
            error_banner: None,
            mutations_blocked,
            deep_dive: None,
            _deep_dive_subscription: None,
            pending_notes_paste: None,
            focus_handle: cx.focus_handle().tab_stop(true),
            question_list_scroll_handle: ScrollHandle::new(),
            workspace_focus: WorkspaceFocus::QuestionList,
            notes_editing: false,
            scaffolding_pending,
            _poll_task: poll_task,
        }
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

    fn can_replenish(&self) -> bool {
        self.session.status == InterviewSessionStatus::Active
            && !self.scaffolding_pending
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
    fn try_bind_bootstrap_scaffolding(&mut self) -> bool {
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
        if self.session.status == InterviewSessionStatus::Complete && !self.questions.is_empty() {
            if reopen_complete_with_bound_queue(&mut self.session, &self.store, true) {
                self.mutations_blocked = false;
            }
        }
        self.status_line = "Scaffolding bound from researcher bootstrap".into();
        true
    }

    fn poll_runs_and_queue(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;

        if self.try_bind_bootstrap_scaffolding() {
            changed = true;
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
                self.apply_queue_update(questions);
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

    /// If ACP stays InFlight but researcher-status is idle/complete for long enough,
    /// cancel the hung replenish runs as failure (do not mark success / exhausted).
    fn reconcile_hung_replenishment(&mut self, cx: &mut Context<Self>) -> bool {
        if self.researcher_in_flight() == 0 {
            return false;
        }
        let Some(started) = self.replenish_state.started_at else {
            return false;
        };
        if started.elapsed() < Duration::from_secs(HUNG_REPLENISH_SECS) {
            return false;
        }
        let kind = researcher_status_kind(self.config.researcher_status.as_deref());
        if !matches!(
            kind,
            ResearcherStatusKind::Idle | ResearcherStatusKind::Complete
        ) {
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
            self.replenish_state.started_at = None;
        }
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
                        self.replenish_state.started_at = None;
                        if self.questions.is_empty() {
                            self.replenish_state.exhausted = true;
                            self.status_line = "Researcher returned no further questions".into();
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
                self.replenish_state.started_at = None;
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
        let open_count = self.questions.len();
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
        match self.agent.try_lock() {
            Ok(mut agent) => match agent.start_researcher_replenishment(cwd, prompt) {
                Ok(handle) => {
                    self.runs.insert(handle.id, RunKind::ResearcherReplenish);
                    if self.replenish_state.started_at.is_none() {
                        self.replenish_state.started_at = Some(Instant::now());
                    }
                    if self.status_line.is_empty() || self.replenish_state.retry_count == 0 {
                        self.status_line = "Researcher replenishment in progress…".into();
                    }
                    self.error_banner = None;
                }
                Err(err) => {
                    self.error_banner = Some(format!("Failed to start researcher: {err}").into());
                }
            },
            Err(_) => {
                self.status_line = "Waiting for agent (bootstrap in progress)…".into();
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

    fn apply_queue_update(&mut self, questions: Vec<QueueQuestion>) {
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
        if !questions.is_empty() {
            self.replenish_state.exhausted = false;
            if reopen_complete_with_bound_queue(&mut self.session, &self.store, true) {
                self.mutations_blocked = false;
            }
        }
        self.questions = questions;
        if self
            .selected_question_id
            .as_ref()
            .is_none_or(|id| !self.questions.iter().any(|q| &q.id == id))
        {
            self.selected_question_id = self
                .questions
                .iter()
                .find(|q| !self.pending.contains(&q.id))
                .map(|q| q.id.clone());
            self.reset_response_fields();
            if self.selected_question_id.is_none() {
                self.clear_validation_banner();
            }
        }
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

    fn select_question(&mut self, id: &str, cx: &mut Context<Self>) {
        if self
            .selected_question_id
            .as_ref()
            .is_some_and(|current| current == id)
        {
            return;
        }
        self.selected_question_id = Some(id.to_string());
        self.reset_response_fields();
        self.clear_validation_banner();
        if let Some(idx) = self.questions.iter().position(|q| q.id == id) {
            self.question_list_scroll_handle.scroll_to_item(idx);
        }
        cx.notify();
    }

    fn move_question_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.questions.is_empty() {
            return;
        }
        let current = self
            .selected_question_id
            .as_ref()
            .and_then(|id| self.questions.iter().position(|q| &q.id == id))
            .unwrap_or(0);
        let new_idx = if delta < 0 {
            current.saturating_sub((-delta) as usize)
        } else {
            (current + delta as usize).min(self.questions.len() - 1)
        };
        if new_idx == current {
            return;
        }
        self.selected_question_id = Some(self.questions[new_idx].id.clone());
        self.reset_response_fields();
        self.clear_validation_banner();
        self.question_list_scroll_handle.scroll_to_item(new_idx);
        cx.notify();
    }

    fn reset_response_fields(&mut self) {
        self.selected_mc = None;
        self.notes_editing = false;
        if matches!(self.workspace_focus, WorkspaceFocus::Response(_)) {
            self.workspace_focus = WorkspaceFocus::Response(0);
        }
    }

    fn is_question_pending(&self, id: &str) -> bool {
        self.pending.contains(id)
    }

    fn can_mutate(&self) -> bool {
        !self.mutations_blocked && !self.answer_in_flight()
    }

    fn can_edit_notes(&self) -> bool {
        !self.mutations_blocked
    }

    fn submit_answer(&mut self, cx: &mut Context<Self>) {
        if !self.can_mutate() {
            return;
        }
        let Some(question) = self.selected_question().cloned() else {
            self.clear_validation_banner();
            cx.notify();
            return;
        };
        if self.is_question_pending(&question.id) {
            return;
        }
        let notes = self.notes_input.read(cx).value().to_string();
        let mc = self.selected_mc.clone();
        if notes.trim().is_empty() && mc.is_none() {
            self.error_banner = Some("Enter notes and/or select an MC option".into());
            cx.notify();
            return;
        }

        let transcript = self.config.transcript.clone();
        if let Err(err) = append_answer(
            &transcript,
            &question.id,
            &question.body,
            notes.trim(),
            mc.as_deref(),
        ) {
            self.error_banner = Some(format!("Transcript write failed: {err}").into());
            cx.notify();
            return;
        }

        let record = AnswerRecord {
            id: question.id.clone(),
            option: mc.clone(),
            body: notes.trim().to_string(),
        };
        let payload = match format_answer_payload(&[record]) {
            Ok(p) => p,
            Err(err) => {
                self.error_banner = Some(format!("Payload error: {err}").into());
                cx.notify();
                return;
            }
        };
        let prompt = answer_processor_prompt(&self.config.config_path, &payload);
        let cwd = self.config.entity.clone();
        let agent = self.agent.clone();
        match agent.try_lock() {
            Ok(mut provider) => match provider.start_answer_processor(cwd, prompt) {
                Ok(handle) => {
                    self.runs.insert(
                        handle.id,
                        RunKind::AnswerProcessor {
                            question_id: question.id.clone(),
                        },
                    );
                    self.status_line = format!("Processing answer for {}", question.id).into();
                    self.error_banner = None;
                    if let Some(contents) = file_contents(&question.path) {
                        self.pending_snapshots.insert(question.id.clone(), contents);
                    }
                    self.pending.insert(question.id.clone());
                    self.select_next_question();
                }
                Err(err) => {
                    self.error_banner =
                        Some(format!("Failed to start answer processor: {err}").into());
                }
            },
            Err(_) => {
                self.error_banner =
                    Some("Agent busy (bootstrap in progress) — try again shortly".into());
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
        if self.is_question_pending(&question.id) {
            return;
        }
        let notes = self.notes_input.read(cx).value().to_string();
        let transcript = self.config.transcript.clone();
        if let Err(err) = append_action(
            &transcript,
            &question.id,
            action,
            Some(notes.trim()),
            Some(&question.body),
        ) {
            self.error_banner = Some(format!("Transcript write failed: {err}").into());
            cx.notify();
            return;
        }
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
        let prompt = researcher_action_prompt(&self.config.config_path, &payload);
        let cwd = self.config.entity.clone();
        let agent = self.agent.clone();
        match agent.try_lock() {
            Ok(mut provider) => match provider.start_researcher_replenishment(cwd, prompt) {
                Ok(handle) => {
                    self.runs.insert(
                        handle.id,
                        RunKind::ResearcherAction {
                            question_id: question.id.clone(),
                        },
                    );
                    self.status_line =
                        format!("Researcher action {action} for {}", question.id).into();
                    self.error_banner = None;
                    if let Some(contents) = file_contents(&question.path) {
                        self.pending_snapshots.insert(question.id.clone(), contents);
                    }
                    self.pending.insert(question.id.clone());
                    self.select_next_question();
                }
                Err(err) => {
                    self.error_banner = Some(format!("Failed to start researcher: {err}").into());
                }
            },
            Err(_) => {
                self.error_banner =
                    Some("Agent busy (bootstrap in progress) — try again shortly".into());
            }
        }
        cx.notify();
    }

    fn select_next_question(&mut self) {
        let current_idx = self
            .selected_question_id
            .as_ref()
            .and_then(|id| self.questions.iter().position(|q| &q.id == id));
        let start = current_idx.map(|i| i + 1).unwrap_or(0);
        for offset in 0..self.questions.len() {
            let idx = (start + offset) % self.questions.len();
            let q = &self.questions[idx];
            if !self.pending.contains(&q.id) {
                self.selected_question_id = Some(q.id.clone());
                self.reset_response_fields();
                self.clear_validation_banner();
                self.question_list_scroll_handle.scroll_to_item(idx);
                return;
            }
        }
        // No non-pending question left — clear selection so we don't keep a stale banner.
        self.selected_question_id = None;
        self.reset_response_fields();
        self.clear_validation_banner();
    }

    fn on_digit_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Only Notes *edit mode* suppresses digit MC submit (req 21.6) — not mere focus.
        if self.notes_editing {
            cx.propagate();
            return;
        }
        let _ = window;
        self.submit_mc_option(key, cx);
    }

    fn submit_mc_option(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.notes_editing {
            return;
        }
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
        self.submit_answer(cx);
    }

    fn response_stop_count(&self) -> usize {
        let mc = self
            .selected_question()
            .map(|q| q.options.len())
            .unwrap_or(0);
        mc + 3 // Notes, Other action, Submit
    }

    fn notes_stop_index(&self) -> usize {
        self.selected_question()
            .map(|q| q.options.len())
            .unwrap_or(0)
    }

    fn enter_notes_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit_notes() {
            return;
        }
        if self
            .selected_question_id
            .as_ref()
            .is_some_and(|id| self.is_question_pending(id))
        {
            return;
        }
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

    fn focus_response_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.notes_editing {
            return;
        }
        self.workspace_focus = WorkspaceFocus::Response(0);
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn focus_list_left(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.notes_editing {
            return;
        }
        self.workspace_focus = WorkspaceFocus::QuestionList;
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn move_response_focus(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.notes_editing {
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
        if self.notes_editing {
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
                self.submit_mc_option(&key, cx);
            }
            return;
        }
        let notes_idx = mc_count;
        let actions_idx = mc_count + 1;
        let submit_idx = mc_count + 2;
        if idx == notes_idx {
            self.enter_notes_edit(window, cx);
        } else if idx == submit_idx {
            self.submit_answer(cx);
        } else if idx == actions_idx {
            // Dropdown opens via pointer; keyboard focus chrome only.
        }
    }

    fn focus_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.enter_notes_edit(window, cx);
    }

    fn handle_workspace_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.notes_editing {
            self.exit_notes_edit(window, cx);
        }
        // Otherwise no-op — never navigate to session list (H5 / req 22).
    }

    fn open_deep_dive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(question) = self.selected_question().cloned() else {
            return;
        };
        if self.is_question_pending(&question.id) {
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

    fn back_to_sessions(&mut self, cx: &mut Context<Self>) {
        cx.emit(WorkspaceEvent::BackToSessions);
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
        KeyBinding::new("alt-left", BackToSessions, context),
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

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let accent = cx.theme().accent;
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
            .on_action(cx.listener(|this, _: &SubmitAnswer, _, cx| {
                this.submit_answer(cx);
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
                if this.notes_editing {
                    cx.propagate();
                    return;
                }
                match this.workspace_focus {
                    WorkspaceFocus::QuestionList => {
                        this.move_question_selection(-1, cx);
                        cx.stop_propagation();
                    }
                    WorkspaceFocus::Response(_) => {
                        this.move_response_focus(-1, window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &QuestionMoveDown, window, cx| {
                if this.notes_editing {
                    cx.propagate();
                    return;
                }
                match this.workspace_focus {
                    WorkspaceFocus::QuestionList => {
                        this.move_question_selection(1, cx);
                        cx.stop_propagation();
                    }
                    WorkspaceFocus::Response(_) => {
                        this.move_response_focus(1, window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &FocusRight, window, cx| {
                if this.notes_editing {
                    cx.propagate();
                    return;
                }
                if this.workspace_focus == WorkspaceFocus::QuestionList {
                    this.focus_response_right(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &FocusLeft, window, cx| {
                if this.notes_editing {
                    cx.propagate();
                    return;
                }
                if matches!(this.workspace_focus, WorkspaceFocus::Response(_)) {
                    this.focus_list_left(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &ActivateFocused, window, cx| {
                if this.notes_editing {
                    cx.propagate();
                    return;
                }
                this.activate_focused(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &WorkspaceEscape, window, cx| {
                this.handle_workspace_escape(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &BackToSessions, _, cx| {
                this.back_to_sessions(cx);
            }))
            .child(workspace_header(cx, &self.session, border, muted))
            .when(archived, |el| el.child(archived_banner(border, muted)))
            .when_some(self.error_banner.clone(), |el, msg| {
                el.child(error_banner(msg, border))
            })
            .child(
                div()
                    .id("workspace-columns")
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(question_list_column(
                        cx,
                        &self.questions,
                        &self.selected_question_id,
                        &self.pending,
                        &self.question_list_scroll_handle,
                        self.workspace_focus == WorkspaceFocus::QuestionList,
                        accent,
                        foreground,
                        muted,
                        border,
                        background,
                    ))
                    .child(body_column(
                        cx,
                        self.is_complete(),
                        self.researcher_in_flight() > 0 || self.scaffolding_pending,
                        self.selected_question(),
                        &self.session,
                        foreground,
                        muted,
                        border,
                    ))
                    .child(response_column(
                        cx,
                        window,
                        self.selected_question(),
                        self.is_question_pending(
                            self.selected_question_id.as_deref().unwrap_or(""),
                        ),
                        &self.selected_mc,
                        &self.notes_input,
                        self.can_mutate(),
                        self.can_edit_notes(),
                        self.workspace_focus,
                        self.notes_editing,
                        accent,
                        foreground,
                        muted,
                        border,
                        background,
                    )),
            )
            .child(status_footer(
                cx,
                &self.status_line,
                border,
                muted,
                self.replenish_state.manual_required,
            ))
    }
}

fn workspace_header(
    cx: &mut Context<WorkspaceView>,
    session: &InterviewSession,
    border: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let entity_label: SharedString = session
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
        .child(
            Button::new("back-to-sessions")
                .label("Back to interviews")
                .flex_shrink_0()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.back_to_sessions(cx);
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.back_to_sessions(cx);
                        cx.stop_propagation();
                    }),
                ),
        )
        .child(
            v_flex()
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(session.display_name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(entity_label),
                ),
        )
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
    cx: &mut Context<WorkspaceView>,
    questions: &[QueueQuestion],
    selected_id: &Option<String>,
    pending: &HashSet<String>,
    scroll_handle: &ScrollHandle,
    list_focused: bool,
    accent: gpui::Hsla,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
    background: gpui::Hsla,
) -> impl IntoElement {
    let mut scroll_content = div()
        .id("question-list-scroll")
        .flex()
        .flex_col()
        .size_full()
        .overflow_y_scroll()
        .track_scroll(scroll_handle);
    for (idx, question) in questions.iter().enumerate() {
        let id = question.id.clone();
        let is_selected = selected_id.as_ref() == Some(&id);
        let is_pending = pending.contains(&id);
        let label: SharedString = format!("{} · {}", question.id, question.short_label).into();
        scroll_content = scroll_content.child(
            div()
                .id(("question-row", idx))
                .px_3()
                .py_2()
                .cursor_pointer()
                .border_l_4()
                .border_color(if is_selected {
                    accent
                } else {
                    gpui::transparent_black()
                })
                .when(is_selected, |el| {
                    el.bg(accent).when(list_focused, |el| el.border_color(foreground))
                })
                .when(is_pending, |el| el.opacity(0.55))
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| {
                        this.workspace_focus = WorkspaceFocus::QuestionList;
                        this.notes_editing = false;
                        this.select_question(&id, cx);
                        this.focus_handle.focus(window);
                    }
                }))
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(if is_selected {
                            background
                        } else if is_pending {
                            muted
                        } else {
                            foreground
                        })
                        .child(label),
                ),
        );
    }

    v_flex()
        .w(px(LIST_COLUMN_WIDTH))
        .min_w(px(LIST_COLUMN_WIDTH))
        .max_w(px(LIST_COLUMN_WIDTH))
        .flex_none()
        .h_full()
        .overflow_hidden()
        .border_r_1()
        .border_color(border)
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
                .relative()
                .flex_1()
                .min_h_0()
                .child(scroll_content)
                .vertical_scrollbar(scroll_handle),
        )
}

fn body_column(
    cx: &mut Context<WorkspaceView>,
    complete: bool,
    researcher_waiting: bool,
    question: Option<&QueueQuestion>,
    session: &InterviewSession,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
) -> impl IntoElement {
    let body = if complete {
        complete_body(cx, session, foreground, muted).into_any_element()
    } else if let Some(q) = question {
        div()
            .w_full()
            .min_w_0()
            .text_sm()
            .text_color(foreground)
            .child(q.body.clone())
            .into_any_element()
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
        .flex_none()
        .w(px(BODY_COLUMN_WIDTH))
        .min_w(px(BODY_COLUMN_WIDTH))
        .max_w(px(BODY_COLUMN_WIDTH))
        .h_full()
        .overflow_hidden()
        .border_r_1()
        .border_color(border)
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

fn complete_body(
    cx: &mut Context<WorkspaceView>,
    session: &InterviewSession,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
    v_flex()
        .gap_3()
        .child(
            div()
                .text_lg()
                .font_semibold()
                .text_color(foreground)
                .child("Complete"),
        )
        .child(div().text_sm().text_color(muted).child(format!(
            "Interview \"{}\" has no remaining open questions.",
            session.display_name
        )))
        .child(
            Button::new("complete-back")
                .label("Back to interviews")
                .primary()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.back_to_sessions(cx);
                        cx.stop_propagation();
                    }),
                ),
        )
}

fn response_column(
    cx: &mut Context<WorkspaceView>,
    _window: &mut Window,
    question: Option<&QueueQuestion>,
    pending: bool,
    selected_mc: &Option<String>,
    notes_input: &Entity<InputState>,
    can_mutate: bool,
    can_edit_notes: bool,
    workspace_focus: WorkspaceFocus,
    notes_editing: bool,
    accent: gpui::Hsla,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
    background: gpui::Hsla,
) -> impl IntoElement {
    let disabled = !can_mutate || pending || question.is_none();
    let notes_input_disabled = !can_edit_notes || pending || question.is_none() || !notes_editing;
    let focused_idx = match workspace_focus {
        WorkspaceFocus::Response(i) => Some(i),
        WorkspaceFocus::QuestionList => None,
    };
    let mut col = v_flex()
        .id("response-column")
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_hidden()
        .border_l_1()
        .border_color(border)
        .p_3()
        .gap_2()
        .child(div().text_xs().text_color(muted).child(if pending {
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
                accent,
                foreground,
                muted,
                background,
                disabled,
            ));
            stop_idx += 1;
        }
    }
    let notes_focused = focused_idx == Some(stop_idx);
    stop_idx += 1;
    let actions_focused = focused_idx == Some(stop_idx);
    stop_idx += 1;
    let submit_focused = focused_idx == Some(stop_idx);

    col.child(
        v_flex()
            .id("response-footer")
            .w_full()
            .min_w_0()
            .flex_none()
            .flex_shrink_0()
            .gap_2()
            .child(
                div()
                    .id("notes-field")
                    .w_full()
                    .min_w_0()
                    .h(px(80.))
                    .overflow_hidden()
                    .rounded_sm()
                    .border_l_4()
                    .border_color(if notes_focused {
                        accent
                    } else {
                        gpui::transparent_black()
                    })
                    .when(notes_focused && !notes_editing, |el| el.bg(accent))
                    .when(notes_editing, |el| el.bg(accent.opacity(0.18)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            if this.can_edit_notes() {
                                this.enter_notes_edit(window, cx);
                            }
                        }),
                    )
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
                        div()
                            .rounded_sm()
                            .when(actions_focused, |el| {
                                el.bg(accent).border_l_4().border_color(foreground)
                            })
                            .child(action_dropdown(cx, disabled)),
                    )
                    .child(
                        div()
                            .rounded_sm()
                            .when(submit_focused, |el| {
                                el.bg(accent).border_l_4().border_color(foreground)
                            })
                            .child(
                                Button::new("submit-answer")
                                    .label("Submit")
                                    .primary()
                                    .compact()
                                    .disabled(disabled)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.submit_answer(cx);
                                            cx.stop_propagation();
                                        }),
                                    ),
                            ),
                    ),
            ),
    )
}

fn mc_option_row(
    cx: &mut Context<WorkspaceView>,
    idx: usize,
    opt: crate::interview::queue::McOption,
    selected: bool,
    focused: bool,
    accent: gpui::Hsla,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    background: gpui::Hsla,
    disabled: bool,
) -> impl IntoElement {
    let key = opt.key.clone();
    div()
        .id(("mc-option", idx))
        .w_full()
        .min_w_0()
        .cursor_pointer()
        .rounded_sm()
        .border_l_4()
        .border_color(if focused || selected {
            accent
        } else {
            gpui::transparent_black()
        })
        .when(focused, |el| el.bg(accent))
        .when(selected && !focused, |el| el.bg(accent.opacity(0.35)))
        .when(disabled, |el| el.opacity(0.5))
        .on_click(cx.listener({
            let key = key.clone();
            move |this, _, _, cx| {
                if !disabled {
                    this.submit_mc_option(&key, cx);
                }
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener({
                let key = key.clone();
                move |this, _, _, cx| {
                    if !disabled {
                        this.submit_mc_option(&key, cx);
                        cx.stop_propagation();
                    }
                }
            }),
        )
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .px_2()
                .py_1()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_sm()
                        .font_semibold()
                        .text_color(if focused {
                            background
                        } else if selected {
                            accent
                        } else {
                            muted
                        })
                        .child(format!("{}.", opt.key)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_sm()
                        .whitespace_normal()
                        .text_color(if focused {
                            background
                        } else if selected {
                            foreground
                        } else {
                            muted
                        })
                        .child(opt.label),
                ),
        )
}

fn action_dropdown(cx: &mut Context<WorkspaceView>, disabled: bool) -> impl IntoElement {
    let view = cx.entity().downgrade();
    DropdownButton::new("question-actions")
        .disabled(disabled)
        .compact()
        .button(
            Button::new("actions-trigger")
                .label("Other action")
                .compact(),
        )
        .dropdown_menu(move |menu, _window, _cx| {
            let view = view.clone();
            menu.item(PopupMenuItem::new("Consider / Reconsider").on_click({
                let view = view.clone();
                move |_, window, cx| {
                    if let Some(entity) = view.upgrade() {
                        entity.update(cx, |this, cx| {
                            this.submit_action("reconsider", window, cx);
                        });
                    }
                }
            }))
            .item(PopupMenuItem::new("Defer").on_click({
                let view = view.clone();
                move |_, window, cx| {
                    if let Some(entity) = view.upgrade() {
                        entity.update(cx, |this, cx| {
                            this.submit_action("defer", window, cx);
                        });
                    }
                }
            }))
            .item(PopupMenuItem::new("More options").on_click({
                let view = view.clone();
                move |_, window, cx| {
                    if let Some(entity) = view.upgrade() {
                        entity.update(cx, |this, cx| {
                            this.submit_action("more-options", window, cx);
                        });
                    }
                }
            }))
            .item(PopupMenuItem::new("Deep dive").on_click({
                let view = view.clone();
                move |_, window, cx| {
                    if let Some(entity) = view.upgrade() {
                        entity.update(cx, |this, cx| {
                            this.submit_action("deep-dive", window, cx);
                        });
                    }
                }
            }))
        })
}

fn status_footer(
    cx: &mut Context<WorkspaceView>,
    status: &SharedString,
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
    use super::{ResearcherStatusKind, reopen_complete_with_bound_queue, researcher_status_from_text};
    use crate::interview::{InterviewSessionStatus, SessionStore, TodPaths};
    use std::fs;

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
}
