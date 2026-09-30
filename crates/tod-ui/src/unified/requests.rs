//! Requests: everything a node is waiting on the user for, rendered and
//! answered in one place: the task panel (`doc/ui/task-panel.md`
//! "Requests").
//!
//! [`Requests`] is an entity a host panel embeds as a child. It loads
//! [`tod_core::attention::for_node`] (oldest first) off the UI thread and
//! reloads on every store change and every change to the shared
//! [`LifecycleController`]. Each kind answers through its existing code path,
//! never a new mutation: a decision through `AgentRuns::answer_decision`; a
//! plan step through `AgentRuns::answer_plan_step_handoff`; a finding through
//! `AgentRuns::respond_review_finding`; a gate criterion through
//! `LifecycleController::waive`. Every answer records a journey `UserAction`
//! with a `Presented` snapshot of the choices shown.
//!
//! Keys (digits answer the top request, Up/Down move a stop across the
//! current decision's evidence links and freeform field, Enter/Ctrl+Enter
//! activate it, Escape leaves the freeform field) are bound in
//! [`REQUESTS_CONTEXT`]: the host puts that key context on its focused root
//! and forwards the actions with [`bind_request_actions`].
//!
//! Each request ends with one footer line: its evidence links (each item's
//! name), the reason ([`reason_label`]), and at the right **Shouldn't have
//! asked** (`doc/ui/task-panel.md`): one click records `should_not_ask`
//! feedback (`tod_store::request_feedback`) without answering or dismissing
//! the request, then offers a note and a switch to "bad question".
//!
//! The decision answer log with **Change** is here too
//! ([`Requests::render_log_entries`]), for the task panel's Answered drawer.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    Stateful, Styled, Subscription, Window, actions, div,
    prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Disableable, IconName, Sizable};
use tod_core::attention::{AttentionItem, AttentionKind, RequestReason};
use tod_core::conversation::implement::HandoffAnswer;
use tod_journey::{Presented, PresentedAction};
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::decisions::{DECISION_PENDING, Decision, DecisionAnswer, DecisionRepo, EvidenceRef};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_USER, InterviewCommand, short_id};
use tod_store::outline::PlanStep;
use tod_store::outline::repos::plan_steps::HandoffReason;
use tod_store::outline::repos::{ObligationRepo, PlanStepRepo};
use tod_store::request_feedback::{
    KIND_DECISION, KIND_PLAN_STEP_HANDOFF, KIND_REVIEW_FINDING, NewRequestFeedback,
    VERDICT_BAD_QUESTION, VERDICT_SHOULD_NOT_ASK,
};
use tod_store::review::{FINDING_DECLINED, FINDING_FIXED, FINDING_OUT_OF_SCOPE, ReviewFinding, ReviewRepo};
use uuid::Uuid;

use crate::ui::agent_runs::AgentRuns;
use crate::ui::journey::{Source, record_action};
use crate::ui::key_context;
use crate::ui::selectable_text::{selectable_markdown, selectable_text};
use crate::ui::style;
use crate::ui::terminal_handoff::{self, CONTINUE_IN_TERMINAL};
use crate::unified::columns::PanelKind;
use crate::unified::panel::PanelOpenRequest;
use crate::views::lifecycle_control::LifecycleController;

/// The key context a host panel puts on its focused root so the request keys
/// apply there.
pub const REQUESTS_CONTEXT: &str = "UnifiedRequests";
/// Key-context tag for whichever freeform answer field is in edit mode, so
/// Escape only fires for that one field (`key_context::including_tag`).
const FREEFORM_TAG: &str = "RequestsFreeform";
/// Key-context tag for the feedback note field while it is in edit mode.
const FEEDBACK_NOTE_TAG: &str = "RequestsFeedbackNote";
/// The journey surface every answer is recorded under; the name "decisions"
/// is kept so earlier journeys still compare.
const JOURNEY_SURFACE: &str = "decisions";
/// The journey action id of **Answered elsewhere**.
const RESOLVED_ELSEWHERE_ACTION: &str = "answered-elsewhere";

/// Answer the top request with option `.0` (1-based, matching the numbered
/// options shown).
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = unified_requests, no_json)]
pub struct DecisionOptionKey(pub usize);

actions!(
    unified_requests,
    [
        DecisionsActivate,
        DecisionsCtrlActivate,
        DecisionsFreeformEscape,
        DecisionsLinkPrev,
        DecisionsLinkNext,
    ]
);

/// Registers the request keys in [`REQUESTS_CONTEXT`]. Call once alongside
/// `unified::register_unified_keyboard_bindings`.
pub fn register_request_keyboard_bindings(cx: &mut App) {
    let outside_input = Some(key_context::excluding_input(REQUESTS_CONTEXT));
    let with_input = Some(key_context::including_tag(REQUESTS_CONTEXT, FREEFORM_TAG));
    cx.bind_keys((1..=9).map(|n| KeyBinding::new(&n.to_string(), DecisionOptionKey(n as usize), outside_input)));
    cx.bind_keys([
        KeyBinding::new("enter", DecisionsActivate, outside_input),
        KeyBinding::new("ctrl-enter", DecisionsCtrlActivate, outside_input),
        KeyBinding::new("escape", DecisionsFreeformEscape, with_input),
        KeyBinding::new(
            "escape",
            DecisionsFreeformEscape,
            Some(key_context::including_tag(REQUESTS_CONTEXT, FEEDBACK_NOTE_TAG)),
        ),
        KeyBinding::new("up", DecisionsLinkPrev, outside_input),
        KeyBinding::new("down", DecisionsLinkNext, outside_input),
    ]);
}

/// Puts [`REQUESTS_CONTEXT`] on a host panel's focused root and forwards the
/// request actions to `requests`.
pub fn bind_request_actions(el: Stateful<gpui::Div>, requests: &Entity<Requests>) -> Stateful<gpui::Div> {
    let r1 = requests.clone();
    let r2 = requests.clone();
    let r3 = requests.clone();
    let r4 = requests.clone();
    let r5 = requests.clone();
    let r6 = requests.clone();
    el.key_context(REQUESTS_CONTEXT)
        .on_action(move |a: &DecisionOptionKey, window, cx| {
            r1.update(cx, |r, cx| r.answer_option_key(a, window, cx))
        })
        .on_action(move |_: &DecisionsActivate, window, cx| r2.update(cx, |r, cx| r.activate(false, window, cx)))
        .on_action(move |_: &DecisionsCtrlActivate, window, cx| r3.update(cx, |r, cx| r.activate(true, window, cx)))
        .on_action(move |_: &DecisionsFreeformEscape, window, cx| r4.update(cx, |r, cx| r.exit_freeform_edit(window, cx)))
        .on_action(move |_: &DecisionsLinkPrev, _, cx| r5.update(cx, |r, cx| r.link_prev(cx)))
        .on_action(move |_: &DecisionsLinkNext, _, cx| r6.update(cx, |r, cx| r.link_next(cx)))
}

/// The human label for a request's reason, shown on its footer line.
pub fn reason_label(reason: RequestReason) -> &'static str {
    match reason {
        RequestReason::Intent => "Needs your input",
        RequestReason::MissingRule => "No rule covers this",
        RequestReason::Conflict => "Conflicting requirements",
        RequestReason::Access => "Needs access",
        RequestReason::Risk => "Needs your sign-off",
        RequestReason::Capability => "Capability setting",
        RequestReason::Other => "Other",
    }
}

/// One answer in the append-only log: the decision it answered (as it was
/// at load time) and one of its `decision_answers` rows.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub decision: Decision,
    pub answer: DecisionAnswer,
}

#[derive(Default)]
pub struct Loaded {
    pub pending: Vec<Decision>,
    /// Every plan step currently handed back to the user.
    pub handoff_steps: Vec<PlanStep>,
    /// Every open review finding while the node is in `review`.
    pub findings: Vec<ReviewFinding>,
    /// What the node is waiting on, oldest first.
    pub items: Vec<AttentionItem>,
    /// Oldest answer first.
    pub log: Vec<LogEntry>,
    /// Display names for evidence ids (obligations, plan steps, nodes,
    /// findings), for the footer links.
    pub names: HashMap<Uuid, String>,
    /// Per request id: the lifecycle conversation that asked it, when that
    /// conversation has an agent session a terminal can resume.
    pub sessions: HashMap<Uuid, Uuid>,
}

/// The conversation `item` came from: a decision's or finding's own, or the
/// implement/verify conversation a plan step was handed back by.
fn asking_conversation(
    conn: &rusqlite::Connection,
    item: &AttentionItem,
    pending: &[Decision],
    findings: &[ReviewFinding],
) -> anyhow::Result<Option<Uuid>> {
    Ok(match item.kind {
        AttentionKind::Decision => pending.iter().find(|d| d.id == item.id).and_then(|d| d.conversation_id),
        AttentionKind::PlanStep => {
            crate::ui::agent_runs::latest_handoff_conversation(conn, Focus::Node(item.node_id))?.map(|c| c.id)
        }
        AttentionKind::Finding => findings.iter().find(|f| f.id == item.id).and_then(|f| f.conversation_id),
    })
}

fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut out: String = line.chars().take(48).collect();
    if line.chars().count() > 48 {
        out.push('…');
    }
    out
}

pub fn load(fleet: &FleetStore, node_id: Uuid) -> Loaded {
    let mut loaded = fleet
        .read(|conn| {
            let repo = DecisionRepo::new(conn);
            let pending = repo.list_pending_for_node(node_id)?;
            let mut log = Vec::new();
            for decision in repo.list_for_node(node_id)? {
                if decision.status == DECISION_PENDING {
                    continue;
                }
                if let Some(with_answers) = repo.get_with_answers(decision.id)? {
                    for answer in with_answers.answers {
                        log.push(LogEntry { decision: with_answers.decision.clone(), answer });
                    }
                }
            }
            log.sort_by_key(|entry| entry.answer.answered_at);

            let handoff_steps: Vec<PlanStep> = PlanStepRepo::new(conn)
                .list_needs_user_for_nodes(&[node_id])?
                .into_iter()
                .map(|(step, _)| step)
                .collect();
            let lifecycle = tod_store::outline::repos::NodeRepo::new(conn).get_lifecycle_for_nodes(&[node_id])?;
            let findings = if lifecycle.get(&node_id).map(String::as_str) == Some("review") {
                ReviewRepo::new(conn).list_open_for_nodes(&[node_id])?
            } else {
                Vec::new()
            };
            let items = tod_core::attention::for_node(conn, node_id)?.items;

            let mut sessions = HashMap::new();
            let conversations = ConversationRepo::new(conn);
            for item in &items {
                let Some(conversation) = asking_conversation(conn, item, &pending, &findings)? else {
                    continue;
                };
                let resumable = conversations
                    .get(conversation)?
                    .is_some_and(|c| c.agent_session_id.is_some_and(|s| !s.trim().is_empty()));
                if resumable {
                    sessions.insert(item.id, conversation);
                }
            }

            let mut names = HashMap::new();
            for step in &handoff_steps {
                names.insert(step.id, first_line(&step.body));
            }
            for finding in &findings {
                names.insert(finding.id, first_line(&finding.summary));
            }
            for evidence in pending.iter().chain(log.iter().map(|e| &e.decision)).flat_map(|d| &d.evidence) {
                if names.contains_key(&evidence.id) {
                    continue;
                }
                let name = match evidence.kind.as_str() {
                    "obligation" => ObligationRepo::new(conn).get(evidence.id)?.map(|o| first_line(&o.body)),
                    "plan_step" => PlanStepRepo::new(conn).get(evidence.id)?.map(|s| first_line(&s.body)),
                    _ => None,
                };
                if let Some(name) = name {
                    names.insert(evidence.id, name);
                }
            }
            anyhow::Ok(Loaded { pending, handoff_steps, findings, items, log, names, sessions })
        })
        .unwrap_or_default();
    let node_ids: Vec<Uuid> = loaded
        .pending
        .iter()
        .flat_map(|d| &d.evidence)
        .filter(|e| e.kind == "node" && !loaded.names.contains_key(&e.id))
        .map(|e| e.id)
        .collect();
    for id in node_ids {
        if let Ok(Some(node)) = fleet.get_node(&id.to_string()) {
            loaded.names.insert(id, first_line(&node.title));
        }
    }
    loaded
}

/// Where an evidence reference opens — `None` for kinds with no panel of
/// their own (`test_run`), shown as plain text instead.
pub fn evidence_target(node_id: Uuid, evidence: &EvidenceRef) -> Option<PanelKind> {
    match evidence.kind.as_str() {
        "obligation" => Some(PanelKind::Obligations(node_id)),
        "plan_step" => Some(PanelKind::Plan(node_id)),
        "conversation" => Some(PanelKind::Transcript(evidence.id)),
        "node" => Some(PanelKind::Details(evidence.id)),
        "finding" => Some(PanelKind::Findings(node_id)),
        _ => None,
    }
}

/// One clickable (or plain) evidence entry on a request's footer line.
pub struct EvidenceLink {
    pub label: SharedString,
    pub target: Option<PanelKind>,
}

fn evidence_label(evidence: &EvidenceRef, names: &HashMap<Uuid, String>) -> String {
    if let Some(name) = names.get(&evidence.id).filter(|n| !n.is_empty()) {
        return name.clone();
    }
    match evidence.kind.as_str() {
        "conversation" => "Conversation".to_string(),
        "test_run" => "Test run".to_string(),
        kind => format!("{kind} {}", short_id(evidence.id)),
    }
}

fn evidence_links(node_id: Uuid, evidence: &[EvidenceRef], names: &HashMap<Uuid, String>) -> Vec<EvidenceLink> {
    evidence
        .iter()
        .map(|e| EvidenceLink { label: evidence_label(e, names).into(), target: evidence_target(node_id, e) })
        .collect()
}

/// "Shouldn't have asked" feedback given on one request in this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackState {
    /// The `request_feedback` row, once the first write has landed.
    pub id: Option<Uuid>,
    pub verdict: &'static str,
    pub note: Option<String>,
}

pub struct Requests {
    pub(crate) node_id: Option<Uuid>,
    fleet: Arc<FleetStore>,
    agent_runs: Entity<AgentRuns>,
    /// The one lifecycle controller shared with the conversation view and
    /// the lifecycle panel: gate criteria and `waive` live there.
    pub(crate) lifecycle: Entity<LifecycleController>,
    /// The host panel's focus handle, refocused when the freeform field is
    /// left.
    host_focus: FocusHandle,
    pub(crate) loaded: Loaded,
    /// Bumped on every reload so a slower, older load never overwrites a
    /// newer one.
    generation: u64,
    freeform_editing: Option<Uuid>,
    freeform_input: Entity<InputState>,
    /// A log entry's decision the user asked to change.
    pub(crate) changing: Option<Uuid>,
    /// Index of the keyboard stop on the current decision.
    selected_link: usize,
    pub(crate) last_error: Option<String>,
    /// Feedback given per request id.
    feedback: HashMap<Uuid, FeedbackState>,
    /// The request whose feedback note field is in edit mode.
    feedback_note_editing: Option<Uuid>,
    feedback_note_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
    _poll: gpui::Task<()>,
}

impl EventEmitter<PanelOpenRequest> for Requests {}

impl Requests {
    pub fn new(
        node_id: Option<Uuid>,
        fleet: Arc<FleetStore>,
        agent_runs: Entity<AgentRuns>,
        lifecycle: Entity<LifecycleController>,
        host_focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let freeform_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Freeform answer… (Enter to submit)"));
        let freeform_sub = cx.subscribe(&freeform_input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit_freeform(cx);
            }
        });
        let feedback_note_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Why? (Enter to save)"));
        let note_sub = cx.subscribe_in(&feedback_note_input, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_feedback_note(cx);
                this.host_focus.focus(window, cx);
            }
        });
        // A gate criterion waived elsewhere shows here too: one controller.
        let lifecycle_sub = cx.observe(&lifecycle, |this, _, cx| this.reload_data(cx));
        let fleet_for_poll = fleet.clone();
        let _poll = cx.spawn(async move |this, cx| {
            let mut rx = fleet_for_poll.subscribe_changes();
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
                let mut changed = false;
                while rx.try_recv().is_ok() {
                    changed = true;
                }
                if changed && this.update(cx, |this: &mut Requests, cx| this.reload_data(cx)).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            node_id,
            fleet,
            agent_runs,
            lifecycle,
            host_focus,
            loaded: Loaded::default(),
            generation: 0,
            freeform_editing: None,
            freeform_input,
            changing: None,
            selected_link: 0,
            last_error: None,
            feedback: HashMap::new(),
            feedback_note_editing: None,
            feedback_note_input,
            _subscriptions: vec![freeform_sub, lifecycle_sub, note_sub],
            _poll,
        };
        this.reload(cx);
        this
    }

    #[allow(dead_code)] // for hosts (T5, T6) and tests.
    pub fn items(&self) -> &[AttentionItem] {
        &self.loaded.items
    }

    pub fn log(&self) -> &[LogEntry] {
        &self.loaded.log
    }

    /// Retarget to a different node.
    pub fn set_node(&mut self, node_id: Option<Uuid>, cx: &mut Context<Self>) {
        if self.node_id == node_id {
            return;
        }
        self.node_id = node_id;
        self.freeform_editing = None;
        self.feedback_note_editing = None;
        self.changing = None;
        self.selected_link = 0;
        self.loaded = Loaded::default();
        self.reload(cx);
    }

    /// Reload the lifecycle controller's persisted criteria and the requests.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.node_id {
            self.lifecycle.update(cx, |controller, _| controller.load_persisted(&id.to_string()));
        }
        self.reload_data(cx);
    }

    /// Reload the requests off the UI thread.
    fn reload_data(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let Some(node_id) = self.node_id else {
            self.loaded = Loaded::default();
            cx.notify();
            return;
        };
        let fleet = self.fleet.clone();
        cx.spawn(async move |this, cx| {
            let loaded = cx.background_executor().spawn(async move { load(&fleet, node_id) }).await;
            let _ = this.update(cx, |this: &mut Requests, cx| {
                if this.generation == generation {
                    this.apply(loaded, cx);
                }
            });
        })
        .detach();
    }

    fn apply(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        self.loaded = loaded;
        if let Some(changing) = self.changing
            && !self.loaded.log.iter().any(|e| e.decision.id == changing)
        {
            self.changing = None;
        }
        if let Some(editing) = self.freeform_editing {
            let still_open =
                self.loaded.pending.iter().any(|d| d.id == editing) || self.changing == Some(editing);
            if !still_open {
                self.freeform_editing = None;
            }
        }
        cx.notify();
    }

    fn set_error(&mut self, what: &str, result: anyhow::Result<()>) {
        self.last_error = result.err().map(|err| {
            tracing::warn!("requests: failed to {what}: {err:#}");
            format!("{err:#}")
        });
    }

    /// `Presented` snapshot for one decision's on-screen options.
    fn presented_for(decision: &Decision) -> Presented {
        Presented {
            actions: decision
                .options
                .iter()
                .enumerate()
                .map(|(ix, label)| PresentedAction {
                    id: (ix + 1).to_string(),
                    label: label.clone(),
                    primary: ix == 0,
                    disabled: false,
                })
                .collect(),
            focused: None,
            notices: vec![decision.question.clone()],
        }
    }

    pub(crate) fn answer(
        &mut self,
        decision: Decision,
        option: Option<usize>,
        text: Option<String>,
        source: Source,
        cx: &mut Context<Self>,
    ) {
        let chosen = option.map(|o| o.to_string()).unwrap_or_else(|| "freeform".to_string());
        record_action(
            cx,
            tod_store::conversation::Focus::Node(decision.node_id),
            chosen,
            source,
            JOURNEY_SURFACE,
            Self::presented_for(&decision),
        );
        let decision_id = decision.id;
        let result = self.agent_runs.update(cx, |runs, cx| runs.answer_decision(decision_id, option, text, cx));
        self.set_error("record answer", result.map(|_| ()));
        self.freeform_editing = None;
        self.changing = None;
        self.reload(cx);
    }

    /// Answer the *top* request with option `n` (1-based), when it has
    /// options (a decision, or a plan step handed back as a decision).
    pub(crate) fn answer_option_key(&mut self, action: &DecisionOptionKey, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(top) = self.loaded.items.first().cloned() else {
            return;
        };
        if action.0 == 0 || action.0 > top.options.len() {
            return;
        }
        match top.kind {
            AttentionKind::Decision => {
                if let Some(decision) = self.loaded.pending.iter().find(|d| d.id == top.id).cloned() {
                    // "I've provided it" is the dialog's Save, never a bare pick.
                    if tod_core::environment_request::is_request(&decision) && action.0 == 1 {
                        self.provide_credential(&decision, _window, cx);
                    } else {
                        self.answer(decision, Some(action.0), None, Source::Keyboard, cx);
                    }
                }
            }
            AttentionKind::PlanStep => {
                if let Some(step) = self.loaded.handoff_steps.iter().find(|s| s.id == top.id).cloned() {
                    self.answer_plan_step(&step, HandoffAnswer::Choose(action.0 - 1), Source::Keyboard, cx);
                }
            }
            AttentionKind::Finding => {}
        }
    }

    /// Open the dialog that takes a requested credential's value.
    fn provide_credential(&mut self, decision: &Decision, window: &mut Window, cx: &mut Context<Self>) {
        let ctx = crate::ui::credential_request::Ctx {
            fleet: self.fleet.clone(),
            data_root: self.fleet.paths().root().to_path_buf(),
            agent_runs: self.agent_runs.clone(),
        };
        crate::ui::credential_request::open(window, cx, ctx, decision.clone());
    }

    /// The card's buttons for a credential request: Provide opens the dialog
    /// (the value is typed there, never in the request), and the other answers
    /// that it cannot be provided.
    fn render_credential_buttons(&self, decision: &Decision, cx: &mut Context<Self>) -> impl IntoElement {
        let (provide, decline) = (decision.clone(), decision.clone());
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .child(
                Button::new(SharedString::from(format!("unified-decisions-provide-{}", decision.id)))
                    .label("1. Provide…")
                    .small()
                    .on_click(cx.listener(move |this, _, window, cx| this.provide_credential(&provide, window, cx))),
            )
            .child(
                Button::new(SharedString::from(format!("unified-decisions-decline-{}", decision.id)))
                    .label("2. I can't provide it")
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| this.click_option(decline.clone(), 2, cx))),
            )
    }

    pub(crate) fn click_option(&mut self, decision: Decision, option: usize, cx: &mut Context<Self>) {
        self.answer(decision, Some(option), None, Source::Click, cx);
    }

    fn presented_for_step(step: &PlanStep) -> Presented {
        let actions = match &step.reason {
            Some(HandoffReason::Decision { options }) => options
                .iter()
                .enumerate()
                .map(|(ix, label)| PresentedAction {
                    id: ix.to_string(),
                    label: label.clone(),
                    primary: ix == 0,
                    disabled: false,
                })
                .collect(),
            Some(HandoffReason::Conflict { obligations }) => obligations
                .iter()
                .map(|id| PresentedAction {
                    id: id.to_string(),
                    label: format!("Keep {}", short_id(*id)),
                    primary: false,
                    disabled: false,
                })
                .collect(),
            Some(HandoffReason::Access { .. }) | None => vec![PresentedAction {
                id: "retry".to_string(),
                label: "Retry".to_string(),
                primary: true,
                disabled: false,
            }],
        };
        Presented {
            actions,
            focused: None,
            notices: vec![step.reason.as_ref().map(HandoffReason::label).unwrap_or("").to_string()],
        }
    }

    /// Answer a plan step the agent handed back.
    pub(crate) fn answer_plan_step(&mut self, step: &PlanStep, answer: HandoffAnswer, source: Source, cx: &mut Context<Self>) {
        record_action(
            cx,
            tod_store::conversation::Focus::Node(step.node_id),
            format!("{answer:?}"),
            source,
            JOURNEY_SURFACE,
            Self::presented_for_step(step),
        );
        let step_id = step.id;
        let result = self.agent_runs.update(cx, |runs, cx| runs.answer_plan_step_handoff(step_id, answer, cx));
        self.set_error("answer plan step", result.map(|_| ()));
        self.reload(cx);
    }

    fn presented_for_finding(finding: &ReviewFinding) -> Presented {
        Presented {
            actions: [FINDING_FIXED, FINDING_OUT_OF_SCOPE, FINDING_DECLINED]
                .into_iter()
                .map(|status| PresentedAction {
                    id: status.to_string(),
                    label: status.to_string(),
                    primary: status == FINDING_FIXED,
                    disabled: false,
                })
                .collect(),
            focused: None,
            notices: vec![finding.summary.clone()],
        }
    }

    /// Answer an open review finding.
    pub(crate) fn answer_finding(&mut self, finding: &ReviewFinding, status: &str, source: Source, cx: &mut Context<Self>) {
        record_action(
            cx,
            tod_store::conversation::Focus::Node(finding.node_id),
            status.to_string(),
            source,
            JOURNEY_SURFACE,
            Self::presented_for_finding(finding),
        );
        let finding_id = finding.id;
        let status = status.to_string();
        let result = self.agent_runs.update(cx, |runs, _| runs.respond_review_finding(finding_id, &status));
        self.set_error("answer finding", result.map(|_| ()));
        self.reload(cx);
    }

    /// The feedback given on request `id` in this session, if any.
    #[allow(dead_code)] // for tests.
    pub fn feedback_for(&self, id: Uuid) -> Option<&FeedbackState> {
        self.feedback.get(&id)
    }

    /// What a feedback row records about `item`: its kind, and the
    /// conversation and protocol that asked when the request has them.
    fn new_feedback(&self, item: &AttentionItem) -> NewRequestFeedback {
        let (kind, conversation_id, protocol) = match item.kind {
            AttentionKind::Decision => {
                let decision = self.loaded.pending.iter().find(|d| d.id == item.id);
                (KIND_DECISION, decision.and_then(|d| d.conversation_id), decision.and_then(|d| d.protocol.clone()))
            }
            AttentionKind::PlanStep => (KIND_PLAN_STEP_HANDOFF, None, None),
            AttentionKind::Finding => (
                KIND_REVIEW_FINDING,
                self.loaded.findings.iter().find(|f| f.id == item.id).and_then(|f| f.conversation_id),
                None,
            ),
        };
        NewRequestFeedback {
            node_id: item.node_id,
            request_kind: kind.to_string(),
            request_id: item.id,
            reason: item.reason.as_str().to_string(),
            conversation_id,
            protocol,
            verdict: VERDICT_SHOULD_NOT_ASK.to_string(),
            note: None,
        }
    }

    /// Run one feedback write on the fleet writer off the UI thread; `done`
    /// gets its result back on it.
    fn write_feedback(
        &self,
        command: InterviewCommand,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, anyhow::Result<serde_json::Value>, &mut Context<Self>) + 'static,
    ) {
        let fleet = self.fleet.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { fleet.interview(ACTOR_USER, command).map_err(anyhow::Error::from) })
                .await;
            let _ = this.update(cx, |this: &mut Requests, cx| {
                done(this, result, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// **Shouldn't have asked**: record `should_not_ask` feedback on `item`.
    /// The request itself stays pending.
    pub(crate) fn should_not_have_asked(&mut self, item: &AttentionItem, source: Source, cx: &mut Context<Self>) {
        if self.feedback.contains_key(&item.id) {
            return;
        }
        record_action(
            cx,
            tod_store::conversation::Focus::Node(item.node_id),
            VERDICT_SHOULD_NOT_ASK.to_string(),
            source,
            JOURNEY_SURFACE,
            Presented {
                actions: vec![PresentedAction {
                    id: VERDICT_SHOULD_NOT_ASK.to_string(),
                    label: "Shouldn't have asked".to_string(),
                    primary: false,
                    disabled: false,
                }],
                focused: None,
                notices: vec![item.summary.clone()],
            },
        );
        let request_id = item.id;
        self.feedback.insert(request_id, FeedbackState { id: None, verdict: VERDICT_SHOULD_NOT_ASK, note: None });
        let feedback = self.new_feedback(item);
        cx.notify();
        self.write_feedback(InterviewCommand::RecordRequestFeedback(feedback), cx, move |this, result, cx| {
            let id = result
                .as_ref()
                .ok()
                .and_then(|v| v.get("id")?.as_str().and_then(|raw| Uuid::parse_str(raw).ok()));
            let Some(id) = id else {
                this.feedback.remove(&request_id);
                this.set_error("record feedback", result.map(|_| ()));
                return;
            };
            let Some(state) = this.feedback.get_mut(&request_id) else { return };
            state.id = Some(id);
            // A switch or note made while the first write was in flight.
            if state.verdict != VERDICT_SHOULD_NOT_ASK || state.note.is_some() {
                this.save_feedback(request_id, cx);
            }
        });
    }

    /// Write the feedback on `request_id` as it now stands. Before its row
    /// exists, the first write saves it when it lands.
    fn save_feedback(&mut self, request_id: Uuid, cx: &mut Context<Self>) {
        cx.notify();
        let Some(FeedbackState { id: Some(id), verdict, note }) = self.feedback.get(&request_id).cloned() else {
            return;
        };
        let command = InterviewCommand::UpdateRequestFeedback { id, verdict: verdict.to_string(), note };
        self.write_feedback(command, cx, |this, result, _| this.set_error("update feedback", result.map(|_| ())));
    }

    /// Switch the feedback on `request_id` between "shouldn't have asked"
    /// and "bad question".
    pub(crate) fn toggle_bad_question(&mut self, request_id: Uuid, cx: &mut Context<Self>) {
        let Some(state) = self.feedback.get_mut(&request_id) else { return };
        state.verdict =
            if state.verdict == VERDICT_BAD_QUESTION { VERDICT_SHOULD_NOT_ASK } else { VERDICT_BAD_QUESTION };
        self.save_feedback(request_id, cx);
    }

    fn begin_feedback_note_edit(&mut self, request_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let note = self.feedback.get(&request_id).and_then(|s| s.note.clone()).unwrap_or_default();
        self.feedback_note_editing = Some(request_id);
        self.freeform_editing = None;
        self.feedback_note_input.update(cx, |input, cx| input.set_value(note, window, cx));
        cx.notify();
        let input = self.feedback_note_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
    }

    /// Enter in the note field: save its text on the request being edited.
    fn commit_feedback_note(&mut self, cx: &mut Context<Self>) {
        let Some(request_id) = self.feedback_note_editing.take() else { return };
        let text = self.feedback_note_input.read(cx).text().to_string().trim().to_string();
        self.set_feedback_note(request_id, (!text.is_empty()).then_some(text), cx);
    }

    pub(crate) fn set_feedback_note(&mut self, request_id: Uuid, note: Option<String>, cx: &mut Context<Self>) {
        let Some(state) = self.feedback.get_mut(&request_id) else { return };
        state.note = note;
        self.save_feedback(request_id, cx);
    }

    /// The control at the right of a request's footer line.
    fn render_feedback(&self, item: &AttentionItem, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let request_id = item.id;
        let Some(state) = self.feedback.get(&request_id).cloned() else {
            let item = item.clone();
            return Button::new(SharedString::from(format!("unified-requests-should-not-ask-{request_id}")))
                .label("Shouldn't have asked")
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| this.should_not_have_asked(&item, Source::Click, cx)))
                .into_any_element();
        };
        let editing = self.feedback_note_editing == Some(request_id);
        key_context::set_input_tab_stop(&self.feedback_note_input, editing, cx);
        let bad = state.verdict == VERDICT_BAD_QUESTION;
        let recorded = if state.id.is_some() { "Feedback recorded" } else { "Recording feedback…" };
        let note: AnyElement = if editing {
            div()
                .id(SharedString::from(format!("unified-requests-feedback-note-{request_id}")))
                .key_context(FEEDBACK_NOTE_TAG)
                .w_48()
                .child(Input::new(&self.feedback_note_input).xsmall())
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap_1()
                .children(state.note.clone().map(|note| {
                    style::text_muted(div().text_xs()).child(selectable_text(
                        SharedString::from(format!("unified-requests-feedback-note-text-{request_id}")),
                        note,
                        window,
                        cx,
                    ))
                }))
                .child(
                    Button::new(SharedString::from(format!("unified-requests-feedback-note-edit-{request_id}")))
                        .label(if state.note.is_some() { "Edit note" } else { "Add note" })
                        .xsmall()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.begin_feedback_note_edit(request_id, window, cx)
                        })),
                )
                .into_any_element()
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .child(style::text_muted(div().text_xs()).child(recorded))
            .child(
                Button::new(SharedString::from(format!("unified-requests-feedback-bad-question-{request_id}")))
                    .label(if bad { "Bad question ✓" } else { "Bad question" })
                    .xsmall()
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_bad_question(request_id, cx))),
            )
            .child(note)
            .into_any_element()
    }

    fn begin_freeform_edit(&mut self, decision_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.freeform_editing = Some(decision_id);
        self.freeform_input.update(cx, |input, cx| input.set_value(String::new(), window, cx));
        cx.notify();
        let input = self.freeform_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
    }

    /// The decision the keyboard stops act on: the one being changed in the
    /// log, else the top pending one.
    fn current_decision(&self) -> Option<Decision> {
        if let Some(changing) = self.changing {
            self.loaded.log.iter().find(|e| e.decision.id == changing).map(|e| e.decision.clone())
        } else {
            self.loaded.pending.first().cloned()
        }
    }

    /// Enter/Ctrl+Enter on the current decision's stop: its evidence links,
    /// then its freeform field.
    pub(crate) fn activate(&mut self, ctrl: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(decision) = self.current_decision() else {
            return;
        };
        let links = evidence_links(decision.node_id, &decision.evidence, &self.loaded.names);
        if let Some(link) = links.get(self.selected_link) {
            if let Some(target) = link.target {
                self.open(target, ctrl, cx);
            }
            return;
        }
        self.begin_freeform_edit(decision.id, window, cx);
    }

    pub(crate) fn exit_freeform_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.freeform_editing = None;
        self.feedback_note_editing = None;
        self.host_focus.focus(window, cx);
        cx.notify();
    }

    fn submit_freeform(&mut self, cx: &mut Context<Self>) {
        let Some(decision_id) = self.freeform_editing else {
            return;
        };
        let text = self.freeform_input.read(cx).text().to_string().trim().to_string();
        if text.is_empty() {
            return;
        }
        let decision = self
            .loaded
            .pending
            .iter()
            .find(|d| d.id == decision_id)
            .or_else(|| self.loaded.log.iter().find(|e| e.decision.id == decision_id).map(|e| &e.decision))
            .cloned();
        let Some(decision) = decision else { return };
        self.answer(decision, None, Some(text), Source::Keyboard, cx);
    }

    /// Re-ask an already-answered decision from a log entry; the new answer
    /// is appended, nothing is reversed.
    pub(crate) fn start_change(&mut self, decision_id: Uuid, cx: &mut Context<Self>) {
        self.changing = Some(decision_id);
        self.freeform_editing = None;
        self.selected_link = 0;
        cx.notify();
    }

    pub(crate) fn cancel_change(&mut self, cx: &mut Context<Self>) {
        self.changing = None;
        self.freeform_editing = None;
        cx.notify();
    }

    fn open(&self, target: PanelKind, ctrl: bool, cx: &mut Context<Self>) {
        cx.emit(PanelOpenRequest { target, ctrl });
    }

    fn stop_count(&self) -> usize {
        self.current_decision()
            .map(|d| d.evidence.len() + 1)
            .unwrap_or(0)
    }

    pub(crate) fn link_prev(&mut self, cx: &mut Context<Self>) {
        self.selected_link = self.selected_link.saturating_sub(1);
        cx.notify();
    }

    pub(crate) fn link_next(&mut self, cx: &mut Context<Self>) {
        let max = self.stop_count().saturating_sub(1);
        self.selected_link = (self.selected_link + 1).min(max);
        cx.notify();
    }

    fn render_options(&self, decision: &Decision, cx: &mut Context<Self>) -> impl IntoElement {
        let decision = decision.clone();
        div().flex().flex_wrap().gap_1().children(decision.options.clone().into_iter().enumerate().map(
            |(ix, label)| {
                let option = ix + 1;
                let decision = decision.clone();
                Button::new(SharedString::from(format!("unified-decisions-option-{}-{option}", decision.id)))
                    .label(format!("{option}. {label}"))
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| this.click_option(decision.clone(), option, cx)))
            },
        ))
    }

    /// Evidence links for one request. `stops` marks them as the current
    /// decision's keyboard stops, highlighting the selected one.
    fn render_evidence_links(
        &self,
        key: Uuid,
        node_id: Uuid,
        evidence: &[EvidenceRef],
        stops: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let muted = cx.theme().muted_foreground;
        let accent = cx.theme().accent;
        evidence_links(node_id, evidence, &self.loaded.names)
            .into_iter()
            .enumerate()
            .map(|(ix, link)| {
                let selected = stops && ix == self.selected_link;
                match link.target {
                    Some(target) => div()
                        .id(SharedString::from(format!("unified-decisions-evidence-{key}-{ix}")))
                        .text_xs()
                        .when(selected, |el| el.text_color(accent))
                        .when(!selected, |el| el.text_color(muted))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                if stops {
                                    this.selected_link = ix;
                                }
                                let ctrl = event.modifiers.control || event.modifiers.platform;
                                this.open(target, ctrl, cx);
                            }),
                        )
                        .child(link.label)
                        .into_any_element(),
                    None => div().text_xs().text_color(muted).child(link.label).into_any_element(),
                }
            })
            .collect()
    }

    /// The evidence a request points at: a decision's own evidence, or the
    /// item a plan step, finding, or gate check is about.
    fn item_evidence(&self, item: &AttentionItem) -> Vec<EvidenceRef> {
        match item.kind {
            AttentionKind::Decision => self
                .loaded
                .pending
                .iter()
                .find(|d| d.id == item.id)
                .map(|d| d.evidence.clone())
                .unwrap_or_default(),
            AttentionKind::PlanStep => vec![EvidenceRef { kind: "plan_step".into(), id: item.id }],
            AttentionKind::Finding => vec![EvidenceRef { kind: "finding".into(), id: item.id }],
        }
    }

    /// The one footer line: evidence links, the reason, and "Shouldn't have asked".
    fn render_footer(&self, item: &AttentionItem, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let stops = item.kind == AttentionKind::Decision
            && self.changing.is_none()
            && self.loaded.pending.first().map(|d| d.id) == Some(item.id);
        let evidence = self.item_evidence(item);
        let links = self.render_evidence_links(item.id, item.node_id, &evidence, stops, cx);
        let terminal = self.render_terminal_button(item, cx);
        let elsewhere = self.render_resolved_elsewhere_button(item, cx);
        let feedback = self.render_feedback(item, window, cx);
        div()
            .id(SharedString::from(format!("unified-requests-footer-{}", item.id)))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .children(links)
            .child(style::text_muted(div().text_xs().flex_none()).child(reason_label(item.reason)))
            .child(div().flex_1())
            .children(terminal)
            .children(elsewhere)
            .child(feedback)
            .into_any_element()
    }

    /// The terminal icon: continue the lifecycle session that asked, in the
    /// agent's own CLI, for anything too involved to answer here. Only when
    /// that conversation has a session to resume.
    fn render_terminal_button(&self, item: &AttentionItem, cx: &mut Context<Self>) -> Option<AnyElement> {
        let conversation = *self.loaded.sessions.get(&item.id)?;
        let running = self.agent_runs.read(cx).conversation_running(conversation);
        let tooltip = if running {
            "Continue in a terminal (once the agent is done)"
        } else {
            "Continue the session that asked this in a terminal"
        };
        let item = item.clone();
        Some(
            Button::new(SharedString::from(format!("unified-requests-terminal-{}", item.id)))
                .icon(IconName::SquareTerminal)
                .ghost()
                .xsmall()
                .disabled(running)
                .tooltip(tooltip)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.continue_in_terminal(&item, conversation, Source::Click, window, cx);
                }))
                .into_any_element(),
        )
    }

    /// **Answered elsewhere**: the user settled this request outside the
    /// app (typically in the terminal session beside it), so it is dismissed
    /// without an answer going to the agent. Offered for the kinds whose
    /// answer would otherwise be sent (decisions, handed-back plan steps); a
    /// finding's own status buttons already settle it without a turn.
    fn render_resolved_elsewhere_button(&self, item: &AttentionItem, cx: &mut Context<Self>) -> Option<AnyElement> {
        if item.kind == AttentionKind::Finding {
            return None;
        }
        let item = item.clone();
        Some(
            Button::new(SharedString::from(format!("unified-requests-elsewhere-{}", item.id)))
                .label("Answered elsewhere")
                .xsmall()
                .tooltip(
                    "You settled this outside the app, e.g. in the terminal: dismiss it without \
                     sending an answer. Close that terminal first: the runner continues in the same session.",
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.resolve_elsewhere(&item, Source::Click, cx);
                }))
                .into_any_element(),
        )
    }

    /// Dismiss `item` as settled outside the app. Once the node waits on
    /// nothing else, a runner that stopped for it continues by itself
    /// (`unified::runners`).
    pub(crate) fn resolve_elsewhere(&mut self, item: &AttentionItem, source: Source, cx: &mut Context<Self>) {
        record_action(
            cx,
            Focus::Node(item.node_id),
            RESOLVED_ELSEWHERE_ACTION.to_string(),
            source,
            JOURNEY_SURFACE,
            Presented {
                actions: vec![PresentedAction {
                    id: RESOLVED_ELSEWHERE_ACTION.to_string(),
                    label: "Answered elsewhere".to_string(),
                    primary: false,
                    disabled: false,
                }],
                focused: None,
                notices: Vec::new(),
            },
        );
        let id = item.id;
        let result = match item.kind {
            AttentionKind::Decision => {
                self.agent_runs.update(cx, |runs, _| runs.resolve_decision_elsewhere(id))
            }
            AttentionKind::PlanStep => {
                self.agent_runs.update(cx, |runs, _| runs.resolve_plan_step_elsewhere(id))
            }
            AttentionKind::Finding => Ok(()),
        };
        self.set_error("dismiss the request", result);
        self.reload(cx);
    }

    /// Open a terminal resuming `conversation`'s agent session
    /// (`ui::terminal_handoff`); the app lets go of it first.
    pub(crate) fn continue_in_terminal(
        &mut self,
        item: &AttentionItem,
        conversation: Uuid,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        record_action(
            cx,
            Focus::Node(item.node_id),
            CONTINUE_IN_TERMINAL.to_string(),
            source,
            JOURNEY_SURFACE,
            Presented {
                actions: vec![PresentedAction {
                    id: CONTINUE_IN_TERMINAL.to_string(),
                    label: "Continue in a terminal".to_string(),
                    primary: false,
                    disabled: false,
                }],
                focused: None,
                notices: Vec::new(),
            },
        );
        let (agent, config, running) = {
            let runs = self.agent_runs.read(cx);
            (runs.agent().clone(), runs.conversation_config(), runs.conversation_running(conversation))
        };
        terminal_handoff::continue_in_terminal(
            self.fleet.clone(),
            agent,
            config,
            ProtocolKind::Outline,
            Focus::Node(item.node_id),
            Some(conversation),
            running,
            window,
            cx,
        );
    }

    /// The free-text answer field. `only` marks a question asked with no
    /// options, where typing an answer is the only way to answer it.
    fn render_freeform(&self, decision_id: Uuid, only: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = self.freeform_editing == Some(decision_id);
        key_context::set_input_tab_stop(&self.freeform_input, editing, cx);
        div()
            .id(SharedString::from(format!("unified-decisions-freeform-{decision_id}")))
            .key_context(FREEFORM_TAG)
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| this.begin_freeform_edit(decision_id, window, cx)),
            )
            .child(if editing {
                Input::new(&self.freeform_input).small().into_any_element()
            } else {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if only { "Enter to type your answer…" } else { "Enter to answer freely…" })
                    .into_any_element()
            })
    }

    fn card(&self, id: String, cx: &App) -> Stateful<gpui::Div> {
        div()
            .id(SharedString::from(id))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
    }

    fn render_decision_card(&self, item: &AttentionItem, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(decision) = self.loaded.pending.iter().find(|d| d.id == item.id).cloned() else {
            return div().into_any_element();
        };
        let footer = self.render_footer(item, window, cx);
        self.card(format!("unified-decisions-pending-{}", decision.id), cx)
            .child(selectable_markdown(
                SharedString::from(format!("unified-decisions-question-{}", decision.id)),
                decision.question.clone(),
                window,
                cx,
            ))
            .when(tod_core::environment_request::is_request(&decision), |el| {
                el.child(self.render_credential_buttons(&decision, cx))
            })
            .when(
                !decision.options.is_empty() && !tod_core::environment_request::is_request(&decision),
                |el| el.child(self.render_options(&decision, cx)),
            )
            .when(!tod_core::environment_request::is_request(&decision), |el| {
                el.child(self.render_freeform(decision.id, decision.options.is_empty(), cx))
            })
            .child(footer)
            .into_any_element()
    }

    fn render_plan_step_card(&self, item: &AttentionItem, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let step = self.loaded.handoff_steps.iter().find(|s| s.id == item.id).cloned();
        let mut card = self
            .card(format!("unified-decisions-plan-step-{}", item.id), cx)
            .child(kind_badge("Plan step"))
            .child(selectable_markdown(
                SharedString::from(format!("unified-decisions-plan-step-summary-{}", item.id)),
                item.summary.clone(),
                window,
                cx,
            ));
        if let Some(step) = step {
            let why = [
                step.reason
                    .as_ref()
                    .filter(|reason| !matches!(reason, HandoffReason::Decision { .. }))
                    .map(HandoffReason::describe),
                step.note.clone().filter(|note| !note.trim().is_empty()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            if !why.is_empty() {
                card = card.child(style::text_muted(div().text_sm()).child(selectable_text(
                    SharedString::from(format!("unified-decisions-plan-step-why-{}", item.id)),
                    why.join("\n"),
                    window,
                    cx,
                )));
            }
            let buttons = div().flex().flex_wrap().gap_1();
            let buttons = match &step.reason {
                Some(HandoffReason::Decision { options }) => {
                    buttons.children(options.iter().enumerate().map(|(ix, label)| {
                        let step = step.clone();
                        Button::new(SharedString::from(format!("unified-decisions-step-choose-{}-{ix}", step.id)))
                            .label(format!("{}. {label}", ix + 1))
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.answer_plan_step(&step, HandoffAnswer::Choose(ix), Source::Click, cx);
                            }))
                    }))
                }
                Some(HandoffReason::Conflict { obligations }) => {
                    buttons.children(obligations.iter().map(|obligation| {
                        let step = step.clone();
                        let obligation = *obligation;
                        Button::new(SharedString::from(format!(
                            "unified-decisions-step-keep-{}-{obligation}",
                            step.id
                        )))
                        .label(format!("Keep {}", short_id(obligation)))
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.answer_plan_step(&step, HandoffAnswer::Keep(obligation), Source::Click, cx);
                        }))
                    }))
                }
                Some(HandoffReason::Access { .. }) | None => {
                    let step = step.clone();
                    buttons.child(
                        Button::new(SharedString::from(format!("unified-decisions-step-retry-{}", step.id)))
                            .label("Retry")
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.answer_plan_step(&step, HandoffAnswer::Retry, Source::Click, cx);
                            })),
                    )
                }
            };
            card = card.child(buttons);
        }
        card.child(self.render_footer(item, window, cx)).into_any_element()
    }

    fn render_finding_card(&self, item: &AttentionItem, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let finding = self.loaded.findings.iter().find(|f| f.id == item.id).cloned();
        let mut card = self
            .card(format!("unified-decisions-finding-{}", item.id), cx)
            .child(kind_badge("Finding"))
            .child(selectable_markdown(
                SharedString::from(format!("unified-decisions-finding-summary-{}", item.id)),
                item.summary.clone(),
                window,
                cx,
            ));
        if let Some(finding) = finding {
            card = card.child(div().flex().flex_wrap().gap_1().children(
                [(FINDING_FIXED, "Fixed"), (FINDING_OUT_OF_SCOPE, "Out of scope"), (FINDING_DECLINED, "Declined")]
                    .into_iter()
                    .map(|(status, label)| {
                        let finding = finding.clone();
                        Button::new(SharedString::from(format!("unified-decisions-finding-{status}-{}", finding.id)))
                            .label(label)
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.answer_finding(&finding, status, Source::Click, cx);
                            }))
                    }),
            ));
        }
        card.child(self.render_footer(item, window, cx)).into_any_element()
    }

    fn render_item(&self, item: &AttentionItem, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match item.kind {
            AttentionKind::Decision => self.render_decision_card(item, window, cx),
            AttentionKind::PlanStep => self.render_plan_step_card(item, window, cx),
            AttentionKind::Finding => self.render_finding_card(item, window, cx),
        }
    }

    fn render_log_entry(&self, entry: &LogEntry, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let decision = entry.decision.clone();
        let decision_id = decision.id;
        let chosen = describe_answer(&decision, &entry.answer);
        let changing = self.changing == Some(decision_id);
        let evidence = if changing {
            self.render_evidence_links(decision_id, decision.node_id, &decision.evidence, true, cx)
        } else {
            Vec::new()
        };
        div()
            .id(("unified-decisions-log-entry", entry.answer.id as u64))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_b_1()
            .border_color(border)
            .child(div().text_sm().child(selectable_text(
                ("unified-decisions-log-question", entry.answer.id as u64),
                decision.question.clone(),
                window,
                cx,
            )))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("Answered: {chosen} · {}", entry.answer.actor)),
                    )
                    .child(
                        Button::new(("unified-decisions-change", entry.answer.id as u64))
                            .label(if changing { "Cancel" } else { "Change" })
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.changing == Some(decision_id) {
                                    this.cancel_change(cx);
                                } else {
                                    this.start_change(decision_id, cx);
                                }
                            })),
                    )
                    .when_some(decision.conversation_id, |el, conversation_id| {
                        el.child(
                            Button::new(("unified-decisions-transcript", entry.answer.id as u64))
                                .label("Transcript")
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open(PanelKind::Transcript(conversation_id), false, cx);
                                })),
                        )
                    }),
            )
            .when(changing, |el| {
                el.child(self.render_options(&decision, cx))
                    .child(div().flex().flex_wrap().gap_2().children(evidence))
                    .child(self.render_freeform(decision_id, decision.options.is_empty(), cx))
            })
            .into_any_element()
    }

    /// The log's entries alone, newest first: the task panel's Answered
    /// drawer, which has its own header.
    pub fn render_log_entries(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let entries: Vec<AnyElement> =
            self.loaded.log.iter().rev().map(|entry| self.render_log_entry(entry, window, cx)).collect();
        div().flex().flex_col().children(entries).into_any_element()
    }

}

/// A small label naming which kind a card is.
fn kind_badge(label: &'static str) -> impl IntoElement {
    div().text_xs().child(label)
}

/// "\"per invoice\"" or "\"free text\"" for one log entry.
pub fn describe_answer(decision: &Decision, answer: &DecisionAnswer) -> String {
    let picked = answer
        .option
        .and_then(|o| usize::try_from(o).ok())
        .and_then(|ix| decision.options.get(ix.checked_sub(1)?));
    match (picked, answer.text.as_deref()) {
        (Some(label), Some(text)) if !text.is_empty() => format!("\"{label}\" ({text})"),
        (Some(label), _) => format!("\"{label}\""),
        (None, Some(text)) if !text.is_empty() => format!("\"{text}\""),
        _ => "(no answer recorded)".to_string(),
    }
}

impl Render for Requests {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let items: Vec<AnyElement> = self
            .loaded
            .items
            .clone()
            .iter()
            .map(|item| self.render_item(item, window, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .when_some(self.last_error.clone(), |el, err| {
                el.child(style::text_error(div().text_sm()).child(selectable_text(
                    "unified-decisions-error",
                    err,
                    window,
                    cx,
                )))
            })
            .children(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_has_a_label() {
        for reason in [
            RequestReason::Intent,
            RequestReason::MissingRule,
            RequestReason::Conflict,
            RequestReason::Access,
            RequestReason::Risk,
            RequestReason::Capability,
            RequestReason::Other,
        ] {
            assert!(!reason_label(reason).is_empty());
        }
    }

    #[test]
    fn evidence_names_fall_back_to_kind_and_short_id() {
        let id = Uuid::new_v4();
        let mut names = HashMap::new();
        let evidence = EvidenceRef { kind: "obligation".into(), id };
        assert!(evidence_label(&evidence, &names).starts_with("obligation "));
        names.insert(id, "Rounds per line".to_string());
        assert_eq!(evidence_label(&evidence, &names), "Rounds per line");
    }

    #[gpui::test]
    fn should_not_have_asked_records_feedback_and_leaves_the_request_pending(cx: &mut gpui::TestAppContext) {
        use crate::views::rows::fixture::Fixture;
        use std::cell::RefCell;
        use std::rc::Rc;
        use tod_store::request_feedback::RequestFeedbackRepo;

        let fixture = Fixture::new();
        let decision_id = fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AskDecision {
                    node_id: fixture.node_id,
                    conversation_id: None,
                    protocol: Some("implement".to_string()),
                    decision: tod_store::decisions::NewDecision {
                        question: "Round per line or per invoice?".to_string(),
                        options: vec!["per line".to_string(), "per invoice".to_string()],
                        reason: "risk".to_string(),
                        ..Default::default()
                    },
                },
            )
            .unwrap()
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap();

        cx.update(gpui_component::init);
        let agent: crate::interview::agent::SharedAgent =
            Arc::new(std::sync::Mutex::new(Box::new(tod_agent::MockAgentProvider::new())));
        let agent_runs = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));
        let lifecycle = cx.new(|_| LifecycleController::new(fixture.store.clone()));
        let (node, fleet) = (fixture.node_id, fixture.store.clone());
        let slot = Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let focus = cx.focus_handle();
            let view = cx.new(|cx| Requests::new(Some(node), fleet, agent_runs, lifecycle, focus, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let requests: Entity<Requests> = slot.borrow_mut().take().unwrap();
        cx.run_until_parked();

        let item = requests.read_with(cx, |r, _| r.items()[0].clone());
        assert_eq!(item.id, decision_id);
        requests.update(cx, |r, cx| r.should_not_have_asked(&item, Source::Click, cx));
        cx.run_until_parked();

        let feedback_id = requests.read_with(cx, |r, _| r.feedback_for(decision_id).and_then(|s| s.id)).unwrap();
        let row = fixture.store.read(|conn| RequestFeedbackRepo::new(conn).get(feedback_id)).unwrap().unwrap();
        assert_eq!(row.verdict, VERDICT_SHOULD_NOT_ASK);
        assert_eq!(row.request_kind, KIND_DECISION);
        assert_eq!(row.request_id, decision_id);
        assert_eq!(row.reason, "risk");
        assert_eq!(row.protocol.as_deref(), Some("implement"));
        assert_eq!(row.conversation_id, None);

        requests.update(cx, |r, cx| {
            r.toggle_bad_question(decision_id, cx);
            r.set_feedback_note(decision_id, Some("It was in the spec".to_string()), cx);
        });
        cx.run_until_parked();
        let row = fixture.store.read(|conn| RequestFeedbackRepo::new(conn).get(feedback_id)).unwrap().unwrap();
        assert_eq!(row.verdict, VERDICT_BAD_QUESTION);
        assert_eq!(row.note.as_deref(), Some("It was in the spec"));

        requests.read_with(cx, |r, _| {
            assert_eq!(r.items().len(), 1);
            assert_eq!(r.loaded.pending.len(), 1);
        });
    }
}
