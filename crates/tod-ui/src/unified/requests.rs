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
//! name), the reason ([`reason_label`]), and at the right the slot set with
//! [`Requests::set_footer_extra`] (T6's "Shouldn't have asked").
//!
//! The decision answer log with **Change** is here too
//! ([`Requests::render_log_entries`]), for the task panel's Answered drawer.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    Stateful, Styled, Subscription, Window, actions, div,
    prelude::FluentBuilder,
};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable};
use tod_core::attention::{AttentionItem, AttentionKind, RequestReason};
use tod_core::conversation::implement::HandoffAnswer;
use tod_journey::{Presented, PresentedAction};
use tod_store::decisions::{DECISION_PENDING, Decision, DecisionAnswer, DecisionRepo, EvidenceRef};
use tod_store::fleet::FleetStore;
use tod_store::interview::short_id;
use tod_store::outline::PlanStep;
use tod_store::outline::repos::plan_steps::HandoffReason;
use tod_store::outline::repos::{ObligationRepo, PlanStepRepo};
use tod_store::review::{FINDING_DECLINED, FINDING_FIXED, FINDING_OUT_OF_SCOPE, ReviewFinding, ReviewRepo};
use uuid::Uuid;

use crate::ui::agent_runs::AgentRuns;
use crate::ui::journey::{Source, record_action};
use crate::ui::key_context;
use crate::ui::selectable_text::{selectable_markdown, selectable_text};
use crate::ui::style;
use crate::unified::columns::PanelKind;
use crate::unified::panel::PanelOpenRequest;
use crate::views::lifecycle_control::{CriterionOutcome, LifecycleController};

/// The key context a host panel puts on its focused root so the request keys
/// apply there.
pub const REQUESTS_CONTEXT: &str = "UnifiedRequests";
/// Key-context tag for whichever freeform answer field is in edit mode, so
/// Escape only fires for that one field (`key_context::including_tag`).
const FREEFORM_TAG: &str = "RequestsFreeform";
/// The journey surface every answer is recorded under; the name "decisions"
/// is kept so earlier journeys still compare.
const JOURNEY_SURFACE: &str = "decisions";

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
            anyhow::Ok(Loaded { pending, handoff_steps, findings, items, log, names })
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

/// A hook for extra controls at the right of each request's footer line
/// (T6's "Shouldn't have asked").
pub type FooterExtra = Rc<dyn Fn(&AttentionItem, &mut Window, &mut App) -> Option<AnyElement>>;

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
    footer_extra: Option<FooterExtra>,
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
            footer_extra: None,
            _subscriptions: vec![freeform_sub, lifecycle_sub],
            _poll,
        };
        this.reload(cx);
        this
    }

    /// T6: set the control rendered at the right of each footer line.
    #[allow(dead_code)] // T6 is its first caller.
    pub fn set_footer_extra(&mut self, extra: Option<FooterExtra>, cx: &mut Context<Self>) {
        self.footer_extra = extra;
        cx.notify();
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
                    self.answer(decision, Some(action.0), None, Source::Keyboard, cx);
                }
            }
            AttentionKind::PlanStep => {
                if let Some(step) = self.loaded.handoff_steps.iter().find(|s| s.id == top.id).cloned() {
                    self.answer_plan_step(&step, HandoffAnswer::Choose(action.0 - 1), Source::Keyboard, cx);
                }
            }
            AttentionKind::Finding | AttentionKind::Gate => {}
        }
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

    /// Waive one failing gate criterion through the shared controller.
    pub(crate) fn waive_criterion(&mut self, node_id: Uuid, criterion: &CriterionOutcome, source: Source, cx: &mut Context<Self>) {
        record_action(
            cx,
            tod_store::conversation::Focus::Node(node_id),
            "waive".to_string(),
            source,
            JOURNEY_SURFACE,
            Presented {
                actions: vec![PresentedAction {
                    id: criterion.criterion_id.to_string(),
                    label: format!("Waive: {}", criterion.label),
                    primary: true,
                    disabled: false,
                }],
                focused: None,
                notices: vec![criterion.label.clone()],
            },
        );
        let criterion_id = criterion.criterion_id;
        let task_id = node_id.to_string();
        self.lifecycle.update(cx, |controller, cx| controller.waive(&task_id, criterion_id, cx));
        self.reload(cx);
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
            AttentionKind::Gate => vec![EvidenceRef { kind: "conversation".into(), id: item.id }],
        }
    }

    /// The one footer line: evidence links, the reason, and the T6 slot.
    fn render_footer(&self, item: &AttentionItem, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let stops = item.kind == AttentionKind::Decision
            && self.changing.is_none()
            && self.loaded.pending.first().map(|d| d.id) == Some(item.id);
        let evidence = self.item_evidence(item);
        let links = self.render_evidence_links(item.id, item.node_id, &evidence, stops, cx);
        let extra = self.footer_extra.clone().and_then(|f| f(item, window, cx));
        div()
            .id(SharedString::from(format!("unified-requests-footer-{}", item.id)))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .children(links)
            .child(style::text_muted(div().text_xs().flex_none()).child(reason_label(item.reason)))
            .child(div().flex_1())
            .children(extra)
            .into_any_element()
    }

    fn render_freeform(&self, decision_id: Uuid, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .child("Enter to answer freely…")
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
            .child(self.render_options(&decision, cx))
            .child(self.render_freeform(decision.id, cx))
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

    fn render_gate_card(&self, item: &AttentionItem, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let node_id = item.node_id;
        let failing: Vec<CriterionOutcome> = self
            .lifecycle
            .read(cx)
            .state(&node_id.to_string())
            .map(|s| s.criteria_detail.iter().filter(|r| r.is_failing()).cloned().collect())
            .unwrap_or_default();
        let card = self
            .card(format!("unified-decisions-gate-{}", item.id), cx)
            .child(kind_badge("Gate check"))
            .child(selectable_markdown(
                SharedString::from(format!("unified-decisions-gate-summary-{}", item.id)),
                item.summary.clone(),
                window,
                cx,
            ));
        let rows: Vec<AnyElement> = failing
            .into_iter()
            .map(|criterion| {
                let label = criterion.label.clone();
                div()
                    .id(SharedString::from(format!("unified-decisions-gate-row-{}", criterion.criterion_id)))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(selectable_text(
                        SharedString::from(format!("unified-decisions-gate-label-{}", criterion.criterion_id)),
                        label,
                        window,
                        cx,
                    )))
                    .child(
                        Button::new(SharedString::from(format!(
                            "unified-decisions-gate-waive-{}",
                            criterion.criterion_id
                        )))
                        .label("Waive")
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.waive_criterion(node_id, &criterion, Source::Click, cx);
                        })),
                    )
                    .into_any_element()
            })
            .collect();
        card.children(rows).child(self.render_footer(item, window, cx)).into_any_element()
    }

    fn render_item(&self, item: &AttentionItem, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match item.kind {
            AttentionKind::Decision => self.render_decision_card(item, window, cx),
            AttentionKind::PlanStep => self.render_plan_step_card(item, window, cx),
            AttentionKind::Finding => self.render_finding_card(item, window, cx),
            AttentionKind::Gate => self.render_gate_card(item, window, cx),
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
                    .child(self.render_freeform(decision_id, cx))
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
}
