use crate::interview::agent::{AgentProvider, AgentRunState, CursorAcpProvider, RunId};
use crate::interview::config::{InterviewConfig, parse_interview_config};
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
    KeyBinding, ParentElement, Render, ScrollHandle, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants, DropdownButton};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, StyledExt, h_flex, v_flex};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

actions!(
    interview_workspace,
    [
        SubmitAnswer,
        McKeyA,
        McKeyB,
        McKeyC,
        McKeyD,
        QuestionMoveUp,
        QuestionMoveDown,
    ]
);

const WORKSPACE_CONTEXT: &str = "InterviewWorkspace";
const MAX_RESEARCHER_RETRIES: u32 = 3;

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
    queue_watcher: QueueWatcher,
    agent: Arc<Mutex<CursorAcpProvider>>,
    runs: HashMap<RunId, RunKind>,
    last_submitted_id: Option<String>,
    replenish_state: ReplenishState,
    status_line: SharedString,
    error_banner: Option<SharedString>,
    mutations_blocked: bool,
    deep_dive: Option<Entity<DeepDiveView>>,
    _deep_dive_subscription: Option<Subscription>,
    pending_notes_paste: Option<String>,
    focus_handle: FocusHandle,
    question_list_scroll_handle: ScrollHandle,
}

impl WorkspaceView {
    pub fn new(
        session: InterviewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
        agent: Arc<Mutex<CursorAcpProvider>>,
    ) -> Self {
        register_workspace_keys(cx);
        let paths = TodPaths::discover().expect("failed to resolve tod paths");
        let store = SessionStore::open(&paths).expect("failed to open session store");
        let config_path = session
            .config_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .unwrap_or_else(|| paths.repo_root().join("interview-config.md"));
        let settings = TodSettings::load(&paths).unwrap_or_default();
        let config = parse_interview_config(&config_path).unwrap_or_else(|_| InterviewConfig {
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
            queue: paths.repo_root().join("queue"),
            config_path,
            queue_target: None,
            to_process: None,
            researcher_status: None,
            answer_processor_status: None,
            scope: Vec::new(),
            state_agent: None,
        });
        let queue_watcher =
            QueueWatcher::new(config.queue.clone()).expect("failed to start queue watcher");
        let questions = load_queue_dir(&config.queue).unwrap_or_default();
        let selected_question_id = questions.first().map(|q| q.id.clone());
        let notes_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(5)
                .placeholder("Notes (Ctrl+Enter to submit)")
        });
        let mutations_blocked = session.status == InterviewSessionStatus::Archived
            || session.status == InterviewSessionStatus::Complete;

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
            last_submitted_id: None,
            replenish_state: ReplenishState::default(),
            status_line: SharedString::default(),
            error_banner: None,
            mutations_blocked,
            deep_dive: None,
            _deep_dive_subscription: None,
            pending_notes_paste: None,
            focus_handle: cx.focus_handle().tab_stop(true),
            question_list_scroll_handle: ScrollHandle::new(),
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
            && !self.is_complete()
            && !self.replenish_state.manual_required
    }

    fn poll_runs_and_queue(&mut self, cx: &mut Context<Self>) {
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
        }

        if let Ok(Some(questions)) = self.queue_watcher.poll() {
            self.apply_queue_update(questions);
        }

        self.maybe_start_replenishment(cx);

        if self.is_complete() && self.session.status == InterviewSessionStatus::Active {
            let _ = self
                .store
                .set_status(self.session.id, InterviewSessionStatus::Complete);
            self.session.status = InterviewSessionStatus::Complete;
            self.mutations_blocked = true;
            cx.emit(WorkspaceEvent::SessionComplete);
        }

        cx.notify();
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
                        self.last_submitted_id = None;
                    }
                    RunKind::ResearcherReplenish => {
                        self.replenish_state.retry_count = 0;
                        self.replenish_state.next_retry_at = None;
                        self.status_line = "Researcher replenishment succeeded".into();
                    }
                    RunKind::ResearcherAction { question_id } => {
                        self.status_line =
                            format!("Researcher action completed for {question_id}").into();
                        self.last_submitted_id = None;
                    }
                }
            }
            (RunKind::AnswerProcessor { .. }, Err(message)) => {
                self.error_banner = Some(message.into());
                self.status_line = "Answer processor failed".into();
                if let Some(id) = self.last_submitted_id.take() {
                    self.pending.remove(&id);
                    self.pending_snapshots.remove(&id);
                }
            }
            (RunKind::ResearcherReplenish, Err(message)) => {
                self.error_banner = Some(message.clone().into());
                self.status_line = "Researcher replenishment failed".into();
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
            (RunKind::ResearcherAction { .. }, Err(message)) => {
                self.error_banner = Some(message.into());
                self.status_line = "Researcher action failed".into();
                if let Some(id) = self.last_submitted_id.take() {
                    self.pending.remove(&id);
                    self.pending_snapshots.remove(&id);
                }
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
        }
    }

    fn is_complete(&self) -> bool {
        self.questions.is_empty()
            && !self.answer_in_flight()
            && self.researcher_in_flight() == 0
            && researcher_is_complete(self.config.researcher_status.as_deref())
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
        self.question_list_scroll_handle.scroll_to_item(new_idx);
        cx.notify();
    }

    fn reset_response_fields(&mut self) {
        self.selected_mc = None;
    }

    fn is_question_pending(&self, id: &str) -> bool {
        self.pending.contains(id)
    }

    fn can_mutate(&self) -> bool {
        !self.mutations_blocked && !self.answer_in_flight()
    }

    fn submit_answer(&mut self, cx: &mut Context<Self>) {
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
                    self.last_submitted_id = Some(question.id.clone());
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
                    self.last_submitted_id = Some(question.id.clone());
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
                self.question_list_scroll_handle.scroll_to_item(idx);
                return;
            }
        }
    }

    fn select_mc(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.can_mutate() {
            return;
        }
        if let Some(q) = self.selected_question() {
            if q.options.iter().any(|o| o.key.eq_ignore_ascii_case(key)) {
                self.selected_mc = Some(key.to_ascii_uppercase());
                cx.notify();
            }
        }
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
    cx.bind_keys([
        KeyBinding::new("ctrl-enter", SubmitAnswer, context),
        KeyBinding::new("a", McKeyA, context),
        KeyBinding::new("b", McKeyB, context),
        KeyBinding::new("c", McKeyC, context),
        KeyBinding::new("d", McKeyD, context),
        KeyBinding::new("up", QuestionMoveUp, context),
        KeyBinding::new("down", QuestionMoveDown, context),
    ]);
}

fn file_contents(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn researcher_is_complete(path: Option<&Path>) -> bool {
    let Some(path) = path else {
        return true;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|line| {
        let t = line.trim();
        t.starts_with("status:") && t.contains("complete")
    })
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

        self.poll_runs_and_queue(cx);

        if let Some(deep_dive) = &self.deep_dive {
            return div().size_full().child(deep_dive.clone());
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
            .bg(background)
            .v_flex()
            .on_action(cx.listener(|this, _: &SubmitAnswer, _, cx| {
                this.submit_answer(cx);
            }))
            .on_action(cx.listener(|this, _: &McKeyA, _, cx| {
                this.select_mc("A", cx);
            }))
            .on_action(cx.listener(|this, _: &McKeyB, _, cx| {
                this.select_mc("B", cx);
            }))
            .on_action(cx.listener(|this, _: &McKeyC, _, cx| {
                this.select_mc("C", cx);
            }))
            .on_action(cx.listener(|this, _: &McKeyD, _, cx| {
                this.select_mc("D", cx);
            }))
            .on_action(cx.listener(|this, _: &QuestionMoveUp, _, cx| {
                this.move_question_selection(-1, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &QuestionMoveDown, _, cx| {
                this.move_question_selection(1, cx);
                cx.stop_propagation();
            }))
            .child(workspace_header(cx, &self.session, border, muted))
            .when(archived, |el| el.child(archived_banner(border, muted)))
            .when_some(self.error_banner.clone(), |el, msg| {
                el.child(error_banner(msg, border))
            })
            .child(
                h_flex()
                    .flex_1()
                    .child(question_list_column(
                        cx,
                        &self.questions,
                        &self.selected_question_id,
                        &self.pending,
                        &self.question_list_scroll_handle,
                        accent,
                        foreground,
                        muted,
                        border,
                    ))
                    .child(body_column(
                        cx,
                        self.is_complete(),
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
                        accent,
                        foreground,
                        muted,
                        border,
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
        .items_center()
        .justify_between()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(border)
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .child(session.display_name.clone()),
                )
                .child(div().text_xs().text_color(muted).child(entity_label)),
        )
        .child(
            Button::new("back-to-sessions")
                .label("Back to interviews")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.back_to_sessions(cx);
                })),
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
    accent: gpui::Hsla,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
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
        scroll_content = scroll_content.child(
            div()
                .id(("question-row", idx))
                .px_3()
                .py_2()
                .cursor_pointer()
                .border_l_2()
                .border_color(if is_selected {
                    accent
                } else {
                    gpui::transparent_black()
                })
                .when(is_selected, |el| el.bg(accent.opacity(0.08)))
                .when(is_pending, |el| el.opacity(0.45))
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| this.select_question(&id, cx)
                }))
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .font_semibold()
                                .text_color(if is_pending { muted } else { accent })
                                .child(id),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(if is_pending { muted } else { foreground })
                                .child(question.short_label.clone()),
                        ),
                ),
        );
    }

    v_flex()
        .w(px(240.))
        .h_full()
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
            .text_sm()
            .text_color(foreground)
            .child(q.body.clone())
            .into_any_element()
    } else {
        div()
            .text_sm()
            .text_color(muted)
            .child("No open questions")
            .into_any_element()
    };
    v_flex()
        .flex_1()
        .h_full()
        .border_r_1()
        .border_color(border)
        .p_4()
        .overflow_y_scrollbar()
        .child(body)
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
                .on_click(cx.listener(|this, _, _, cx| {
                    this.back_to_sessions(cx);
                })),
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
    accent: gpui::Hsla,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    _border: gpui::Hsla,
) -> impl IntoElement {
    let disabled = !can_mutate || pending || question.is_none();
    let mut col =
        v_flex()
            .w(px(320.))
            .h_full()
            .p_4()
            .gap_3()
            .child(div().text_xs().text_color(muted).child(if pending {
                "Pending — waiting for agent"
            } else {
                "Response"
            }));
    if let Some(q) = question {
        for (idx, opt) in q.options.iter().enumerate() {
            let key = opt.key.clone();
            col = col.child(mc_option_row(
                cx,
                idx,
                opt.clone(),
                selected_mc
                    .as_ref()
                    .is_some_and(|k| k.eq_ignore_ascii_case(&key)),
                accent,
                foreground,
                muted,
                disabled,
            ));
        }
    }
    col.child(Input::new(notes_input).disabled(disabled).w_full())
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(action_dropdown(cx, disabled))
                .child(
                    Button::new("submit-answer")
                        .label("Submit")
                        .primary()
                        .disabled(disabled)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.submit_answer(cx);
                        })),
                ),
        )
}

fn mc_option_row(
    cx: &mut Context<WorkspaceView>,
    idx: usize,
    opt: crate::interview::queue::McOption,
    selected: bool,
    accent: gpui::Hsla,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    disabled: bool,
) -> impl IntoElement {
    let key = opt.key.clone();
    div()
        .id(("mc-option", idx))
        .cursor_pointer()
        .when(selected, |el| el.bg(accent.opacity(0.12)))
        .when(disabled, |el| el.opacity(0.5))
        .on_click(cx.listener({
            let key = key.clone();
            move |this, _, _, cx| {
                if !disabled {
                    this.select_mc(&key, cx);
                }
            }
        }))
        .child(
            h_flex()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_sm()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(if selected { accent } else { muted })
                        .child(format!("{}.", opt.key.to_ascii_uppercase())),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(if selected { foreground } else { muted })
                        .child(opt.label),
                ),
        )
}

fn action_dropdown(cx: &mut Context<WorkspaceView>, disabled: bool) -> impl IntoElement {
    let view = cx.entity().downgrade();
    DropdownButton::new("question-actions")
        .disabled(disabled)
        .button(Button::new("actions-trigger").label("Other action"))
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
        .px_4()
        .py_2()
        .border_t_1()
        .border_color(border)
        .justify_between()
        .items_center()
        .child(
            div()
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
