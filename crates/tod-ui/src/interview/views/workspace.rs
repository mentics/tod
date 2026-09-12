use crate::interview::agent::SharedAgent;
use crate::interview::question_feedback::append_question_feedback;
use crate::interview::views::question_list::QuestionListDelegate;
use crate::interview::{
    InterviewSession, InterviewSessionStatus, SessionStore, TaskListProceedContext, TodPaths,
};
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav, on_app_nav_toggle};
use crate::ui::list::{ListArrowDown, ListArrowUp};
use crate::ui::selectable_text::selectable_text;
use crate::views::obligations::{ObligationsEvent, ObligationsView};
use crate::views::plan_steps::{PlanStepsEvent, PlanStepsView};
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
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, Selectable, StyledExt, h_flex, v_flex};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tod_core::interview::driver::{DriverEvent, DriverStatus, InterviewDriver};
use tod_core::interview::interview_complete;
use tod_core::process::lifecycle_for_interview_phase;
use tod_store::fleet::FleetStore;
use tod_store::interview::{
    ACTOR_USER, InterviewCommand, InterviewQuestion, InterviewRepo, PHASE_PLANNING, Proposal,
    ProposalOp, STATUS_ANSWERED, STATUS_OPEN, phase_for_session_key,
};
use tod_store::outline::repos::{NodeRepo, ObligationRepo};
use uuid::Uuid;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Recent answers listed under the question.
const RECENT_ANSWERS: usize = 5;

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
const OTHER_ACTION_ITEMS: [(&str, &str); 3] = [
    ("improve", "Improve this question"),
    ("more-options", "More options"),
    ("defer", "Defer"),
];
const LIST_COLUMN_WIDTH: f32 = 160.;
const LIST_COLUMN_MIN: f32 = 120.;
/// Middle reading pane. Initial width; response column flexes for remaining width.
const BODY_COLUMN_WIDTH: f32 = 250.;
const BODY_COLUMN_MIN: f32 = 160.;
const RESPONSE_COLUMN_MIN: f32 = 200.;
const OBLIGATIONS_COLUMN_WIDTH: f32 = 280.;
const OBLIGATIONS_COLUMN_MIN: f32 = 220.;
/// `InputState` has no character-column API, so all multi-line fields (notes,
/// proposed text, question feedback, freeform submission) share one row count
/// and pixel height instead of a literal 40-column width.
const TEXTAREA_ROWS: usize = 4;
const TEXTAREA_HEIGHT: f32 = 96.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceFocus {
    QuestionList,
    /// Index into response interactive controls (MC options, optional proposed text, Notes,
    /// Other action, Submit, feedback field, Submit feedback, freeform field, Submit freeform).
    Response(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceEvent {
    NavigateBack,
    SessionComplete,
    /// User chose **Proceed** on the in-place Complete state (task-list origin).
    ProceedToLifecycle,
}

/// Options the user can pick for `q`: its own, or a lone Accept for a
/// proposal offered without alternatives.
fn option_labels(q: &InterviewQuestion) -> Vec<String> {
    if q.options.is_empty() && q.proposal.is_some() {
        vec!["Accept".to_string()]
    } else {
        q.options.clone()
    }
}

/// The part of a proposal the user may edit before accepting.
fn proposal_text(q: &InterviewQuestion) -> Option<&str> {
    let proposal = q.proposal.as_ref()?;
    match proposal.op {
        ProposalOp::Add | ProposalOp::Update | ProposalOp::Content => proposal.text.as_deref(),
        ProposalOp::Delete => None,
    }
}

/// The edited proposal text to send, when it differs from what was proposed.
fn edited_proposal_text(original: Option<&str>, edited: &str) -> Option<String> {
    let original = original?.trim();
    let edited = edited.trim();
    (!edited.is_empty() && edited != original).then(|| edited.to_string())
}

pub struct WorkspaceView {
    session: InterviewSession,
    store: SessionStore,
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    driver: Arc<Mutex<InterviewDriver>>,
    phase: &'static str,
    questions: Vec<InterviewQuestion>,
    recent: Vec<InterviewQuestion>,
    /// Plain-language description of each open question's proposal, by seq.
    proposal_summaries: HashMap<i64, String>,
    selected_seq: Option<i64>,
    selected_mc: Option<String>,
    notes_input: Entity<InputState>,
    proposed_input: Entity<InputState>,
    feedback_input: Entity<InputState>,
    freeform_input: Entity<InputState>,
    obligations: Entity<ObligationsView>,
    /// Keeps the embedded obligations panel alive; "closed" is not a valid state
    /// for this column, so a Close event is reversed immediately.
    _obligations_subscription: Subscription,
    plan_steps: Entity<PlanStepsView>,
    /// Keeps the embedded plan-steps panel alive; "closed" is not a valid state
    /// for this column, so a Close event is reversed immediately.
    _plan_steps_subscription: Subscription,
    /// Question seq whose proposal text is currently loaded into `proposed_input`.
    proposed_loaded_for: Option<i64>,
    /// Response fields were reset without a Window; clear their text on the next render.
    notes_pending_clear: bool,
    driver_status: DriverStatus,
    /// Set once the first `reload` has completed, to avoid a "No open questions"
    /// flash before the driver status has had a chance to report itself.
    loaded_once: bool,
    complete: bool,
    status_line: SharedString,
    error_banner: Option<SharedString>,
    mutations_blocked: bool,
    focus_handle: FocusHandle,
    question_list_state: Entity<ListState<QuestionListDelegate>>,
    _question_list_subscription: Subscription,
    workspace_focus: WorkspaceFocus,
    notes_editing: bool,
    proposed_editing: bool,
    feedback_editing: bool,
    freeform_editing: bool,
    /// Open state for the native PopupMenu. Menu entity is eager (keyboard);
    /// paint uses `deferred` so it stacks above the bottom feedback panel.
    actions_menu_open: bool,
    actions_menu: Option<Entity<PopupMenu>>,
    _actions_menu_subscription: Option<Subscription>,
    /// When the current wait for questions began.
    wait_started: Option<Instant>,
    replenish_target: u32,
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

    pub fn set_task_list_proceed(&mut self, context: Option<TaskListProceedContext>) {
        self.task_list_proceed = context;
    }

    pub fn new(
        session: InterviewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
        agent: SharedAgent,
        fleet: Arc<FleetStore>,
        driver: Arc<Mutex<InterviewDriver>>,
        task_list_proceed: Option<TaskListProceedContext>,
    ) -> Self {
        register_workspace_keys(cx);
        let store = SessionStore::open(fleet.clone());
        let replenish_target = driver
            .lock()
            .map(|d| d.config().replenish_threshold)
            .unwrap_or(8);
        let textarea = |placeholder: &'static str, window: &mut Window, cx: &mut App| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .rows(TEXTAREA_ROWS)
                    .placeholder(placeholder)
            })
        };
        let notes_input = textarea("Notes (Enter to edit; Ctrl+Enter to submit)", window, cx);
        let proposed_input = textarea("Proposed text (Enter to edit)", window, cx);
        let feedback_input =
            textarea("Feedback on this question (e.g. not useful, too meta)", window, cx);
        let freeform_input =
            textarea("Anything to tell the interview directly (Ctrl+Enter to submit)", window, cx);

        let obligations = cx.new(|cx| ObligationsView::new(window, cx, fleet.clone()));
        let obligations_phase = phase_for_session_key(&session.phase);
        obligations.update(cx, |panel, cx| {
            panel.open(
                session.node_id,
                &session.display_name,
                Some(obligations_phase),
                window,
                cx,
            );
        });
        let obligations_node_id = session.node_id;
        let obligations_title = session.display_name.clone();
        let _obligations_subscription = cx.subscribe_in(
            &obligations,
            window,
            move |this, panel, event, window, cx| match event {
                ObligationsEvent::Close => {
                    panel.update(cx, |panel, cx| {
                        panel.retarget(
                            obligations_node_id,
                            &obligations_title,
                            Some(obligations_phase),
                            true,
                            window,
                            cx,
                        );
                    });
                }
                // Ctrl+Left out of the third column lands on the response column.
                ObligationsEvent::FocusTaskList => this.focus_response_right(window, cx),
                ObligationsEvent::DeleteSelectedTask
                | ObligationsEvent::OpenAgentChat { .. }
                | ObligationsEvent::OpenAgentConfig { .. } => {}
            },
        );

        let plan_steps = cx.new(|cx| PlanStepsView::new(window, cx, fleet.clone()));
        plan_steps.update(cx, |panel, cx| {
            panel.open(session.node_id, &session.display_name, window, cx);
        });
        let plan_steps_node_id = session.node_id;
        let plan_steps_title = session.display_name.clone();
        let _plan_steps_subscription = cx.subscribe_in(
            &plan_steps,
            window,
            move |this, panel, event, window, cx| match event {
                PlanStepsEvent::Close => {
                    panel.update(cx, |panel, cx| {
                        panel.retarget(plan_steps_node_id, &plan_steps_title, true, window, cx);
                    });
                }
                // Ctrl+Left out of the third column lands on the response column.
                PlanStepsEvent::FocusTaskList => this.focus_response_right(window, cx),
                PlanStepsEvent::DeleteSelectedTask => {}
            },
        );

        let poll_task = cx.spawn(async move |this, cx| {
            loop {
                Timer::after(POLL_INTERVAL).await;
                let Ok(()) = this.update(cx, |this, cx| {
                    if this.poll(cx) {
                        cx.notify();
                    }
                }) else {
                    break;
                };
            }
        });

        let question_list_state =
            cx.new(|cx| ListState::new(QuestionListDelegate::new(Vec::new()), window, cx).searchable(false));
        let _question_list_subscription =
            cx.subscribe(&question_list_state, |this, state, event, cx| match event {
                ListEvent::Select(ix) | ListEvent::Confirm(ix) => {
                    let seq = state.read(cx).delegate().items().get(ix.row).map(|q| q.seq);
                    if let Some(seq) = seq {
                        this.workspace_focus = WorkspaceFocus::QuestionList;
                        this.notes_editing = false;
                        this.proposed_editing = false;
                        this.feedback_editing = false;
                        this.select_question_without_window(seq, cx);
                    }
                }
                ListEvent::Cancel => {}
            });

        let phase = phase_for_session_key(&session.phase);
        let mutations_blocked = session.status == InterviewSessionStatus::Archived;
        let mut view = Self {
            session,
            store,
            fleet,
            agent,
            driver,
            phase,
            questions: Vec::new(),
            recent: Vec::new(),
            proposal_summaries: HashMap::new(),
            selected_seq: None,
            selected_mc: None,
            notes_input,
            proposed_input,
            feedback_input,
            freeform_input,
            obligations,
            _obligations_subscription,
            plan_steps,
            _plan_steps_subscription,
            proposed_loaded_for: None,
            notes_pending_clear: false,
            driver_status: DriverStatus::default(),
            loaded_once: false,
            complete: false,
            status_line: SharedString::default(),
            error_banner: None,
            mutations_blocked,
            focus_handle: cx.focus_handle().tab_stop(true),
            question_list_state,
            _question_list_subscription,
            workspace_focus: WorkspaceFocus::QuestionList,
            notes_editing: false,
            proposed_editing: false,
            feedback_editing: false,
            freeform_editing: false,
            actions_menu_open: false,
            actions_menu: None,
            _actions_menu_subscription: None,
            wait_started: None,
            replenish_target,
            _poll_task: poll_task,
            app_nav: AppNavMenu::default(),
            task_list_proceed,
        };
        view.reload(cx);

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

    /// Advance the agents and refresh from the database. Returns whether anything changed.
    fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        if !self.mutations_blocked {
            if let (Ok(mut agent), Ok(mut driver)) = (self.agent.try_lock(), self.driver.lock()) {
                for event in driver.tick(&self.fleet, agent.as_mut()) {
                    changed = true;
                    match event {
                        DriverEvent::QuestionMakerFinished { error: None } => {}
                        DriverEvent::AnswersFinished { questions, error: None } => {
                            let labels: Vec<String> =
                                questions.iter().map(|s| format!("q-{s}")).collect();
                            self.status_line = format!("Processed {}", labels.join(", ")).into();
                        }
                        DriverEvent::QuestionMakerFinished { error: Some(err) }
                        | DriverEvent::AnswersFinished { error: Some(err), .. } => {
                            self.error_banner = Some(err.into());
                        }
                    }
                }
                let status = driver.status();
                if status != self.driver_status {
                    self.driver_status = status;
                    changed = true;
                }
            }
        }
        if self.reload(cx) {
            changed = true;
        }
        if self.question_maker_waiting() {
            self.wait_started.get_or_insert_with(Instant::now);
            changed = true;
        } else {
            self.wait_started = None;
        }
        changed
    }

    /// Re-read questions and completion; returns whether anything visible changed.
    fn reload(&mut self, cx: &mut Context<Self>) -> bool {
        let node = self.session.node_id;
        let session_id = self.session.id;
        let phase = self.phase;
        let Ok((questions, recent, summaries, complete)) = self.fleet.read(|conn| {
            let repo = InterviewRepo::new(conn);
            let all = repo.list_questions(node, &[])?;
            let questions: Vec<InterviewQuestion> =
                all.iter().filter(|q| q.status == STATUS_OPEN).cloned().collect();
            let mut recent: Vec<InterviewQuestion> = all
                .iter()
                .filter(|q| q.status == STATUS_ANSWERED && q.phase == phase)
                .cloned()
                .collect();
            recent.sort_by_key(|q| std::cmp::Reverse(q.answered_at));
            recent.truncate(RECENT_ANSWERS);
            let obligations = ObligationRepo::new(conn);
            let summaries = questions
                .iter()
                .filter_map(|q| {
                    q.proposal
                        .as_ref()
                        .map(|p| (q.seq, describe_proposal(p, &obligations)))
                })
                .collect();
            Ok((questions, recent, summaries, interview_complete(conn, node, session_id)?))
        }) else {
            return false;
        };

        let mut changed = false;
        if questions != self.questions {
            self.questions = questions;
            self.proposal_summaries = summaries;
            if self
                .selected_seq
                .is_none_or(|seq| !self.questions.iter().any(|q| q.seq == seq))
            {
                self.selected_seq = self.questions.first().map(|q| q.seq);
                self.reset_response_fields(None, cx);
            }
            self.sync_question_list_items(cx);
            changed = true;
        }
        if recent != self.recent {
            self.recent = recent;
            changed = true;
        }

        if complete != self.complete {
            self.complete = complete;
            changed = true;
        }
        if complete && self.session.status == InterviewSessionStatus::Active {
            if let Ok(session) = self
                .store
                .set_status(self.session.id, InterviewSessionStatus::Complete)
            {
                self.session = session;
            }
            self.status_line = "Interview complete".into();
            cx.emit(WorkspaceEvent::SessionComplete);
        } else if !complete && self.session.status == InterviewSessionStatus::Complete {
            if let Ok(session) = self
                .store
                .set_status(self.session.id, InterviewSessionStatus::Active)
            {
                self.session = session;
            }
            if self.status_line.as_ref() == "Interview complete" {
                self.status_line = SharedString::default();
            }
        }
        self.loaded_once = true;
        changed
    }

    fn question_maker_waiting(&self) -> bool {
        self.questions.is_empty() && self.driver_status.question_maker_running
    }

    fn agent_status_text(&self) -> SharedString {
        let mut parts = Vec::new();
        if self.driver_status.question_maker_running {
            parts.push("Question maker is writing questions".to_string());
        }
        if self.driver_status.answers_in_flight > 0 {
            let lanes = if self.driver_status.answer_lanes_busy > 1 {
                format!(" in {} sessions", self.driver_status.answer_lanes_busy)
            } else {
                String::new()
            };
            parts.push(format!(
                "Processing {} answer{}{lanes}",
                self.driver_status.answers_in_flight,
                if self.driver_status.answers_in_flight == 1 { "" } else { "s" }
            ));
        }
        if parts.is_empty() {
            return self.status_line.clone();
        }
        parts.join(" · ").into()
    }

    fn interview_node_context(&self) -> (String, String) {
        self.fleet
            .read(|conn| {
                let nodes = NodeRepo::new(conn);
                let title = nodes
                    .get(self.session.node_id)?
                    .map(|n| n.title)
                    .unwrap_or_else(|| self.session.display_name.clone());
                let lifecycle = nodes
                    .get_lifecycle(self.session.node_id)?
                    .unwrap_or_else(|| lifecycle_for_interview_phase(&self.session.phase).into());
                Ok((title, lifecycle))
            })
            .unwrap_or_else(|_| (self.session.display_name.clone(), String::new()))
    }

    fn notes_focused(&self, window: &Window, cx: &App) -> bool {
        self.notes_input.read(cx).focus_handle(cx).is_focused(window)
    }

    fn proposed_focused(&self, window: &Window, cx: &App) -> bool {
        self.proposed_input.read(cx).focus_handle(cx).is_focused(window)
    }

    fn feedback_focused(&self, window: &Window, cx: &App) -> bool {
        self.feedback_input.read(cx).focus_handle(cx).is_focused(window)
    }

    fn response_text_editing(&self) -> bool {
        self.notes_editing || self.proposed_editing || self.feedback_editing || self.freeform_editing
    }

    fn has_proposed_editor(&self) -> bool {
        self.selected_question()
            .and_then(proposal_text)
            .is_some_and(|t| !t.trim().is_empty())
    }

    fn clear_validation_banner(&mut self) {
        if self
            .error_banner
            .as_ref()
            .is_some_and(|msg| msg.as_ref() == "Pick an option and/or write notes")
        {
            self.error_banner = None;
        }
    }

    fn retry_agents(&mut self, cx: &mut Context<Self>) {
        if let Ok(mut driver) = self.driver.lock() {
            driver.retry();
        }
        self.error_banner = None;
        cx.notify();
    }

    fn sync_question_list_items(&mut self, cx: &mut Context<Self>) {
        let questions = self.questions.clone();
        let selected = self.selected_seq;
        self.question_list_state.update(cx, |state, cx| {
            state.delegate_mut().set_items(questions);
            match selected {
                Some(seq) => {
                    let _ = state.delegate_mut().select_by_seq(seq);
                }
                None => state.delegate_mut().clear_selected_index(),
            }
            cx.notify();
        });
    }

    /// Keep ListState selection aligned with workspace selection (needs a Window).
    fn sync_question_list_selection(&mut self, window: &mut Window, cx: &mut Context<Self>, scroll: bool) {
        let questions = self.questions.clone();
        let selected = self.selected_seq;
        self.question_list_state.update(cx, |state, cx| {
            state.delegate_mut().set_items(questions);
            let ix = selected
                .and_then(|seq| state.delegate().index_of_seq(seq))
                .map(IndexPath::new);
            state.set_selected_index(ix, window, cx);
            if scroll && ix.is_some() {
                state.scroll_to_selected_item(window, cx);
            }
        });
    }

    fn selected_question(&self) -> Option<&InterviewQuestion> {
        self.selected_seq
            .and_then(|seq| self.questions.iter().find(|q| q.seq == seq))
    }

    fn select_question(&mut self, seq: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_seq == Some(seq) {
            return;
        }
        self.selected_seq = Some(seq);
        self.reset_response_fields(Some(window), cx);
        self.clear_validation_banner();
        self.sync_question_list_selection(window, cx, true);
        cx.notify();
    }

    fn select_question_without_window(&mut self, seq: i64, cx: &mut Context<Self>) {
        if self.selected_seq == Some(seq) {
            return;
        }
        self.selected_seq = Some(seq);
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
            .selected_seq
            .and_then(|seq| self.questions.iter().position(|q| q.seq == seq))
            .unwrap_or(0);
        let new_idx = if delta < 0 {
            current.saturating_sub((-delta) as usize)
        } else {
            (current + delta as usize).min(self.questions.len() - 1)
        };
        if new_idx != current {
            let seq = self.questions[new_idx].seq;
            self.select_question(seq, window, cx);
        }
    }

    /// Select the question after the current one (wrapping), once it is gone.
    fn select_next_question(&mut self, after: i64, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let next = self
            .questions
            .iter()
            .find(|q| q.seq > after)
            .or_else(|| self.questions.iter().find(|q| q.seq != after))
            .map(|q| q.seq);
        self.selected_seq = next;
        self.reset_response_fields(window, cx);
        self.clear_validation_banner();
        self.sync_question_list_items(cx);
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
            self.notes_input.update(cx, |input, cx| input.set_value("", window, cx));
            self.feedback_input.update(cx, |input, cx| input.set_value("", window, cx));
            self.sync_proposed_input(window, cx);
            if should_unfocus {
                self.focus_handle.focus(window);
            }
        } else {
            // Cleared on the next render, which has a Window.
            self.notes_pending_clear = true;
        }
        if matches!(self.workspace_focus, WorkspaceFocus::Response(_)) {
            self.workspace_focus = if self.selected_seq.is_none() {
                WorkspaceFocus::QuestionList
            } else {
                WorkspaceFocus::Response(0)
            };
        }
    }

    /// Load the selected question's proposal text into the editor (or clear it).
    fn sync_proposed_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.proposed_loaded_for == self.selected_seq {
            return;
        }
        let text = self
            .selected_question()
            .and_then(proposal_text)
            .unwrap_or_default()
            .to_string();
        self.proposed_input.update(cx, |input, cx| input.set_value(text, window, cx));
        self.proposed_loaded_for = self.selected_seq;
    }

    fn can_mutate(&self) -> bool {
        !self.mutations_blocked
    }

    fn interview(&mut self, command: InterviewCommand) -> Option<serde_json::Value> {
        match self.fleet.interview(ACTOR_USER, command) {
            Ok(value) => Some(value),
            Err(err) => {
                self.error_banner = Some(format!("{err:#}").into());
                None
            }
        }
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
        let notes = self.notes_input.read(cx).value().trim().to_string();
        let option = self.selected_mc.as_deref().and_then(|k| k.parse::<i64>().ok());
        if notes.is_empty() && option.is_none() {
            self.error_banner = Some("Pick an option and/or write notes".into());
            cx.notify();
            return;
        }
        let edited = edited_proposal_text(
            proposal_text(&question),
            &self.proposed_input.read(cx).value(),
        );
        let Some(value) = self.interview(InterviewCommand::AnswerQuestion {
            node_id: self.session.node_id,
            seq: question.seq,
            option,
            text: Some(notes).filter(|n| !n.is_empty()),
            edited_text: edited,
        }) else {
            cx.notify();
            return;
        };
        self.error_banner = value
            .pointer("/applied/error")
            .and_then(|e| e.as_str())
            .map(|err| format!("Answer recorded, but the proposal was not applied: {err}").into());
        self.status_line = format!("Answered {}", question.label()).into();
        self.questions.retain(|q| q.seq != question.seq);
        self.select_next_question(question.seq, Some(window), cx);
        self.reload(cx);
        cx.notify();
    }

    fn submit_freeform(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_mutate() {
            return;
        }
        let text = self.freeform_input.read(cx).value().trim().to_string();
        if text.is_empty() {
            self.error_banner = Some("Enter some text before submitting".into());
            cx.notify();
            return;
        }
        if self
            .interview(InterviewCommand::SubmitFreeform {
                node_id: self.session.node_id,
                session_id: Some(self.session.id),
                phase: self.phase.to_string(),
                text,
            })
            .is_some()
        {
            self.error_banner = None;
            self.status_line = "Sent to the interview".into();
            self.freeform_editing = false;
            self.freeform_input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        cx.notify();
    }

    fn submit_action(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_mutate() {
            return;
        }
        let Some(question) = self.selected_question().cloned() else {
            return;
        };
        let notes = self.notes_input.read(cx).value().trim().to_string();
        let node_id = self.session.node_id;
        let command = match action {
            "defer" => InterviewCommand::DeferQuestion {
                node_id,
                seq: question.seq,
            },
            "more-options" => InterviewCommand::WithdrawQuestion {
                node_id,
                seq: question.seq,
                reason: if notes.is_empty() {
                    "The user wants more options.".into()
                } else {
                    format!("The user wants more options: {notes}")
                },
            },
            _ => InterviewCommand::WithdrawQuestion {
                node_id,
                seq: question.seq,
                reason: if notes.is_empty() {
                    "The user asked for a better question.".into()
                } else {
                    notes
                },
            },
        };
        if self.interview(command).is_none() {
            cx.notify();
            return;
        }
        if action != "defer" {
            if let Ok(mut driver) = self.driver.lock() {
                driver.wake_question_maker();
            }
        }
        self.error_banner = None;
        self.status_line = match action {
            "defer" => format!("Deferred {}", question.label()),
            _ => format!("Sent {} back for a better version", question.label()),
        }
        .into();
        self.questions.retain(|q| q.seq != question.seq);
        self.select_next_question(question.seq, Some(window), cx);
        self.reload(cx);
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
            &question.label(),
            &node_title,
            &lifecycle_state,
            &feedback,
            &question_source(&question),
        ) {
            self.error_banner = Some(format!("Feedback write failed: {err}").into());
            cx.notify();
            return;
        }
        self.error_banner = None;
        self.status_line = format!("Feedback saved for {}", question.label()).into();
        self.feedback_editing = false;
        self.feedback_input.update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn copy_question_source(&mut self, cx: &mut Context<Self>) {
        let Some(question) = self.selected_question().cloned() else {
            self.error_banner = Some("No question selected".into());
            cx.notify();
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(question_source(&question)));
        self.error_banner = None;
        self.status_line = format!("Copied raw source for {}", question.label()).into();
        cx.notify();
    }

    fn on_digit_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Text edit mode suppresses digit MC submit — not mere focus.
        if self.response_text_editing() {
            cx.propagate();
            return;
        }
        self.submit_mc_option(key, window, cx);
    }

    fn submit_mc_option(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_mutate() {
            return;
        }
        let Some(q) = self.selected_question() else {
            return;
        };
        let count = option_labels(q).len();
        if !key.parse::<usize>().is_ok_and(|n| n >= 1 && n <= count) {
            return;
        }
        self.selected_mc = Some(key.to_string());
        self.clear_validation_banner();
        self.submit_answer(window, cx);
    }

    fn option_count(&self) -> usize {
        self.selected_question().map(|q| option_labels(q).len()).unwrap_or(0)
    }

    fn response_stop_count(&self) -> usize {
        let proposed = usize::from(self.has_proposed_editor());
        // Notes, Other action, Submit, feedback field, Submit feedback,
        // freeform field, Submit freeform
        self.option_count() + proposed + 7
    }

    fn proposed_stop_index(&self) -> Option<usize> {
        self.has_proposed_editor().then(|| self.option_count())
    }

    fn notes_stop_index(&self) -> usize {
        self.option_count() + usize::from(self.has_proposed_editor())
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

    fn freeform_stop_index(&self) -> usize {
        self.feedback_submit_stop_index() + 1
    }

    fn freeform_submit_stop_index(&self) -> usize {
        self.freeform_stop_index() + 1
    }

    fn actions_disabled(&self) -> bool {
        !self.can_mutate() || self.selected_question().is_none()
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
            menu.update(cx, |menu, cx| menu.focus_handle(cx).focus(window));
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
        if !self.can_mutate() || self.selected_question().is_none() {
            return;
        }
        self.proposed_editing = false;
        self.workspace_focus = WorkspaceFocus::Response(self.notes_stop_index());
        self.notes_editing = true;
        cx.notify();
        // Focus after Input re-renders enabled (disabled when !notes_editing).
        cx.on_next_frame(window, |this, window, cx| {
            this.notes_input.update(cx, |input, cx| input.focus(window, cx));
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
        if !self.can_mutate() || !self.has_proposed_editor() {
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
            this.proposed_input.update(cx, |input, cx| input.focus(window, cx));
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
        if !self.can_mutate() || self.selected_question().is_none() {
            return;
        }
        self.notes_editing = false;
        self.proposed_editing = false;
        self.workspace_focus = WorkspaceFocus::Response(self.feedback_stop_index());
        self.feedback_editing = true;
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.feedback_input.update(cx, |input, cx| input.focus(window, cx));
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

    fn enter_freeform_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_mutate() {
            return;
        }
        self.notes_editing = false;
        self.proposed_editing = false;
        self.feedback_editing = false;
        self.workspace_focus = WorkspaceFocus::Response(self.freeform_stop_index());
        self.freeform_editing = true;
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.freeform_input.update(cx, |input, cx| input.focus(window, cx));
        });
    }

    fn exit_freeform_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.freeform_editing {
            return;
        }
        self.freeform_editing = false;
        self.workspace_focus = WorkspaceFocus::Response(self.freeform_stop_index());
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

    /// Focus the third (obligations, or plan steps during planning) column.
    fn focus_obligations_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.response_text_editing() {
            return;
        }
        if self.phase == PHASE_PLANNING {
            self.plan_steps.update(cx, |panel, cx| panel.focus_handle(cx).focus(window));
        } else {
            self.obligations.update(cx, |panel, cx| panel.focus_handle(cx).focus(window));
        }
        cx.notify();
    }

    fn focus_list_left(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.response_text_editing() {
            return;
        }
        self.workspace_focus = WorkspaceFocus::QuestionList;
        self.question_list_state.update(cx, |state, cx| state.focus(window, cx));
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
        if idx < self.option_count() {
            self.submit_mc_option(&(idx + 1).to_string(), window, cx);
            return;
        }
        if self.proposed_stop_index() == Some(idx) {
            self.enter_proposed_edit(window, cx);
        } else if idx == self.notes_stop_index() {
            self.enter_notes_edit(window, cx);
        } else if idx == self.submit_stop_index() {
            self.submit_answer(window, cx);
        } else if idx == self.actions_stop_index() && !self.actions_disabled() {
            self.open_actions_menu_from_keyboard(window, cx);
        } else if idx == self.feedback_stop_index() {
            self.enter_feedback_edit(window, cx);
        } else if idx == self.feedback_submit_stop_index() {
            self.submit_feedback(window, cx);
        } else if idx == self.freeform_stop_index() {
            self.enter_freeform_edit(window, cx);
        } else if idx == self.freeform_submit_stop_index() {
            self.submit_freeform(window, cx);
        }
    }

    fn handle_workspace_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.freeform_editing {
            self.exit_freeform_edit(window, cx);
        } else if self.proposed_editing {
            self.exit_proposed_edit(window, cx);
        } else if self.notes_editing {
            self.exit_notes_edit(window, cx);
        } else if self.feedback_editing {
            self.exit_feedback_edit(window, cx);
        } else if self.actions_menu_open {
            self.close_actions_menu(window, cx);
        } else {
            cx.emit(WorkspaceEvent::NavigateBack);
        }
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

/// Plain-language description of what accepting `proposal` does, naming any
/// obligation it removes.
fn describe_proposal(proposal: &Proposal, obligations: &ObligationRepo<'_>) -> String {
    let body_of = |raw: &str| {
        Uuid::parse_str(raw)
            .ok()
            .and_then(|id| obligations.get(id).ok().flatten())
            .map(|o| format!("\"{}\"", o.body))
            .unwrap_or_else(|| format!("[{}] (no longer exists)", raw.get(..8).unwrap_or(raw)))
    };
    let section = proposal
        .section
        .as_deref()
        .map(|s| format!(" under {s}"))
        .unwrap_or_default();
    let mut text = match proposal.op {
        ProposalOp::Add => format!(
            "Accepting adds a {}{section}.",
            proposal.kind.as_deref().unwrap_or("obligation")
        ),
        ProposalOp::Update => format!(
            "Accepting rewrites {}.",
            body_of(proposal.id.as_deref().unwrap_or(""))
        ),
        ProposalOp::Delete => format!(
            "Accepting removes {}.",
            body_of(proposal.id.as_deref().unwrap_or(""))
        ),
        ProposalOp::Content => format!(
            "Accepting {} the node's {}.",
            if proposal.append { "adds to" } else { "sets" },
            proposal.content_type.as_deref().unwrap_or("content")
        ),
    };
    for raw in &proposal.replaces {
        text.push_str(&format!(" It also removes {}.", body_of(raw)));
    }
    text
}

/// The question as stored, for "Copy raw source" and feedback logs.
fn question_source(q: &InterviewQuestion) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "id": q.label(),
        "author": q.author,
        "phase": q.phase,
        "covers": q.covers,
        "context": q.context,
        "question": q.question,
        "options": q.options,
        "recommend": q.recommend,
        "proposal": q.proposal,
        "intent": q.intent,
    }))
    .unwrap_or_default()
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
        // Ctrl+arrows cross panels everywhere in the app; accept them here too.
        KeyBinding::new("ctrl-right", FocusRight, context),
        KeyBinding::new("ctrl-left", FocusLeft, context),
        KeyBinding::new("enter", ActivateFocused, context),
        KeyBinding::new("space", ActivateFocused, context),
        KeyBinding::new("escape", WorkspaceEscape, context),
        KeyBinding::new("alt-left", NavigateBack, context),
    ]);
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
        // Align ListState after polls that only had App context (no Window).
        self.sync_question_list_selection(window, cx, false);
        if self.notes_pending_clear {
            self.notes_pending_clear = false;
            self.notes_input.update(cx, |input, cx| input.set_value("", window, cx));
            self.feedback_input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.sync_proposed_input(window, cx);
        if self.phase == PHASE_PLANNING {
            self.plan_steps.update(cx, |panel, cx| panel.reload(window, cx));
        } else {
            self.obligations.update(cx, |panel, cx| panel.reload(window, cx));
        }

        let background = cx.theme().background;
        let border = cx.theme().border;
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let archived = self.session.status == InterviewSessionStatus::Archived;
        let wait = self.question_maker_waiting().then(|| QuestionMakerWaitUi {
            open: self.questions.len(),
            target: self.replenish_target as usize,
            elapsed_secs: self.wait_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
            animate_dots: self
                .wait_started
                .map(|t| ((t.elapsed().as_millis() / 500) % 4) as usize)
                .unwrap_or(0),
        });
        let selected = self.selected_question().cloned();
        let summary = selected
            .as_ref()
            .and_then(|q| self.proposal_summaries.get(&q.seq).cloned());
        let status_text = self.agent_status_text();
        let show_retry = self.driver_status.manual_required || self.driver_status.last_error.is_some();

        div()
            .key_context(WORKSPACE_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(background)
            .v_flex()
            .on_action(cx.listener(|this, _: &SubmitAnswer, window, cx| {
                // Ctrl+Enter is bound globally for Input focus; route to whichever
                // submit matches the field the user is actually editing.
                if this.freeform_input.read(cx).focus_handle(cx).is_focused(window) {
                    this.submit_freeform(window, cx);
                } else {
                    this.submit_answer(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FocusNotes, window, cx| {
                this.enter_notes_edit(window, cx);
            }))
            .on_action(cx.listener(|this, _: &McDigit1, window, cx| this.on_digit_key("1", window, cx)))
            .on_action(cx.listener(|this, _: &McDigit2, window, cx| this.on_digit_key("2", window, cx)))
            .on_action(cx.listener(|this, _: &McDigit3, window, cx| this.on_digit_key("3", window, cx)))
            .on_action(cx.listener(|this, _: &McDigit4, window, cx| this.on_digit_key("4", window, cx)))
            .on_action(cx.listener(|this, _: &McDigit5, window, cx| this.on_digit_key("5", window, cx)))
            .on_action(cx.listener(|this, _: &McDigit6, window, cx| this.on_digit_key("6", window, cx)))
            .on_action(cx.listener(|this, _: &McDigit7, window, cx| this.on_digit_key("7", window, cx)))
            .on_action(cx.listener(|this, _: &McDigit8, window, cx| this.on_digit_key("8", window, cx)))
            .on_action(cx.listener(|this, _: &McDigit9, window, cx| this.on_digit_key("9", window, cx)))
            .on_action(cx.listener(|this, _: &QuestionMoveUp, window, cx| {
                if this.actions_menu_focused(window, cx) || this.response_text_editing() {
                    cx.propagate();
                    return;
                }
                match this.workspace_focus {
                    WorkspaceFocus::QuestionList => this.move_question_list_by(-1, window, cx),
                    WorkspaceFocus::Response(_) => this.move_response_focus(-1, window, cx),
                }
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &QuestionMoveDown, window, cx| {
                if this.actions_menu_focused(window, cx) || this.response_text_editing() {
                    cx.propagate();
                    return;
                }
                match this.workspace_focus {
                    WorkspaceFocus::QuestionList => this.move_question_list_by(1, window, cx),
                    WorkspaceFocus::Response(_) => this.move_response_focus(1, window, cx),
                }
                cx.stop_propagation();
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
                match this.workspace_focus {
                    WorkspaceFocus::QuestionList => this.focus_response_right(window, cx),
                    WorkspaceFocus::Response(_) => this.focus_obligations_right(window, cx),
                }
                cx.stop_propagation();
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
                if this.actions_menu_focused(window, cx) || this.response_text_editing() {
                    cx.propagate();
                    return;
                }
                this.activate_focused(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(on_app_nav_toggle::<Self>))
            .on_action(cx.listener(|_, _: &NavigateBack, _, cx| {
                cx.emit(WorkspaceEvent::NavigateBack);
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
                                        self.complete,
                                        self.loaded_once,
                                        wait,
                                        selected.as_ref(),
                                        summary,
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
                                        selected.as_ref(),
                                        &self.selected_mc,
                                        &self.proposed_input,
                                        &self.notes_input,
                                        &self.feedback_input,
                                        &self.freeform_input,
                                        self.can_mutate(),
                                        self.has_proposed_editor(),
                                        self.workspace_focus,
                                        self.proposed_editing,
                                        self.notes_editing,
                                        self.feedback_editing,
                                        self.freeform_editing,
                                        self.actions_menu_open,
                                        self.actions_menu.clone(),
                                        muted,
                                    )),
                            )
                            .child(
                                resizable_panel()
                                    .size(px(OBLIGATIONS_COLUMN_WIDTH))
                                    .size_range(px(OBLIGATIONS_COLUMN_MIN)..Pixels::MAX)
                                    .child(if self.phase == PHASE_PLANNING {
                                        self.plan_steps.clone().into_any_element()
                                    } else {
                                        self.obligations.clone().into_any_element()
                                    }),
                            ),
                    ),
            )
            .child(status_footer(
                cx,
                window,
                &status_text,
                border,
                muted,
                show_retry,
                self.driver_status.question_maker_running || self.driver_status.answers_in_flight > 0,
            ))
    }
}

/// Waiting UI while the question maker writes the first questions.
#[derive(Debug, Clone)]
struct QuestionMakerWaitUi {
    open: usize,
    target: usize,
    elapsed_secs: u64,
    animate_dots: usize,
}

fn archived_banner(border: gpui::Hsla, muted: gpui::Hsla) -> impl IntoElement {
    div()
        .px_4()
        .py_2()
        .border_b_1()
        .border_color(border)
        .text_sm()
        .text_color(muted)
        .child("Archived — answering and agent work are paused")
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

#[allow(clippy::too_many_arguments)]
fn body_column(
    cx: &mut Context<WorkspaceView>,
    window: &mut Window,
    complete: bool,
    loaded_once: bool,
    wait: Option<QuestionMakerWaitUi>,
    question: Option<&InterviewQuestion>,
    proposal_summary: Option<String>,
    session: &InterviewSession,
    show_proceed: bool,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let body = if complete {
        complete_body(window, cx, session, show_proceed, foreground, muted).into_any_element()
    } else if let Some(q) = question {
        question_body_view(q, proposal_summary, foreground, muted, window, cx).into_any_element()
    } else if let Some(wait) = wait {
        question_maker_waiting_body(wait, foreground, muted).into_any_element()
    } else if !loaded_once {
        // Avoid a "No open questions" flash before the first reload has had a
        // chance to report the driver's actual status.
        div().into_any_element()
    } else {
        div()
            .text_sm()
            .text_color(muted)
            .child("No open questions")
            .into_any_element()
    };
    v_flex()
        .id("body-column-scroll")
        .size_full()
        .min_w_0()
        .overflow_y_scrollbar()
        .p_4()
        .gap_4()
        .child(v_flex().w_full().gap_4().child(body))
        .child(
            div().flex_none().pt_2().child(
                Button::new("copy-question-source")
                    .label("Copy raw source")
                    .compact()
                    .disabled(question.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.copy_question_source(cx))),
            ),
        )
}

fn question_body_view(
    q: &InterviewQuestion,
    proposal_summary: Option<String>,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let mut col = v_flex().w_full().min_w_0().gap_3();
    let text = |id: String, body: String, color: gpui::Hsla, bold: bool, window: &mut Window, cx: &mut App| {
        let el = selectable_text(SharedString::from(id), SharedString::from(body), window, cx)
            .w_full()
            .min_w_0()
            .text_sm()
            .text_color(color);
        if bold { el.font_semibold() } else { el }
    };
    if let Some(context) = q.context.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        col = col.child(text(format!("question-context-{}", q.seq), context.into(), muted, false, window, cx));
    }
    if let Some(question) = q.question.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        col = col.child(text(format!("question-text-{}", q.seq), question.into(), foreground, true, window, cx));
    }
    if let Some(summary) = proposal_summary {
        col = col.child(text(format!("question-proposal-{}", q.seq), summary, muted, false, window, cx));
    }
    col
}

fn complete_body(
    window: &mut Window,
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
        .child(
            div().text_sm().text_color(muted).child(
                selectable_text(
                    "complete-body-summary",
                    format!("No open questions remain for \"{}\".", session.display_name),
                    window,
                    cx,
                )
                .text_color(muted),
            ),
        );
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

#[allow(clippy::too_many_arguments)]
fn response_column(
    cx: &mut Context<WorkspaceView>,
    question: Option<&InterviewQuestion>,
    selected_mc: &Option<String>,
    proposed_input: &Entity<InputState>,
    notes_input: &Entity<InputState>,
    feedback_input: &Entity<InputState>,
    freeform_input: &Entity<InputState>,
    can_mutate: bool,
    show_proposed: bool,
    workspace_focus: WorkspaceFocus,
    proposed_editing: bool,
    notes_editing: bool,
    feedback_editing: bool,
    freeform_editing: bool,
    actions_menu_open: bool,
    actions_menu: Option<Entity<PopupMenu>>,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let disabled = !can_mutate || question.is_none();
    let proposed_input_disabled = disabled || !proposed_editing;
    let notes_input_disabled = disabled || !notes_editing;
    let feedback_input_disabled = disabled || !feedback_editing;
    // Freeform submission is independent of the selected question.
    let freeform_input_disabled = !can_mutate || !freeform_editing;
    let focused_idx = match workspace_focus {
        WorkspaceFocus::Response(i) => Some(i),
        WorkspaceFocus::QuestionList => None,
    };
    let col = v_flex()
        .id("response-column")
        .size_full()
        .min_w_0()
        .overflow_hidden()
        .p_3()
        .gap_2()
        .child(div().text_xs().text_color(muted).child("Response"));
    let mut scroll_body = v_flex().id("response-scroll-body").w_full().min_w_0().gap_2();
    let mut stop_idx = 0usize;
    if let Some(q) = question {
        let recommended_key = q.recommend.as_deref().map(str::trim).filter(|s| !s.is_empty());
        for (idx, label) in option_labels(q).into_iter().enumerate() {
            let key = (idx + 1).to_string();
            scroll_body = scroll_body.child(mc_option_row(
                cx,
                idx,
                key.clone(),
                label,
                selected_mc.as_ref().is_some_and(|k| k == &key),
                recommended_key == Some(key.as_str()),
                focused_idx == Some(stop_idx),
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
    let actions_focused = focused_idx == Some(stop_idx + 1);
    let submit_focused = focused_idx == Some(stop_idx + 2);
    let feedback_focused = focused_idx == Some(stop_idx + 3);
    let feedback_submit_focused = focused_idx == Some(stop_idx + 4);
    let freeform_focused = focused_idx == Some(stop_idx + 5);
    let freeform_submit_focused = focused_idx == Some(stop_idx + 6);
    let notes_view = cx.entity();
    let proposed_view = cx.entity();
    let feedback_view = cx.entity();
    let freeform_view = cx.entity();

    let mut response_body = v_flex()
        .id("response-body")
        .w_full()
        .min_w_0()
        .flex_none()
        .flex_shrink_0()
        .gap_2();
    if show_proposed {
        response_body = response_body
            .child(div().text_xs().text_color(muted).child("Proposed text (applied with option 1)"))
            .child(
                ListItem::new("proposed-field")
                    .selected(proposed_focused)
                    .w_full()
                    .h(px(TEXTAREA_HEIGHT))
                    .overflow_hidden()
                    .on_click(move |_, window, app| {
                        proposed_view.update(app, |this, cx| this.enter_proposed_edit(window, cx));
                    })
                    .child(
                        Input::new(proposed_input)
                            .disabled(proposed_input_disabled)
                            .w_full()
                            .h(px(TEXTAREA_HEIGHT)),
                    ),
            );
    }
    response_body = response_body
        .child(
            ListItem::new("notes-field")
                .selected(notes_focused)
                .w_full()
                .h(px(TEXTAREA_HEIGHT))
                .overflow_hidden()
                .on_click(move |_, window, app| {
                    notes_view.update(app, |this, cx| this.enter_notes_edit(window, cx));
                })
                .child(
                    Input::new(notes_input)
                        .disabled(notes_input_disabled)
                        .w_full()
                        .h(px(TEXTAREA_HEIGHT)),
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
                        .child(action_dropdown(cx, disabled, actions_menu_open, actions_focused, actions_menu)),
                )
                .child(
                    ListItem::new("submit-focus").selected(submit_focused).child(
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
                .h(px(TEXTAREA_HEIGHT))
                .overflow_hidden()
                .on_click(move |_, window, app| {
                    feedback_view.update(app, |this, cx| this.enter_feedback_edit(window, cx));
                })
                .child(
                    Input::new(feedback_input)
                        .disabled(feedback_input_disabled)
                        .w_full()
                        .h(px(TEXTAREA_HEIGHT)),
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

    let freeform_panel = v_flex()
        .id("response-freeform-panel")
        .w_full()
        .min_w_0()
        .flex_none()
        .flex_shrink_0()
        .gap_2()
        .pt_2()
        .border_t_1()
        .border_color(muted.opacity(0.25))
        .child(div().text_xs().text_color(muted).child("Tell the interview something"))
        .child(
            ListItem::new("freeform-field")
                .selected(freeform_focused)
                .w_full()
                .h(px(TEXTAREA_HEIGHT))
                .overflow_hidden()
                .on_click(move |_, window, app| {
                    freeform_view.update(app, |this, cx| this.enter_freeform_edit(window, cx));
                })
                .child(
                    Input::new(freeform_input)
                        .disabled(freeform_input_disabled)
                        .w_full()
                        .h(px(TEXTAREA_HEIGHT)),
                ),
        )
        .child(
            ListItem::new("freeform-submit-focus")
                .selected(freeform_submit_focused)
                .child(
                    Button::new("submit-freeform")
                        .label("Send")
                        .compact()
                        .disabled(!can_mutate)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                this.submit_freeform(window, cx);
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
            .child(
                scroll_body
                    .child(response_body)
                    .child(feedback_panel)
                    .child(freeform_panel),
            ),
    )
}

fn populate_action_menu(menu: PopupMenu, view: WeakEntity<WorkspaceView>) -> PopupMenu {
    OTHER_ACTION_ITEMS.iter().fold(menu, |menu, (action, label)| {
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
    key: String,
    label: String,
    selected: bool,
    recommended: bool,
    focused: bool,
    disabled: bool,
) -> impl IntoElement {
    let label: SharedString = format!("{key}. {label}").into();
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
                .justify_start()
                .w_full()
                .disabled(disabled)
                .selected(focused || selected)
                .when(recommended, |this| {
                    this.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().accent_foreground)
                            .child("Recommended"),
                    )
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    if !disabled {
                        this.submit_mc_option(&key, window, cx);
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
    show_retry: bool,
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
        .when(show_retry, |el| {
            el.child(
                Button::new("retry-interview-agents")
                    .label("Retry agents")
                    .on_click(cx.listener(|this, _, _, cx| this.retry_agents(cx))),
            )
        })
}

fn question_maker_waiting_body(
    wait: QuestionMakerWaitUi,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let dots = ".".repeat(wait.animate_dots.max(1));
    let mut col = v_flex().w_full().min_w_0().gap_3().child(
        div()
            .text_sm()
            .font_semibold()
            .text_color(foreground)
            .child(format!("Question maker is preparing questions{dots}")),
    );
    if wait.elapsed_secs >= 3 {
        col = col.child(
            div()
                .text_xs()
                .text_color(muted)
                .child(format!("Elapsed {}", format_elapsed(wait.elapsed_secs))),
        );
    }
    if wait.target > 0 {
        let pct = (wait.open as f32 / wait.target as f32).clamp(0., 1.);
        col = col.child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("Queue: {} / {}", wait.open, wait.target)),
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
    col.child(
        div()
            .text_xs()
            .text_color(muted)
            .child("Questions appear here one by one as they are written."),
    )
}

fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

impl gpui::EventEmitter<WorkspaceEvent> for WorkspaceView {}

#[cfg(test)]
mod tests {
    use super::{edited_proposal_text, option_labels};
    use tod_store::interview::{InterviewQuestion, Proposal, ProposalOp};
    use uuid::Uuid;

    fn question(options: Vec<String>, proposal: Option<Proposal>) -> InterviewQuestion {
        InterviewQuestion {
            id: Uuid::nil(),
            node_id: Uuid::nil(),
            session_id: None,
            seq: 1,
            phase: "requirements".into(),
            author: "question-maker".into(),
            status: "open".into(),
            covers: Vec::new(),
            context: None,
            question: Some("Q?".into()),
            intent: None,
            recommend: None,
            options,
            proposal,
            answer_option: None,
            answer_text: None,
            answer_edited_text: None,
            applied: None,
            processed_at: None,
            processed_summary: None,
            withdrawn_by: None,
            withdrawn_reason: None,
            created_at: 0,
            answered_at: None,
            updated_at: 0,
        }
    }

    #[test]
    fn unedited_proposal_text_is_not_resent() {
        assert_eq!(edited_proposal_text(Some("Fleet uses SQLite."), " Fleet uses SQLite. "), None);
        assert_eq!(
            edited_proposal_text(Some("Fleet uses SQLite."), "Fleet uses SQLite under the root."),
            Some("Fleet uses SQLite under the root.".into())
        );
        assert_eq!(edited_proposal_text(None, "anything"), None);
    }

    #[test]
    fn a_bare_proposal_offers_accept() {
        let proposal = Proposal {
            op: ProposalOp::Add,
            kind: Some("requirement".into()),
            section: None,
            node: None,
            id: None,
            content_type: None,
            text: Some("X".into()),
            append: false,
            replaces: Vec::new(),
        };
        assert_eq!(option_labels(&question(Vec::new(), Some(proposal))), vec!["Accept"]);
        assert_eq!(
            option_labels(&question(vec!["A".into(), "B".into()], None)),
            vec!["A", "B"]
        );
    }
}
