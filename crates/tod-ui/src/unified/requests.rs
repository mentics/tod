//! Requests: everything a node is waiting on the user for, rendered and
//! answered in one place: the task panel (`doc/ui/task-panel.md`
//! "Requests").
//!
//! [`Requests`] is an entity a host panel embeds as a child. It loads
//! [`tod_core::attention::for_node`] (oldest first) off the UI thread and
//! reloads on every store change and every change to the shared
//! [`LifecycleController`]. Each kind answers through its existing code path,
//! never a new mutation: a decision through `AgentRuns::answer_decision`; a
//! plan step through `AgentRuns::answer_plan_step_handoff`. A review finding
//! is not a request (tod and the agents settle findings; the user reopens or
//! rescores them in the findings panel), and neither is a gate criterion
//! (`tod_core::attention` has two sources only: gates are app checks and
//! never hand anything to the user); the criteria last found failing show
//! on the runner line's Check now result; there is no way to waive one, the
//! user moves the node on with the lifecycle dropdown. Every answer records
//! a journey `UserAction` with a `Presented` snapshot of the choices shown.
//!
//! Keys (digits answer the top request, Up/Down move a stop across the
//! current decision's evidence links and freeform field, Enter/Ctrl+Enter
//! activate it, Escape leaves the freeform field) are bound in
//! [`REQUESTS_CONTEXT`]: the host puts that key context on its focused root
//! and forwards the actions with [`bind_request_actions`].
//!
//! Each request ends with one footer line: its evidence links (each item's
//! name), the reason ([`reason_label`]), when it was raised ([`when_label`]:
//! the wall-clock time, with the date once it is not today's), and at the
//! right, for a request triage escalated, **Discuss** (`doc/lifecycle/triage.md`):
//! it opens the triage session that read the request in the chat drawer.
//! There is no "shouldn't have asked" button: a confusing request is flagged
//! by highlighting its text (see [`Requests::flag_confusing`]), which sends the
//! highlighted words into the triage session.
//!
//! The decision answer log with **Change** is here too
//! ([`Requests::render_log_entries`]), for the task panel's Answered drawer.

use std::collections::HashMap;
use std::sync::Arc;

/// Unsent free-form answers, by decision, for the life of the app: the
/// panel showing a node is rebuilt each time it is opened, so they live here.
#[derive(Default)]
struct FreeformDrafts(HashMap<Uuid, String>);

impl gpui::Global for FreeformDrafts {}

impl FreeformDrafts {
    fn get(decision: Uuid, cx: &App) -> String {
        cx.try_global::<Self>().and_then(|d| d.0.get(&decision).cloned()).unwrap_or_default()
    }

    fn set(decision: Uuid, text: String, cx: &mut App) {
        let drafts = cx.default_global::<Self>();
        if text.is_empty() {
            drafts.0.remove(&decision);
        } else {
            drafts.0.insert(decision, text);
        }
    }
}

use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    Stateful, StatefulInteractiveElement, Styled, Subscription, Window, actions, div,
    prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, IconName, Sizable};
use tod_core::attention::{AttentionItem, AttentionKind, RequestReason};
use tod_core::conversation::implement::HandoffAnswer;
use tod_journey::{Presented, PresentedAction};
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::decisions::{DECISION_PENDING, Decision, DecisionAnswer, DecisionRepo, EvidenceRef};
use tod_store::fleet::FleetStore;
use tod_store::interview::short_id;
use tod_store::outline::PlanStep;
use tod_store::outline::repos::plan_steps::HandoffReason;
use tod_store::outline::repos::{ObligationRepo, PlanStepRepo};
use uuid::Uuid;

use crate::ui::panel_export::RecordText as _;
use crate::ui::off_thread::off_thread;
use crate::ui::agent_runs::AgentRuns;
use crate::ui::journey::{Source, record_action};
use crate::ui::key_context;
use crate::ui::selectable_text::{FlagHandler, selectable_markdown, selectable_markdown_flaggable, selectable_text};
use crate::ui::style;
use crate::ui::terminal_handoff::{self, CONTINUE_IN_TERMINAL};
use crate::unified::columns::PanelKind;
use crate::unified::panel::{PanelDiscussRequest, PanelOpenRequest};
use crate::views::lifecycle_control::LifecycleController;

/// The key context a host panel puts on its focused root so the request keys
/// apply there.
pub const REQUESTS_CONTEXT: &str = "UnifiedRequests";
/// Key-context tag for whichever freeform answer field is in edit mode, so
/// Escape only fires for that one field (`key_context::including_tag`).
/// The accessible name of a request's free-text answer field.
fn freeform_name(question: &str) -> String {
    let line = question.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line: String = line.chars().take(80).collect();
    format!("Answer: {line}")
}

const FREEFORM_TAG: &str = "RequestsFreeform";
/// Height of the free-text answer area while it is being edited (a textarea
/// has no height of its own).
const FREEFORM_HEIGHT: f32 = 96.;
/// The journey surface every answer is recorded under; the name "decisions"
/// is kept so earlier journeys still compare.
const JOURNEY_SURFACE: &str = "decisions";
/// The journey action id of **Answered elsewhere**.
const RESOLVED_ELSEWHERE_ACTION: &str = "answered-elsewhere";
/// The journey action id of **Discuss** (open the triage session in the chat).
const DISCUSS_ACTION: &str = "discuss";
/// The journey action id of the highlight menu's **This is confusing**.
const FLAG_CONFUSING_ACTION: &str = "flag-confusing";
/// The journey action id of **Send the task back** beside a changed answer.
const SEND_BACK_ACTION: &str = "send-back";
/// The label of the highlight menu's flag.
pub const FLAG_CONFUSING_LABEL: &str = "This is confusing";

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
        DecisionsFreeformSubmit,
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
        KeyBinding::new("ctrl-enter", DecisionsFreeformSubmit, with_input),
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
    let r7 = requests.clone();
    el.key_context(REQUESTS_CONTEXT)
        .on_action(move |a: &DecisionOptionKey, window, cx| {
            r1.update(cx, |r, cx| r.answer_option_key(a, window, cx))
        })
        .on_action(move |_: &DecisionsActivate, window, cx| r2.update(cx, |r, cx| r.activate(false, window, cx)))
        .on_action(move |_: &DecisionsCtrlActivate, window, cx| r3.update(cx, |r, cx| r.activate(true, window, cx)))
        .on_action(move |_: &DecisionsFreeformEscape, window, cx| r4.update(cx, |r, cx| r.exit_freeform_edit(window, cx)))
        .on_action(move |_: &DecisionsFreeformSubmit, _, cx| r7.update(cx, |r, cx| r.submit_freeform(cx)))
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
        RequestReason::Broken => "Something is broken",
        RequestReason::Other => "Other",
    }
}

/// What picking each option changes in the node's obligations, shown above
/// the options so the user knows what answering approves
/// (`Decision::edits`; applied as the user's own edit when answered).
fn edit_preview(decision: &Decision) -> Option<String> {
    if decision.edits.is_empty() {
        return None;
    }
    let mut out = if decision.conversation_id.is_some() {
        String::from(
            "Answering changes the obligations as your own decision (listed in the asking conversation's change set, where it can be reversed):",
        )
    } else {
        String::from("Answering changes the obligations as your own decision:")
    };
    for edit in &decision.edits {
        if edit.affirm {
            out.push_str(&format!(
                "\n\n**Option {}** approves obligation {} as it stands (it becomes yours: no agent changes it)",
                edit.option,
                tod_store::interview::short_id(edit.obligation_id)
            ));
            continue;
        }
        out.push_str(&format!(
            "\n\n**Option {}** changes obligation {}",
            edit.option,
            tod_store::interview::short_id(edit.obligation_id)
        ));
        if let Some(was) = &edit.was {
            out.push_str(&format!("\n\nfrom: {}", was.trim()));
        }
        out.push_str(&format!("\n\nto: {}", edit.body.trim()));
    }
    Some(out)
}

/// When a request was raised (`AttentionItem::since`), as its footer shows
/// it: the time of day when it was raised today, else the date and time, in
/// local time. `today` is the day the footer is drawn on, so the label does
/// not depend on the clock in a test.
pub fn when_label(since_ms: i64, today: chrono::NaiveDate) -> String {
    use chrono::{Local, TimeZone};
    let Some(at) = Local.timestamp_millis_opt(since_ms).single() else {
        return String::new();
    };
    if at.date_naive() == today {
        at.format("%H:%M").to_string()
    } else {
        at.format("%b %-d %H:%M").to_string()
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
    /// The node's lifecycle state, to tell whether a changed answer's task
    /// has moved past the phase that asked.
    pub lifecycle: Option<String>,
}

/// The lifecycle state a request filed by an agent running `protocol` was
/// asked in, when the protocol says (a phase agent's state is the node's own
/// at the time, which is not recorded).
pub fn asking_state(protocol: &str) -> Option<&'static str> {
    match protocol {
        "implementation" | "fix" => Some("active"),
        "verification" => Some("verifying"),
        "review" => Some("review"),
        "pr" => Some("pr"),
        _ => None,
    }
}

/// The state to offer sending the task back to after the user changes the
/// answer to `decision`: the phase that asked, when the node has moved past it.
pub fn send_back_target(decision: &Decision, lifecycle: Option<&str>) -> Option<&'static str> {
    let asked = asking_state(decision.protocol.as_deref()?)?;
    let now = tod_core::task::model::lifecycle_rank(lifecycle?);
    (now > tod_core::task::model::lifecycle_rank(asked)).then_some(asked)
}

/// The conversation `item` came from: a decision's own, or the
/// implement/verify conversation a plan step was handed back by.
fn asking_conversation(
    conn: &rusqlite::Connection,
    item: &AttentionItem,
    pending: &[Decision],
) -> anyhow::Result<Option<Uuid>> {
    Ok(match item.kind {
        AttentionKind::Decision => pending.iter().find(|d| d.id == item.id).and_then(|d| d.conversation_id),
        AttentionKind::PlanStep => {
            crate::ui::agent_runs::latest_handoff_conversation(conn, Focus::Node(item.node_id))?.map(|c| c.id)
        }
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
            let items = tod_core::attention::for_node(conn, node_id)?.items;

            let mut sessions = HashMap::new();
            let conversations = ConversationRepo::new(conn);
            for item in &items {
                let Some(conversation) = asking_conversation(conn, item, &pending)? else {
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
            let lifecycle = lifecycle.get(&node_id).cloned();
            anyhow::Ok(Loaded { pending, handoff_steps, items, log, names, sessions, lifecycle })
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

pub struct Requests {
    pub(crate) node_id: Option<Uuid>,
    fleet: Arc<FleetStore>,
    agent_runs: Entity<AgentRuns>,
    /// The one lifecycle controller the shell shares: it holds the criteria
    /// the last gate check recorded (`load_persisted`), and nothing else a
    /// request answers through.
    pub(crate) lifecycle: Entity<LifecycleController>,
    /// The host panel's focus handle, refocused when the freeform field is
    /// left.
    host_focus: FocusHandle,
    pub(crate) loaded: Loaded,
    /// Bumped on every reload so a slower, older load never overwrites a
    /// newer one.
    generation: u64,
    freeform_editing: Option<Uuid>,
    /// The decision whose unsent text `freeform_input` holds.
    freeform_draft_for: Option<Uuid>,
    freeform_input: Entity<TextareaState>,
    /// A log entry's decision the user asked to change.
    pub(crate) changing: Option<Uuid>,
    /// Index of the keyboard stop on the current decision.
    selected_link: usize,
    pub(crate) last_error: Option<String>,
    /// A line saying what a flag recorded (**This is confusing** on a request
    /// no triage session read), until the next one or a reload.
    pub(crate) flag_note: Option<String>,
    _subscriptions: Vec<Subscription>,
    _poll: gpui::Task<()>,
}

impl EventEmitter<PanelOpenRequest> for Requests {}
impl EventEmitter<PanelDiscussRequest> for Requests {}

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
            cx.new(|cx| TextareaState::new(window, cx).rows(4).placeholder("Freeform answer… (Ctrl+Enter to submit)"));
        let freeform_sub = cx.subscribe(&freeform_input, |this, _, event, cx| {
            match event {
                InputEvent::Change => {
                    if let Some(owner) = this.freeform_draft_for {
                        let text = this.freeform_input.read(cx).text().to_string();
                        FreeformDrafts::set(owner, text, cx);
                    }
                }
                _ => {}
            }
        });
        // A change to the shared controller (the criteria it shows) reloads here too.
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
            freeform_draft_for: None,
            freeform_input,
            changing: None,
            selected_link: 0,
            last_error: None,
            flag_note: None,
            _subscriptions: vec![freeform_sub, lifecycle_sub],
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
            // An option that moves the node is part of what the user saw.
            notices: std::iter::once(tod_core::request_brief::headline(decision))
                .chain(decision.moves.iter().map(|m| format!("option {} sends the task back to {}", m.option, m.to)))
                .collect(),
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
        // The answer is recorded off the UI thread; what was being edited
        // closes now, and the list reloads when it is in.
        self.flag_note = None;
        self.freeform_editing = None;
        FreeformDrafts::set(decision.id, String::new(), cx);
        if self.freeform_draft_for == Some(decision.id) {
            self.freeform_draft_for = None;
        }
        self.changing = None;
        let this = cx.weak_entity();
        self.agent_runs.update(cx, |runs, cx| {
            runs.answer_decision_then(decision_id, option, text, cx, move |result, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.set_error("record answer", result);
                    this.reload(cx);
                });
            })
        });
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
                    // A request triage escalated is read with its session
                    // first: a number key never answers it.
                    if decision.triage_conversation_id.is_some() {
                        return;
                    }
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
        }
    }

    /// **Discuss**: open the triage session that read `decision` in the chat
    /// drawer, with `message` sent into it when there is one.
    pub(crate) fn discuss(&mut self, decision: &Decision, message: Option<String>, source: Source, cx: &mut Context<Self>) {
        let Some(conversation) = decision.triage_conversation_id else { return };
        let (id, label) = match message {
            Some(_) => (FLAG_CONFUSING_ACTION, FLAG_CONFUSING_LABEL),
            None => (DISCUSS_ACTION, "Discuss"),
        };
        let mut presented = Self::presented_for(decision);
        presented.actions.push(PresentedAction {
            id: id.to_string(),
            label: label.to_string(),
            primary: false,
            disabled: false,
        });
        record_action(cx, Focus::Node(decision.node_id), id.to_string(), source, JOURNEY_SURFACE, presented);
        cx.emit(PanelDiscussRequest { node_id: decision.node_id, conversation, message });
    }

    /// The highlight menu's **This is confusing**. On a request a triage
    /// session read, the highlighted words go into that session as a chat
    /// message, so it can explain or rewrite its brief. On any other request
    /// (one that went straight to the user, or that triage never read) there is
    /// no one to explain it, so the flag is recorded as an improvement lesson
    /// (source `user`, linked to the request and the conversation that asked
    /// it) with the highlighted words as the user's remark.
    pub(crate) fn flag_confusing(&mut self, decision_id: Uuid, text: String, cx: &mut Context<Self>) {
        let Some(decision) = self.loaded.pending.iter().find(|d| d.id == decision_id).cloned() else { return };
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if decision.triage_conversation_id.is_none() {
            self.record_confusing_lesson(&decision, text.to_string(), cx);
            return;
        }
        let message = format!(
            "I highlighted this in the request and it is confusing: \"{text}\". Say what it means in plain \
             words, and fix the brief if it should have said it better."
        );
        self.discuss(&decision, Some(message), Source::Click, cx);
    }

    /// **This is confusing** on a request with no triage session: records the
    /// lesson off the UI thread and says so; a failure is shown, never dropped.
    fn record_confusing_lesson(&mut self, decision: &Decision, remark: String, cx: &mut Context<Self>) {
        let mut presented = Self::presented_for(decision);
        presented.actions.push(PresentedAction {
            id: FLAG_CONFUSING_ACTION.to_string(),
            label: FLAG_CONFUSING_LABEL.to_string(),
            primary: false,
            disabled: false,
        });
        record_action(
            cx,
            Focus::Node(decision.node_id),
            FLAG_CONFUSING_ACTION.to_string(),
            Source::Click,
            JOURNEY_SURFACE,
            presented,
        );
        let fleet = self.fleet.clone();
        let id = decision.id;
        off_thread(
            cx,
            move || {
                fleet
                    .interview(
                        tod_store::interview::AUTHOR_USER,
                        tod_store::interview::InterviewCommand::Lessons(tod_store::lessons::Op::FlagRequest {
                            decision: id,
                            remark,
                        }),
                    )
                    .map(|_| ())
                    .map_err(|e| anyhow::anyhow!("{e}"))
            },
            |this, result, cx| {
                let ok = result.is_ok();
                this.set_error("record that the request is confusing", result);
                this.flag_note = ok.then(|| "Noted: this request was confusing. It goes into the process review.".to_string());
                cx.notify();
            },
        );
    }

    /// The flag a request's text offers when highlighted: on every request.
    /// With a triage session it goes there; without one it is recorded as a
    /// lesson ([`Self::flag_confusing`]).
    fn flag_for(&self, decision: &Decision, cx: &mut Context<Self>) -> Option<(&'static str, FlagHandler)> {
        let this = cx.weak_entity();
        let id = decision.id;
        let handler: FlagHandler = std::rc::Rc::new(move |text, _window, cx| {
            let _ = this.update(cx, |this, cx| this.flag_confusing(id, text, cx));
        });
        Some((FLAG_CONFUSING_LABEL, handler))
    }

    /// **Send the task back** beside a changed answer: the user moves the
    /// task back to the phase that asked, since it went on without the answer
    /// they are now changing.
    pub(crate) fn send_back(&mut self, decision: &Decision, state: &'static str, cx: &mut Context<Self>) {
        record_action(
            cx,
            Focus::Node(decision.node_id),
            SEND_BACK_ACTION.to_string(),
            Source::Click,
            JOURNEY_SURFACE,
            Presented {
                actions: vec![PresentedAction {
                    id: SEND_BACK_ACTION.to_string(),
                    label: format!("Send the task back to {state}"),
                    primary: false,
                    disabled: false,
                }],
                focused: None,
                notices: vec![tod_core::request_brief::headline(decision)],
            },
        );
        let fleet = self.fleet.clone();
        let node = decision.node_id;
        off_thread(
            cx,
            move || {
                tod_core::lifecycle::set_lifecycle_by_user_because(
                    &fleet,
                    node,
                    state,
                    Some("The user changed the answer to a request the task had already gone past"),
                )
            },
            |this, result, cx| {
                this.set_error("send the task back", result);
                this.reload(cx);
            },
        );
    }

    /// Open the dialog that takes a requested credential's value.
    fn provide_credential(&mut self, decision: &Decision, window: &mut Window, cx: &mut Context<Self>) {
        crate::ui::credential_request::open(window, cx, self.credential_ctx(), decision.clone());
    }

    /// Retry without entering a value (the credential may already be set).
    fn retry_credential(&mut self, decision: &Decision, window: &mut Window, cx: &mut Context<Self>) {
        crate::ui::credential_request::open_and_retry(window, cx, self.credential_ctx(), decision.clone());
    }

    fn credential_ctx(&self) -> crate::ui::credential_request::Ctx {
        crate::ui::credential_request::Ctx {
            fleet: self.fleet.clone(),
            data_root: self.fleet.paths().root().to_path_buf(),
            agent_runs: self.agent_runs.clone(),
        }
    }

    /// The card's buttons for a credential request: Provide opens the dialog
    /// (the value is typed there, never in the request), and the other answers
    /// that it cannot be provided.
    fn render_credential_buttons(&self, decision: &Decision, cx: &mut Context<Self>) -> impl IntoElement {
        let (provide, retry, decline) = (decision.clone(), decision.clone(), decision.clone());
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
                Button::new(SharedString::from(format!("unified-decisions-retry-{}", decision.id)))
                    .label("Retry")
                    .small()
                    .on_click(cx.listener(move |this, _, window, cx| this.retry_credential(&retry, window, cx))),
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
        let this = cx.weak_entity();
        self.agent_runs.update(cx, |runs, cx| {
            runs.answer_plan_step_handoff_then(step_id, answer, cx, move |result, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.set_error("answer plan step", result);
                    this.reload(cx);
                });
            })
        });
    }

    fn begin_freeform_edit(&mut self, decision_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.freeform_editing = Some(decision_id);
        // The one input is shared by every decision, and the panel is rebuilt
        // per node: its text is mirrored into `FreeformDrafts` as it changes,
        // and the decision's own draft is loaded back here.
        if self.freeform_draft_for != Some(decision_id) {
            self.freeform_draft_for = Some(decision_id);
            let text = FreeformDrafts::get(decision_id, cx);
            self.freeform_input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
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
        let border = cx.theme().border;
        let hover = cx.theme().secondary_hover;
        let radius = cx.theme().radius;
        // One option per row, wrapping: a long option must stay readable, not
        // run off the edge as a one-line button label would.
        div().flex().flex_col().gap_1().children(decision.options.clone().into_iter().enumerate().map(
            |(ix, label)| {
                let option = ix + 1;
                let decision = decision.clone();
                div()
                    .id(SharedString::from(format!("unified-decisions-option-{}-{option}", decision.id)))
                    .w_full()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .border_1()
                    .border_color(border)
                    .rounded(radius)
                    .cursor_pointer()
                    .hover(move |el| el.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.click_option(decision.clone(), option, cx)))
                    .child({
                        let text = format!("{option}. {label}");
                        div().child(text.clone()).record_text(text)
                    })
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
                        .child(link.label.clone())
                        .record_text(link.label)
                        .into_any_element(),
                    None => div()
                        .text_xs()
                        .text_color(muted)
                        .child(link.label.clone())
                        .record_text(link.label)
                        .into_any_element(),
                }
            })
            .collect()
    }

    /// The evidence a request points at: a decision's own evidence, or the
    /// item a plan step or gate check is about.
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
        }
    }

    /// The one footer line: evidence links, the reason, and "Shouldn't have asked".
    fn render_footer(&self, item: &AttentionItem, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let stops = item.kind == AttentionKind::Decision
            && self.changing.is_none()
            && self.loaded.pending.first().map(|d| d.id) == Some(item.id);
        let evidence = self.item_evidence(item);
        let links = self.render_evidence_links(item.id, item.node_id, &evidence, stops, cx);
        let terminal = self.render_terminal_button(item, cx);
        let elsewhere = self.render_resolved_elsewhere_button(item, cx);
        let discuss = self.render_discuss_button(item, cx);
        div()
            .id(SharedString::from(format!("unified-requests-footer-{}", item.id)))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .children(links)
            .child(
                style::text_muted(div().text_xs().flex_none())
                    .child(reason_label(item.reason))
                    .record_text(reason_label(item.reason)),
            )
            .child({
                let when = format!("Asked {}", when_label(item.since, chrono::Local::now().date_naive()));
                style::text_muted(div().text_xs().flex_none())
                    .child(when.clone())
                    .record_text(when)
            })
            .child(div().flex_1())
            .children(discuss)
            .children(terminal)
            .children(elsewhere)
            .into_any_element()
    }

    /// **Discuss**, on a request triage escalated: its session, in the chat.
    fn render_discuss_button(&self, item: &AttentionItem, cx: &mut Context<Self>) -> Option<AnyElement> {
        if item.kind != AttentionKind::Decision {
            return None;
        }
        let decision = self.loaded.pending.iter().find(|d| d.id == item.id)?.clone();
        decision.triage_conversation_id?;
        Some(
            Button::new(SharedString::from(format!("unified-requests-discuss-{}", item.id)))
                .label("Discuss")
                .xsmall()
                .tooltip("Talk it over with the session that read this request, in the chat")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.discuss(&decision, None, Source::Click, cx);
                }))
                .into_any_element(),
        )
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
                .accessibility_label("Continue in a terminal")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.continue_in_terminal(&item, conversation, Source::Click, window, cx);
                }))
                .into_any_element(),
        )
    }

    /// **Answered elsewhere**: the user settled this request outside the
    /// app (typically in the terminal session beside it), so it is dismissed
    /// without an answer going to the agent. Offered for the kinds whose
    /// answer would otherwise be sent (decisions, handed-back plan steps).
    fn render_resolved_elsewhere_button(&self, item: &AttentionItem, cx: &mut Context<Self>) -> Option<AnyElement> {
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
        let kind = item.kind;
        let fleet = self.fleet.clone();
        off_thread(
            cx,
            move || match kind {
                AttentionKind::Decision => AgentRuns::resolve_decision_on(&fleet, id),
                AttentionKind::PlanStep => AgentRuns::resolve_plan_step_on(&fleet, id),
            },
            |this, result, cx| {
                this.set_error("dismiss the request", result);
                this.reload(cx);
            },
        );
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
    ///
    /// Not editing, the field is a text input in the accessibility tree named
    /// "Answer: <question>" (`textinput:"Answer: <question>"`): clicking or
    /// focusing it enters edit mode, and `set-value` puts the text in the draft
    /// and enters edit mode, ready for Ctrl+Enter. Editing, the textarea carries
    /// the same name.
    fn render_freeform(&self, decision_id: Uuid, question: &str, only: bool, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::{Window as GpuiWindow, accesskit};
        let editing = self.freeform_editing == Some(decision_id);
        key_context::set_input_tab_stop(&self.freeform_input, editing, cx);
        let name = freeform_name(question);
        let this = cx.entity().downgrade();
        let begin = {
            let this = this.clone();
            move |_: Option<&accesskit::ActionData>, window: &mut GpuiWindow, cx: &mut App| {
                this.update(cx, |this, cx| this.begin_freeform_edit(decision_id, window, cx)).ok();
            }
        };
        let set_value = move |data: Option<&accesskit::ActionData>, window: &mut GpuiWindow, cx: &mut App| {
            let Some(accesskit::ActionData::Value(text)) = data else { return };
            let text = text.to_string();
            this.update(cx, |this, cx| {
                FreeformDrafts::set(decision_id, text, cx);
                // Make the input load the draft even if it held this decision's.
                this.freeform_draft_for = None;
                this.begin_freeform_edit(decision_id, window, cx);
            })
            .ok();
        };
        let base = div()
            .id(SharedString::from(format!("unified-decisions-freeform-{decision_id}")))
            .key_context(FREEFORM_TAG)
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| this.begin_freeform_edit(decision_id, window, cx)),
            );
        if editing {
            return base
                .child(
                    Textarea::new(&self.freeform_input)
                        .aria_label(name)
                        .disabled(false)
                        .w_full()
                        .h(gpui::px(FREEFORM_HEIGHT)),
                )
                .into_any_element();
        }
        let draft = Some(FreeformDrafts::get(decision_id, cx)).filter(|text| !text.trim().is_empty());
        base.role(accesskit::Role::MultilineTextInput)
            .aria_label(name)
            .aria_value(draft.clone().unwrap_or_default())
            .on_a11y_action(accesskit::Action::Click, begin.clone())
            .on_a11y_action(accesskit::Action::Focus, begin)
            .on_a11y_action(accesskit::Action::SetValue, set_value)
            .child(match draft {
                Some(draft) => div().text_xs().child(draft.clone()).record_text(draft).into_any_element(),
                None => div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if only { "Enter to type your answer…" } else { "Enter to answer freely…" })
                    .record_text(if only { "Enter to type your answer…" } else { "Enter to answer freely…" })
                    .into_any_element(),
            })
            .into_any_element()
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
        // The steps this request holds up, listed under it: one cause, one
        // request (`Decision::blocks`).
        let held_up: Vec<String> = decision
            .blocks
            .iter()
            .filter_map(|id| self.loaded.handoff_steps.iter().find(|s| s.id == *id))
            .map(|step| {
                let why = step.note.as_deref().map(first_line).filter(|n| !n.is_empty());
                match why {
                    Some(why) => format!("- {} ({why})", first_line(&step.body)),
                    None => format!("- {}", first_line(&step.body)),
                }
            })
            .collect();
        let brief = tod_core::request_brief::of(&decision);
        self.card(format!("unified-decisions-pending-{}", decision.id), cx)
            .child(capped_scroll(
                format!("unified-decisions-question-scroll-{}", decision.id),
                output_max_height(window),
                match &brief {
                    // Written by the app from the agent's fields
                    // (`tod_core::request_brief`), never by a model.
                    Some(brief) => self.render_brief(decision.id, brief, self.flag_for(&decision, cx), window, cx),
                    None => selectable_markdown_flaggable(
                        SharedString::from(format!("unified-decisions-question-{}", decision.id)),
                        decision.question.clone(),
                        self.flag_for(&decision, cx),
                        window,
                        cx,
                    )
                    .into_any_element(),
                },
            ))
            .when(!decision.new_evidence.trim().is_empty(), |el| {
                el.child(style::text_muted(div().text_sm()).child(selectable_markdown(
                    SharedString::from(format!("unified-decisions-new-evidence-{}", decision.id)),
                    format!("New since the last answer: {}", decision.new_evidence.trim()),
                    window,
                    cx,
                )))
            })
            .when(!held_up.is_empty(), |el| {
                el.child(style::text_muted(div().text_sm()).child(selectable_markdown(
                    SharedString::from(format!("unified-decisions-held-up-{}", decision.id)),
                    format!("Holds up {} plan step(s); answering opens them again:\n\n{}", held_up.len(), held_up.join("\n")),
                    window,
                    cx,
                )))
            })
            .when_some(edit_preview(&decision), |el, text| {
                el.child(style::text_muted(div().text_sm()).child(selectable_markdown(
                    SharedString::from(format!("unified-decisions-edits-{}", decision.id)),
                    text,
                    window,
                    cx,
                )))
            })
            .when(tod_core::environment_request::is_request(&decision), |el| {
                el.child(self.render_credential_buttons(&decision, cx))
            })
            .when(
                !decision.options.is_empty() && !tod_core::environment_request::is_request(&decision),
                |el| el.child(self.render_options(&decision, cx)),
            )
            .when(!tod_core::environment_request::is_request(&decision), |el| {
                el.child(self.render_freeform(decision.id, &decision.question, decision.options.is_empty(), cx))
            })
            .child(footer)
            .into_any_element()
    }

    /// An access, broken or capability request, rendered from its fields:
    /// the need first, the command in a copy block, then the error, what was
    /// tried and what happens after.
    fn render_brief(
        &self,
        decision_id: Uuid,
        brief: &tod_core::request_brief::RequestBrief,
        flag: Option<(&'static str, FlagHandler)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut col = div().flex().flex_col().gap_2().w_full().min_w_0().child(selectable_markdown_flaggable(
            SharedString::from(format!("unified-decisions-need-{decision_id}")),
            brief.need.clone(),
            flag.clone(),
            window,
            cx,
        ));
        if let Some(command) = &brief.command {
            let copy = command.clone();
            col = col.child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .p_2()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_sm()
                            .child(selectable_text(
                                SharedString::from(format!("unified-decisions-command-{decision_id}")),
                                command.clone(),
                                window,
                                cx,
                            )),
                    )
                    .child(
                        Button::new(SharedString::from(format!("unified-decisions-copy-command-{decision_id}")))
                            .label("Copy")
                            .small()
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()));
                            }),
                    ),
            );
        }
        let details = brief.details_markdown();
        if !details.is_empty() {
            col = col.child(selectable_markdown_flaggable(
                SharedString::from(format!("unified-decisions-brief-details-{decision_id}")),
                details,
                flag,
                window,
                cx,
            ));
        }
        col.into_any_element()
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
                card = card.child(style::text_muted(div().text_sm()).child(selectable_markdown(
                    SharedString::from(format!("unified-decisions-plan-step-why-{}", item.id)),
                    why.join("\n\n"),
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

    fn render_item(&self, item: &AttentionItem, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match item.kind {
            AttentionKind::Decision => self.render_decision_card(item, window, cx),
            AttentionKind::PlanStep => self.render_plan_step_card(item, window, cx),
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
            .child(div().text_sm().child(selectable_markdown(
                ("unified-decisions-log-question", entry.answer.id as u64),
                tod_core::request_brief::headline(&decision),
                window,
                cx,
            )))
            .child(div().w_full().min_w_0().text_xs().child(selectable_text(
                ("unified-decisions-log-answer", entry.answer.id as u64),
                format!("Answered: {chosen}"),
                window,
                cx,
            )))
            .when_some(entry.answer.rationale.clone().filter(|r| !r.trim().is_empty()), |el, rationale| {
                el.child(div().w_full().min_w_0().text_xs().text_color(muted).child(selectable_text(
                    ("unified-decisions-log-rationale", entry.answer.id as u64),
                    format!("Why: {rationale}"),
                    window,
                    cx,
                )))
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child({
                        let times = log_times_label(&decision, &entry.answer, chrono::Local::now().date_naive());
                        div().text_xs().text_color(muted).child(times.clone()).record_text(times)
                    })
                    .child(div().flex_1())
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
                    .when_some(decision.triage_conversation_id, |el, conversation| {
                        let node_id = decision.node_id;
                        el.child(
                            Button::new(("unified-decisions-triage-session", entry.answer.id as u64))
                                .label("Triage session")
                                .small()
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(PanelDiscussRequest { node_id, conversation, message: None });
                                })),
                        )
                    })
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
            .when_some(
                send_back_target(&decision, self.loaded.lifecycle.as_deref()).filter(|_| changing),
                |el, state| {
                    let decision = decision.clone();
                    el.child(
                        Button::new(("unified-decisions-send-back", entry.answer.id as u64))
                            .label(format!("Also send the task back to {state}"))
                            .small()
                            .tooltip("The task went on past the phase that asked, without this answer")
                            .on_click(cx.listener(move |this, _, _, cx| this.send_back(&decision, state, cx))),
                    )
                },
            )
            .when(changing, |el| {
                el.child(self.render_options(&decision, cx))
                    .child(div().flex().flex_wrap().gap_2().children(evidence))
                    .child(self.render_freeform(decision_id, &decision.question, decision.options.is_empty(), cx))
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
    div().text_xs().child(label).record_text(label)
}

/// A log entry's times and who answered: "Asked Nov 12 14:03 · Answered
/// 09:15 · user", in local time (see [`when_label`]).
pub fn log_times_label(decision: &Decision, answer: &DecisionAnswer, today: chrono::NaiveDate) -> String {
    // An answer tod or a triage agent gave for the user says so
    // (`decision_answers.decided_by`), and not only who wrote the row.
    let by = if answer.decided_by == tod_store::decisions::DECIDED_BY_USER {
        answer.actor.clone()
    } else {
        format!("decided by {}", answer.decided_by)
    };
    format!(
        "Asked {} · Answered {} · {}",
        when_label(decision.created_at, today),
        when_label(answer.answered_at, today),
        by
    )
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
            .when_some(self.flag_note.clone(), |el, note| {
                el.child(style::text_muted(div().text_sm()).child(selectable_text(
                    "unified-decisions-flag-note",
                    note,
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
            RequestReason::Broken,
            RequestReason::Other,
        ] {
            assert!(!reason_label(reason).is_empty());
        }
    }

    /// The footer shows only the time for a request raised today, and the
    /// date too for an older one.
    #[test]
    fn when_label_shows_the_time_today_and_the_date_otherwise() {
        use chrono::{Local, TimeZone};
        let at = Local.with_ymd_and_hms(2026, 11, 5, 14, 32, 0).single().unwrap();
        let ms = at.timestamp_millis();
        assert_eq!(when_label(ms, at.date_naive()), "14:32");
        assert_eq!(when_label(ms, at.date_naive().succ_opt().unwrap()), "Nov 5 14:32");
    }

    /// An answered request shows both when it was asked and when it was answered.
    #[test]
    fn log_times_show_when_asked_and_when_answered() {
        use chrono::{Local, TimeZone};
        let asked = Local.with_ymd_and_hms(2026, 11, 4, 8, 5, 0).single().unwrap();
        let answered = Local.with_ymd_and_hms(2026, 11, 5, 14, 32, 0).single().unwrap();
        let decision = Decision {
            id: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
            conversation_id: None,
            protocol: None,
            question: "Which?".into(),
            options: Vec::new(),
            evidence: Vec::new(),
            status: "answered".into(),
            created_at: asked.timestamp_millis(),
            reason: "intent".into(),
            moves: Vec::new(),
            edits: Vec::new(),
            blocks: Vec::new(),
            new_evidence: String::new(),
            brief: Default::default(),
            error_signature: String::new(),
            cause: String::new(),
            triage_conversation_id: None,
            asked: None,
        };
        let answer = DecisionAnswer {
            id: 1,
            decision_id: decision.id,
            option: None,
            text: Some("this one".into()),
            actor: "user".into(),
            answered_at: answered.timestamp_millis(),
            decided_by: tod_store::decisions::DECIDED_BY_USER.into(),
            conversation_id: None,
            rationale: None,
        };
        assert_eq!(
            log_times_label(&decision, &answer, answered.date_naive()),
            "Asked Nov 4 08:05 · Answered 14:32 · user"
        );
    }

    #[test]
    fn an_answer_tod_gave_says_it_was_decided_by_tod() {
        use chrono::TimeZone;
        let at = chrono::Local.with_ymd_and_hms(2026, 11, 4, 14, 32, 0).single().unwrap();
        let decision = Decision {
            id: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
            conversation_id: None,
            protocol: Some("bot_silence".into()),
            question: "Proceed?".into(),
            options: vec!["Proceed".into()],
            evidence: Vec::new(),
            status: "answered".into(),
            created_at: at.timestamp_millis(),
            reason: "broken".into(),
            moves: Vec::new(),
            edits: Vec::new(),
            blocks: Vec::new(),
            new_evidence: String::new(),
            brief: Default::default(),
            error_signature: String::new(),
            cause: String::new(),
            triage_conversation_id: None,
            asked: None,
        };
        let answer = DecisionAnswer {
            id: 2,
            decision_id: decision.id,
            option: Some(1),
            text: None,
            actor: "tod".into(),
            answered_at: at.timestamp_millis(),
            decided_by: tod_store::decisions::DECIDED_BY_TOD.into(),
            conversation_id: None,
            rationale: Some("asked 3 times".into()),
        };
        assert!(log_times_label(&decision, &answer, at.date_naive()).ends_with("decided by tod"));
    }

    #[test]
    fn the_options_obligation_edits_are_shown_with_the_request() {
        let mut decision = Decision {
            id: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
            conversation_id: None,
            protocol: None,
            question: "Which check?".into(),
            options: vec!["a".into(), "b".into()],
            evidence: Vec::new(),
            status: "pending".into(),
            created_at: 0,
            reason: "intent".into(),
            moves: Vec::new(),
            edits: Vec::new(),
            blocks: Vec::new(),
            new_evidence: String::new(),
            brief: Default::default(),
            error_signature: String::new(),
            cause: String::new(),
            triage_conversation_id: None,
            asked: None,
        };
        assert_eq!(edit_preview(&decision), None);
        decision.edits.push(tod_store::decisions::OptionEdit {
            option: 2,
            obligation_id: Uuid::new_v4(),
            body: "uses the in-memory comparison".into(),
            was: Some("uses last_synced_at".into()),
            affirm: false,
        });
        let text = edit_preview(&decision).unwrap();
        assert!(text.contains("Option 2"), "{text}");
        assert!(text.contains("from: uses last_synced_at"), "{text}");
        assert!(text.contains("to: uses the in-memory comparison"), "{text}");
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

    fn escalated(protocol: &str, triage: bool) -> Decision {
        Decision {
            id: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
            conversation_id: None,
            protocol: Some(protocol.into()),
            question: "Brief: which?".into(),
            options: vec!["A".into(), "B".into()],
            evidence: Vec::new(),
            status: "pending".into(),
            created_at: 0,
            reason: "intent".into(),
            moves: Vec::new(),
            edits: Vec::new(),
            blocks: Vec::new(),
            new_evidence: String::new(),
            brief: Default::default(),
            error_signature: String::new(),
            cause: String::new(),
            triage_conversation_id: triage.then(Uuid::new_v4),
            asked: None,
        }
    }

    #[test]
    fn a_changed_answer_offers_the_send_back_only_past_the_asking_phase() {
        let asked_in_implementation = escalated("implementation", true);
        assert_eq!(send_back_target(&asked_in_implementation, Some("review")), Some("active"));
        assert_eq!(send_back_target(&asked_in_implementation, Some("active")), None);
        assert_eq!(send_back_target(&asked_in_implementation, Some("planning")), None);
        // A phase agent's state is not recorded, so there is nothing to offer.
        assert_eq!(send_back_target(&escalated("phase", true), Some("review")), None);
    }

    #[gpui::test]
    fn number_keys_skip_an_escalated_request_and_discuss_opens_its_session(cx: &mut gpui::TestAppContext) {
        use crate::views::rows::fixture::Fixture;
        use std::cell::RefCell;
        use std::rc::Rc;
        use tod_store::interview::InterviewCommand;
        use tod_store::triage::Op;

        let fixture = Fixture::new();
        let ask = fixture
            .store
            .interview(
                "autopilot",
                InterviewCommand::AskDecision {
                    node_id: fixture.node_id,
                    conversation_id: None,
                    protocol: Some("implementation".into()),
                    decision: tod_store::decisions::NewDecision {
                        question: "Keep the old name?".into(),
                        options: vec!["Keep it".into(), "Drop it".into()],
                        reason: "intent".into(),
                        guard_reask: true,
                        ..Default::default()
                    },
                },
            )
            .unwrap();
        let decision_id = Uuid::parse_str(ask.get("id").and_then(|v| v.as_str()).unwrap()).unwrap();
        let session = Uuid::new_v4();
        fixture
            .store
            .interview("autopilot", InterviewCommand::Triage(Op::Start { decision: decision_id, conversation: session }))
            .unwrap();
        fixture
            .store
            .interview(
                &tod_store::conversation::actor_for(session),
                InterviewCommand::Triage(Op::Escalate {
                    decision: decision_id,
                    conversation: session,
                    brief: "Brief: the old name or a new one?".into(),
                    options: None,
                }),
            )
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

        let opened = Rc::new(RefCell::new(Vec::new()));
        let sink = opened.clone();
        let _sub = cx.update(|_, app| {
            app.subscribe(&requests, move |_, event: &PanelDiscussRequest, _| sink.borrow_mut().push(event.clone()))
        });
        requests.update_in(cx, |r, window, cx| r.answer_option_key(&DecisionOptionKey(1), window, cx));
        cx.run_until_parked();
        let still = fixture.store.read(|conn| Ok(DecisionRepo::new(conn).get(decision_id)?)).unwrap().unwrap();
        assert_eq!(still.status, DECISION_PENDING, "a number key never answers an escalated request");

        let decision = requests.read_with(cx, |r, _| r.loaded.pending[0].clone());
        requests.update(cx, |r, cx| r.discuss(&decision, None, Source::Click, cx));
        requests.update(cx, |r, cx| r.flag_confusing(decision_id, " the old name ".into(), cx));
        let events = opened.borrow();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].conversation, session);
        assert_eq!(events[0].message, None);
        assert!(events[1].message.as_deref().is_some_and(|m| m.contains("the old name")));
    }

    #[gpui::test]
    fn this_is_confusing_on_a_request_no_triage_session_read_records_a_lesson(cx: &mut gpui::TestAppContext) {
        use crate::views::rows::fixture::Fixture;
        use std::cell::RefCell;
        use std::rc::Rc;
        use tod_store::interview::InterviewCommand;

        let fixture = Fixture::new();
        // An access request goes straight to the user: no triage session.
        let ask = fixture
            .store
            .interview(
                "autopilot",
                InterviewCommand::AskDecision {
                    node_id: fixture.node_id,
                    conversation_id: None,
                    protocol: Some("implementation".into()),
                    decision: tod_store::decisions::NewDecision {
                        question: "Give me read access to the v2 index?".into(),
                        options: vec!["Done".into(), "No".into()],
                        reason: "access".into(),
                        guard_reask: true,
                        ..Default::default()
                    },
                },
            )
            .unwrap();
        let decision_id = Uuid::parse_str(ask.get("id").and_then(|v| v.as_str()).unwrap()).unwrap();

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

        let opened = Rc::new(RefCell::new(0usize));
        let sink = opened.clone();
        let _sub = cx.update(|_, app| {
            app.subscribe(&requests, move |_, _: &PanelDiscussRequest, _| *sink.borrow_mut() += 1)
        });
        // Every request offers the flag, with or without a triage session.
        let decision = requests.read_with(cx, |r, _| r.loaded.pending[0].clone());
        assert!(decision.triage_conversation_id.is_none());
        requests.update(cx, |r, cx| r.flag_confusing(decision_id, " what is v2? ".into(), cx));
        cx.run_until_parked();
        assert_eq!(*opened.borrow(), 0, "no session to open: nothing goes to the chat");
        let lessons = fixture.store.read(|conn| tod_store::lessons::for_decision(conn, decision_id)).unwrap();
        assert_eq!(lessons.len(), 1);
        assert_eq!(lessons[0].source, tod_store::lessons::SOURCE_USER);
        assert_eq!(lessons[0].user_quote, "what is v2?");
        assert_eq!(lessons[0].decision_id, Some(decision_id));
        assert!(requests.read_with(cx, |r, _| r.flag_note.is_some() && r.last_error.is_none()));
    }
}

/// Most rows of agent output a request shows before it scrolls.
const OUTPUT_MAX_ROWS: f32 = 12.;

fn output_max_height(window: &Window) -> gpui::Pixels {
    (window.line_height() * OUTPUT_MAX_ROWS).min(window.viewport_size().height)
}

/// `content` in a box no taller than 12 text rows or the window, whichever is
/// shorter, with a scrollbar that appears only when the content overflows.
fn capped_scroll(id: String, max_h: gpui::Pixels, content: impl IntoElement) -> AnyElement {
    div()
        .id(SharedString::from(id))
        .max_h(max_h)
        .overflow_y_scrollbar()
        .child(content)
        .into_any_element()
}
