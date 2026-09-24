//! The decisions panel (`doc/ui/unified-view.md` "Decisions"): the
//! singleton panel where the user answers *everything* a node is waiting on
//! — not only rows from `tod_store::decisions`, but every kind
//! `tod_core::attention` knows about: pending decisions, plan steps handed
//! back to the user, open review findings, and a gate check needing a human.
//! It always shows the currently selected node's items — `UnifiedView`
//! retargets it (`DecisionsPanel::set_node`) whenever the tree selection
//! changes, so this column does not need its own target in `PanelKind`.
//!
//! The pending list ([`tod_core::attention::for_node`]) is oldest first;
//! number keys 1-9 answer the *top* item when it has options
//! (`doc/ui/unified-view.md` "Keys"). Below it sits the append-only decision
//! answer log: one entry per decision answer ever given (never per decision —
//! a change of mind adds a new entry, the old one stays), each with
//! **Change** (re-ask the same decision; the new answer is a new
//! `decision_answers` row, nothing is reversed) and a link to the
//! conversation that asked. Plan steps and findings answer directly (their
//! own status *is* the record — `tod_store::outline::repos::plan_steps` and
//! `tod_store::review`); a gate criterion's waive is recorded by
//! `tod_store::outline::repos::gate`.
//!
//! Each kind answers through the existing code path for it, never a new
//! mutation: a decision through `AgentRuns::answer_decision`; a plan step
//! through `AgentRuns::answer_plan_step_handoff` (the same message
//! `conversation::side_pane::answer_handoff` sends); a finding through
//! `AgentRuns::respond_review_finding` (the same status write
//! `conversation::side_pane::respond_to_finding` makes); a gate criterion
//! through the one shared `LifecycleController::waive` — the same entity the
//! conversation view and lifecycle panel use, passed in from
//! `crate::app::window`. Every answer records a journey `UserAction` with a
//! `Presented` snapshot of the choices shown (`crate::conversation::lifecycle`
//! is the pattern this follows).

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, actions, div, prelude::FluentBuilder,
};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable};
use tod_core::attention::{AttentionItem, AttentionKind};
use tod_core::conversation::implement::HandoffAnswer;
use tod_journey::{Presented, PresentedAction};
use tod_store::decisions::{DECISION_PENDING, Decision, DecisionAnswer, DecisionRepo, EvidenceRef};
use tod_store::fleet::FleetStore;
use tod_store::interview::short_id;
use tod_store::outline::repos::plan_steps::HandoffReason;
use tod_store::outline::repos::PlanStepRepo;
use tod_store::outline::PlanStep;
use tod_store::review::{ReviewFinding, ReviewRepo, FINDING_DECLINED, FINDING_FIXED, FINDING_OUT_OF_SCOPE};
use uuid::Uuid;

use crate::ui::agent_runs::AgentRuns;
use crate::ui::journey::{Source, record_action};
use crate::ui::key_context;
use crate::ui::selectable_text::{selectable_markdown, selectable_text};
use crate::unified::columns::PanelKind;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};
use crate::views::lifecycle_control::LifecycleController;

pub const UNIFIED_DECISIONS_CONTEXT: &str = "UnifiedDecisionsPanel";
/// Key-context tag for whichever freeform answer field is currently in edit
/// mode, so Enter-to-commit only fires for that one field
/// (`key_context::including_tag`).
const FREEFORM_TAG: &str = "DecisionsFreeform";

/// Answer the top pending decision with option `.0` (1-based, matching the
/// numbered options shown).
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = unified_decisions_panel, no_json)]
pub struct DecisionOptionKey(pub usize);

actions!(
    unified_decisions_panel,
    [
        DecisionsActivate,
        DecisionsCtrlActivate,
        DecisionsFreeformEscape,
        DecisionsLinkPrev,
        DecisionsLinkNext,
    ]
);

/// Registers the decisions panel's own keys: digits 1-9 answer the top
/// pending decision (`doc/ui/unified-view.md` "Keys"); Up/Down move a
/// keyboard "stop" across the current decision's evidence links and its
/// freeform field; Enter activates the stop (opens an evidence link, or
/// enters freeform edit mode), Ctrl+Enter does the ctrl-click equivalent
/// (`doc/ui/unified-view.md`: "Evidence links open panels by the column
/// rule: Enter acts like a click, Ctrl+Enter like a Ctrl+click"). Call once
/// alongside `unified::register_unified_keyboard_bindings`.
pub fn register_decisions_panel_keyboard_bindings(cx: &mut App) {
    let outside_input = Some(key_context::excluding_input(UNIFIED_DECISIONS_CONTEXT));
    let with_input = Some(key_context::including_tag(
        UNIFIED_DECISIONS_CONTEXT,
        FREEFORM_TAG,
    ));
    let digit_bindings = (1..=9).map(|n| {
        KeyBinding::new(
            &n.to_string(),
            DecisionOptionKey(n as usize),
            outside_input,
        )
    });
    cx.bind_keys(digit_bindings);
    cx.bind_keys([
        KeyBinding::new("enter", DecisionsActivate, outside_input),
        KeyBinding::new("ctrl-enter", DecisionsCtrlActivate, outside_input),
        KeyBinding::new("escape", DecisionsFreeformEscape, with_input),
        KeyBinding::new("up", DecisionsLinkPrev, outside_input),
        KeyBinding::new("down", DecisionsLinkNext, outside_input),
    ]);
}

/// One answer in the append-only log: the decision it answered (as it was
/// at load time) and one of its `decision_answers` rows.
#[derive(Debug, Clone)]
struct LogEntry {
    decision: Decision,
    answer: DecisionAnswer,
}

#[derive(Default)]
struct Loaded {
    pending: Vec<Decision>,
    /// Every plan step currently handed back to the user (`partial` /
    /// `blocked`), for the `PlanStep` attention items to render their own
    /// reason and options from.
    handoff_steps: Vec<PlanStep>,
    /// Every open review finding while the node is in `review`, for the
    /// `Finding` attention items.
    findings: Vec<ReviewFinding>,
    /// What the node is waiting on, oldest first, combining all four
    /// sources (`tod_core::attention::for_node`).
    items: Vec<AttentionItem>,
    /// Oldest answer first.
    log: Vec<LogEntry>,
}

fn load(fleet: &FleetStore, node_id: Uuid) -> Loaded {
    fleet
        .read(|conn| {
            let repo = DecisionRepo::new(conn);
            let pending = repo.list_pending_for_node(node_id)?;
            let all = repo.list_for_node(node_id)?;
            let mut log = Vec::new();
            for decision in all {
                if decision.status == DECISION_PENDING {
                    continue;
                }
                if let Some(with_answers) = repo.get_with_answers(decision.id)? {
                    for answer in with_answers.answers {
                        log.push(LogEntry {
                            decision: with_answers.decision.clone(),
                            answer,
                        });
                    }
                }
            }
            log.sort_by_key(|entry| entry.answer.answered_at);

            let handoff_steps: Vec<PlanStep> = PlanStepRepo::new(conn)
                .list_needs_user_for_nodes(&[node_id])?
                .into_iter()
                .map(|(step, _updated_at)| step)
                .collect();

            let lifecycle = tod_store::outline::repos::NodeRepo::new(conn)
                .get_lifecycle_for_nodes(&[node_id])?;
            let findings = if lifecycle.get(&node_id).map(String::as_str) == Some("review") {
                ReviewRepo::new(conn).list_open_for_nodes(&[node_id])?
            } else {
                Vec::new()
            };

            let items = tod_core::attention::for_node(conn, node_id)?.items;

            anyhow::Ok(Loaded {
                pending,
                handoff_steps,
                findings,
                items,
                log,
            })
        })
        .unwrap_or_default()
}

/// A link an evidence reference (or a log entry's conversation) opens, and
/// where — `None` for evidence kinds with no panel of their own yet
/// (`test_run`), shown as plain text instead.
fn evidence_target(node_id: Uuid, evidence: &EvidenceRef) -> Option<PanelKind> {
    match evidence.kind.as_str() {
        "obligation" => Some(PanelKind::Obligations(node_id)),
        "plan_step" => Some(PanelKind::Plan(node_id)),
        "conversation" => Some(PanelKind::Transcript(evidence.id)),
        "node" => Some(PanelKind::Details(evidence.id)),
        // "finding" has no panel of its own yet, and "test_run" has none at
        // all; both show as plain text below.
        _ => None,
    }
}

/// One clickable (or plain) evidence entry rendered under a decision.
struct EvidenceLink {
    label: SharedString,
    target: Option<PanelKind>,
}

fn evidence_links(node_id: Uuid, evidence: &[EvidenceRef]) -> Vec<EvidenceLink> {
    evidence
        .iter()
        .map(|e| {
            let short = e.id.simple().to_string();
            let short = &short[..short.len().min(8)];
            EvidenceLink {
                label: format!("{} {short}", e.kind).into(),
                target: evidence_target(node_id, e),
            }
        })
        .collect()
}

pub struct DecisionsPanel {
    node_id: Option<Uuid>,
    fleet: Arc<FleetStore>,
    agent_runs: Entity<AgentRuns>,
    /// The one lifecycle controller shared with the conversation view and
    /// the lifecycle panel (`.claude/CLAUDE.md`): a gate check's criteria and
    /// `waive` live here, never duplicated.
    lifecycle: Entity<LifecycleController>,
    focus_handle: FocusHandle,
    loaded: Loaded,
    /// The decision (pending, or a log entry being changed) whose freeform
    /// field is in edit mode.
    freeform_editing: Option<Uuid>,
    freeform_input: Entity<InputState>,
    /// A log entry's decision the user asked to change: shows the same
    /// option/freeform controls as a pending decision, inline in the log.
    changing: Option<Uuid>,
    /// Index into this decision's evidence links, for Enter/Ctrl+Enter
    /// (shared across whichever decision currently owns keyboard focus —
    /// the top pending one, or the one being changed).
    selected_link: usize,
    pending_refresh: bool,
    _freeform_subscription: Subscription,
    _lifecycle_subscription: Subscription,
    _poll: gpui::Task<()>,
}

impl DecisionsPanel {
    pub fn new(
        node_id: Option<Uuid>,
        fleet: Arc<FleetStore>,
        agent_runs: Entity<AgentRuns>,
        lifecycle: Entity<LifecycleController>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let freeform_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Freeform answer… (Enter to submit)")
        });
        let _freeform_subscription = cx.subscribe(&freeform_input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit_freeform(cx);
            }
        });
        // A gate criterion recorded (or waived) via the conversation view or
        // the lifecycle panel shows up here too, since all three share the
        // one controller entity.
        let _lifecycle_subscription = cx.observe(&lifecycle, |this, _, cx| {
            this.pending_refresh = true;
            cx.notify();
        });

        let poll_entity = cx.weak_entity();
        let fleet_for_poll = fleet.clone();
        let _poll = cx.spawn(async move |_, cx| {
            let mut fleet_rx = fleet_for_poll.subscribe_changes();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                let mut changed = false;
                while fleet_rx.try_recv().is_ok() {
                    changed = true;
                }
                if changed {
                    let Ok(()) = poll_entity.update(cx, |this: &mut DecisionsPanel, cx| {
                        this.pending_refresh = true;
                        cx.notify();
                    }) else {
                        break;
                    };
                }
            }
        });

        let loaded = node_id.map(|id| load(&fleet, id)).unwrap_or_default();
        if let Some(id) = node_id {
            lifecycle.update(cx, |controller, _| controller.load_persisted(&id.to_string()));
        }

        Self {
            node_id,
            fleet,
            agent_runs,
            lifecycle,
            focus_handle: cx.focus_handle(),
            loaded,
            freeform_editing: None,
            freeform_input,
            changing: None,
            selected_link: 0,
            pending_refresh: false,
            _freeform_subscription,
            _lifecycle_subscription,
            _poll,
        }
    }

    /// Retarget this column to a different node — called whenever the tree
    /// selection changes, since this panel always shows whichever node is
    /// current (`doc/ui/unified-view.md` "Decisions").
    pub fn set_node(&mut self, node_id: Option<Uuid>, _window: &mut Window, cx: &mut Context<Self>) {
        if self.node_id == node_id {
            return;
        }
        self.node_id = node_id;
        self.freeform_editing = None;
        self.changing = None;
        self.selected_link = 0;
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.loaded = self
            .node_id
            .map(|id| load(&self.fleet, id))
            .unwrap_or_default();
        if let Some(id) = self.node_id {
            self.lifecycle
                .update(cx, |controller, _| controller.load_persisted(&id.to_string()));
        }
        // A decision that finished changing, or vanished, drops any
        // in-flight editing state for it.
        if let Some(changing) = self.changing {
            if !self.loaded.log.iter().any(|e| e.decision.id == changing) {
                self.changing = None;
            }
        }
        if let Some(editing) = self.freeform_editing {
            let still_open = self.loaded.pending.iter().any(|d| d.id == editing)
                || self.changing == Some(editing);
            if !still_open {
                self.freeform_editing = None;
            }
        }
        cx.notify();
    }

    /// `Presented` snapshot for one decision's on-screen options, for the
    /// journey `UserAction` recorded with every answer
    /// (`crate::conversation::lifecycle::presented_lifecycle_controls` is
    /// the pattern).
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

    fn record_answer_journey(decision: &Decision, chosen: &str, source: Source, cx: &mut App) {
        record_action(
            cx,
            tod_store::conversation::Focus::Node(decision.node_id),
            chosen.to_string(),
            source,
            "decisions",
            Self::presented_for(decision),
        );
    }

    fn answer(
        &mut self,
        decision: Decision,
        option: Option<usize>,
        text: Option<String>,
        source: Source,
        cx: &mut Context<Self>,
    ) {
        let chosen = option
            .map(|o| o.to_string())
            .unwrap_or_else(|| "freeform".to_string());
        Self::record_answer_journey(&decision, &chosen, source, cx);
        let decision_id = decision.id;
        self.agent_runs.update(cx, |runs, cx| {
            if let Err(err) = runs.answer_decision(decision_id, option, text, cx) {
                tracing::warn!("decisions panel: failed to record answer: {err:#}");
            }
        });
        self.freeform_editing = None;
        self.changing = None;
        self.reload(cx);
    }

    /// Answer the *top* attention item with option `n` (1-based), when it
    /// has options — `doc/ui/unified-view.md` "Keys": "1, 2, 3 … Answer the
    /// top pending decision with that option." A `PlanStep` item handed back
    /// with `HandoffReason::Decision` has options too, so this answers
    /// whichever kind is on top; other kinds (a plain retry, a finding, a
    /// gate criterion) have no numbered options and are left to their own
    /// buttons.
    fn answer_option_key(&mut self, action: &DecisionOptionKey, _window: &mut Window, cx: &mut Context<Self>) {
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
                    self.answer_plan_step(
                        &step,
                        HandoffAnswer::Choose(action.0 - 1),
                        Source::Keyboard,
                        cx,
                    );
                }
            }
            AttentionKind::Finding | AttentionKind::Gate => {}
        }
    }

    fn click_option(&mut self, decision: Decision, option: usize, cx: &mut Context<Self>) {
        self.answer(decision, Some(option), None, Source::Click, cx);
    }

    /// `Presented` snapshot for one plan step's on-screen options/buttons.
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

    /// Answer a plan step the agent handed back — `AgentRuns::answer_plan_step_handoff`
    /// is the same code path `conversation::side_pane::answer_handoff` uses.
    fn answer_plan_step(
        &mut self,
        step: &PlanStep,
        answer: HandoffAnswer,
        source: Source,
        cx: &mut Context<Self>,
    ) {
        let chosen = format!("{answer:?}");
        record_action(
            cx,
            tod_store::conversation::Focus::Node(step.node_id),
            chosen,
            source,
            "decisions",
            Self::presented_for_step(step),
        );
        let step_id = step.id;
        self.agent_runs.update(cx, |runs, cx| {
            if let Err(err) = runs.answer_plan_step_handoff(step_id, answer, cx) {
                tracing::warn!("decisions panel: failed to answer plan step: {err:#}");
            }
        });
        self.reload(cx);
    }

    /// `Presented` snapshot for one finding's status buttons.
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

    /// Answer an open review finding — `AgentRuns::respond_review_finding` is
    /// the same status write `conversation::side_pane::respond_to_finding`
    /// makes.
    fn answer_finding(&mut self, finding: &ReviewFinding, status: &str, source: Source, cx: &mut Context<Self>) {
        record_action(
            cx,
            tod_store::conversation::Focus::Node(finding.node_id),
            status.to_string(),
            source,
            "decisions",
            Self::presented_for_finding(finding),
        );
        let finding_id = finding.id;
        let status = status.to_string();
        self.agent_runs.update(cx, |runs, _| {
            if let Err(err) = runs.respond_review_finding(finding_id, &status) {
                tracing::warn!("decisions panel: failed to answer finding: {err:#}");
            }
        });
        self.reload(cx);
    }

    /// Waive one failing gate criterion, through the shared
    /// [`LifecycleController`] — the same `waive` the lifecycle panel calls.
    fn waive_criterion(&mut self, node_id: Uuid, criterion: &crate::views::lifecycle_control::CriterionOutcome, source: Source, cx: &mut Context<Self>) {
        record_action(
            cx,
            tod_store::conversation::Focus::Node(node_id),
            "waive".to_string(),
            source,
            "decisions",
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
        self.lifecycle
            .update(cx, |controller, cx| controller.waive(&task_id, criterion_id, cx));
        self.reload(cx);
    }

    fn begin_freeform_edit(&mut self, decision_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.freeform_editing = Some(decision_id);
        self.freeform_input.update(cx, |input, cx| {
            input.set_value(String::new(), window, cx);
        });
        cx.notify();
        let input = self.freeform_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    /// The decision keyboard "stops" (Up/Down, Enter/Ctrl+Enter) currently
    /// act on: whichever decision is being changed in the log, else the top
    /// pending one.
    fn current_decision(&self) -> Option<Decision> {
        if let Some(changing) = self.changing {
            self.loaded
                .log
                .iter()
                .find(|e| e.decision.id == changing)
                .map(|e| e.decision.clone())
        } else {
            self.loaded.pending.first().cloned()
        }
    }

    /// Enter/Ctrl+Enter on the current decision's focused stop: its
    /// evidence links, then its freeform field.
    fn activate(&mut self, ctrl: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(decision) = self.current_decision() else {
            return;
        };
        let links = evidence_links(decision.node_id, &decision.evidence);
        if let Some(link) = links.get(self.selected_link) {
            if let Some(target) = link.target {
                self.open(target, ctrl, cx);
            }
            return;
        }
        // Past the evidence links: the freeform stop.
        self.begin_freeform_edit(decision.id, window, cx);
    }

    fn on_activate(&mut self, _: &DecisionsActivate, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(false, window, cx);
    }

    fn on_ctrl_activate(
        &mut self,
        _: &DecisionsCtrlActivate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(true, window, cx);
    }

    fn exit_freeform_edit(
        &mut self,
        _: &DecisionsFreeformEscape,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.freeform_editing = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn submit_freeform(&mut self, cx: &mut Context<Self>) {
        let Some(decision_id) = self.freeform_editing else {
            return;
        };
        let text = self.freeform_input.read(cx).text().to_string();
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let decision = self
            .loaded
            .pending
            .iter()
            .find(|d| d.id == decision_id)
            .or_else(|| {
                self.loaded
                    .log
                    .iter()
                    .find(|e| e.decision.id == decision_id)
                    .map(|e| &e.decision)
            })
            .cloned();
        let Some(decision) = decision else { return };
        self.answer(decision, None, Some(text), Source::Keyboard, cx);
    }

    /// Start (or clear) re-asking an already-answered decision from a log
    /// entry: `doc/ui/unified-view.md` "Change on an entry asks the question
    /// again, and the new answer is sent to the agent as a new action and
    /// logged as a new entry." Nothing about the old answer is touched —
    /// `AgentRuns::answer_decision` always appends.
    fn start_change(&mut self, decision_id: Uuid, cx: &mut Context<Self>) {
        self.changing = Some(decision_id);
        self.freeform_editing = None;
        self.selected_link = 0;
        cx.notify();
    }

    fn cancel_change(&mut self, cx: &mut Context<Self>) {
        self.changing = None;
        self.freeform_editing = None;
        cx.notify();
    }

    fn open(&self, target: PanelKind, ctrl: bool, cx: &mut Context<Self>) {
        cx.emit(PanelOpenRequest { target, ctrl });
    }

    /// The number of keyboard stops on the current decision: its evidence
    /// links, plus one for the freeform field.
    fn stop_count(&self) -> usize {
        self.current_decision()
            .map(|d| evidence_links(d.node_id, &d.evidence).len() + 1)
            .unwrap_or(0)
    }

    fn link_prev(&mut self, _: &DecisionsLinkPrev, _window: &mut Window, cx: &mut Context<Self>) {
        self.selected_link = self.selected_link.saturating_sub(1);
        cx.notify();
    }

    fn link_next(&mut self, _: &DecisionsLinkNext, _window: &mut Window, cx: &mut Context<Self>) {
        let max = self.stop_count().saturating_sub(1);
        self.selected_link = (self.selected_link + 1).min(max);
        cx.notify();
    }

    fn render_options(
        &self,
        decision: &Decision,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let decision = decision.clone();
        div().flex().flex_wrap().gap_1().children(
            decision
                .options
                .clone()
                .into_iter()
                .enumerate()
                .map(|(ix, label)| {
                    let option = ix + 1;
                    let decision = decision.clone();
                    Button::new(SharedString::from(format!("unified-decisions-option-{}-{option}", decision.id)))
                        .label(format!("{option}. {label}"))
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.click_option(decision.clone(), option, cx);
                        }))
                }),
        )
    }

    fn render_evidence(&self, decision: &Decision, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let accent = cx.theme().accent;
        let links = evidence_links(decision.node_id, &decision.evidence);
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(links.into_iter().enumerate().map(|(ix, link)| {
                let selected = ix == self.selected_link;
                match link.target {
                    Some(target) => div()
                        .id(SharedString::from(format!("unified-decisions-evidence-{}-{ix}", decision.id)))
                        .text_xs()
                        .when(selected, |el| el.text_color(accent))
                        .when(!selected, |el| el.text_color(muted))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.selected_link = ix;
                                let ctrl = event.modifiers.control || event.modifiers.platform;
                                this.open(target, ctrl, cx);
                            }),
                        )
                        .child(link.label)
                        .into_any_element(),
                    None => div()
                        .text_xs()
                        .text_color(muted)
                        .child(link.label)
                        .into_any_element(),
                }
            }))
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
                cx.listener(move |this, _, window, cx| {
                    this.begin_freeform_edit(decision_id, window, cx);
                }),
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

    fn render_pending_decision(
        &self,
        decision: &Decision,
        is_top: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let radius = cx.theme().radius;
        div()
            .id(SharedString::from(format!("unified-decisions-pending-{}", decision.id)))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_1()
            .border_color(border)
            .rounded(radius)
            .child(selectable_markdown(
                SharedString::from(format!("unified-decisions-question-{}", decision.id)),
                decision.question.clone(),
                window,
                cx,
            ))
            .child(self.render_options(decision, cx))
            .child(self.render_evidence(decision, cx))
            .child(self.render_freeform(decision.id, cx))
            .when(is_top, |el| el)
    }

    fn render_log_entry(
        &self,
        entry: &LogEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let decision = entry.decision.clone();
        let decision_id = decision.id;
        let chosen = describe_answer(&decision, &entry.answer);
        let changing = self.changing == Some(decision_id);
        div()
            .id(("unified-decisions-log-entry", entry.answer.id as u64))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_b_1()
            .border_color(border)
            .child(
                div().text_sm().child(selectable_text(
                    ("unified-decisions-log-question", entry.answer.id as u64),
                    decision.question.clone(),
                    window,
                    cx,
                )),
            )
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
                    .child(self.render_evidence(&decision, cx))
                    .child(self.render_freeform(decision_id, cx))
            })
    }

    /// A card for a plan step handed back to the user: its reason, and
    /// buttons to answer it (`HandoffReason::Decision`'s options as
    /// numbered "Choose" buttons, a conflict's obligations as "Keep"
    /// buttons, or a single Retry for access/no reason).
    fn render_plan_step_card(
        &self,
        item: &AttentionItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let radius = cx.theme().radius;
        let step = self.loaded.handoff_steps.iter().find(|s| s.id == item.id).cloned();
        let mut card = div()
            .id(SharedString::from(format!("unified-decisions-plan-step-{}", item.id)))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_1()
            .border_color(border)
            .rounded(radius)
            .child(kind_badge("Plan step"))
            .child(selectable_markdown(
                SharedString::from(format!("unified-decisions-plan-step-summary-{}", item.id)),
                item.summary.clone(),
                window,
                cx,
            ));
        let Some(step) = step else {
            return card.into_any_element();
        };
        let buttons = div().flex().flex_wrap().gap_1();
        let buttons = match &step.reason {
            Some(HandoffReason::Decision { options }) => buttons.children(
                options.iter().enumerate().map(|(ix, label)| {
                    let step = step.clone();
                    Button::new(SharedString::from(format!(
                        "unified-decisions-step-choose-{}-{ix}",
                        step.id
                    )))
                    .label(format!("{}. {label}", ix + 1))
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.answer_plan_step(&step, HandoffAnswer::Choose(ix), Source::Click, cx);
                    }))
                }),
            ),
            Some(HandoffReason::Conflict { obligations }) => buttons.children(
                obligations.iter().map(|obligation| {
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
                }),
            ),
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
        card.into_any_element()
    }

    /// A card for an open review finding: its summary, and Fixed / Out of
    /// scope / Declined buttons — the statuses `USER_FINDING_STATUSES` other
    /// than `open` offers from the conversation view's own findings pane.
    fn render_finding_card(
        &self,
        item: &AttentionItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let radius = cx.theme().radius;
        let finding = self.loaded.findings.iter().find(|f| f.id == item.id).cloned();
        let mut card = div()
            .id(SharedString::from(format!("unified-decisions-finding-{}", item.id)))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_1()
            .border_color(border)
            .rounded(radius)
            .child(kind_badge("Finding"))
            .child(selectable_markdown(
                SharedString::from(format!("unified-decisions-finding-summary-{}", item.id)),
                item.summary.clone(),
                window,
                cx,
            ));
        let Some(finding) = finding else {
            return card.into_any_element();
        };
        let buttons = div().flex().flex_wrap().gap_1().children(
            [
                (FINDING_FIXED, "Fixed"),
                (FINDING_OUT_OF_SCOPE, "Out of scope"),
                (FINDING_DECLINED, "Declined"),
            ]
            .into_iter()
            .map(|(status, label)| {
                let finding = finding.clone();
                Button::new(SharedString::from(format!(
                    "unified-decisions-finding-{status}-{}",
                    finding.id
                )))
                .label(label)
                .small()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.answer_finding(&finding, status, Source::Click, cx);
                }))
            }),
        );
        card = card.child(buttons);
        card.into_any_element()
    }

    /// A card for the node's gate check: every failing criterion the shared
    /// [`LifecycleController`] holds, each with its own Waive button
    /// (`LifecycleController::waive`) — the same criteria table the
    /// lifecycle panel shows.
    fn render_gate_card(
        &self,
        item: &AttentionItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = cx.theme().border;
        let radius = cx.theme().radius;
        let node_id = item.node_id;
        let failing: Vec<crate::views::lifecycle_control::CriterionOutcome> = self
            .lifecycle
            .read(cx)
            .state(&node_id.to_string())
            .map(|s| s.criteria_detail.iter().filter(|r| r.is_failing()).cloned().collect())
            .unwrap_or_default();
        let mut card = div()
            .id(SharedString::from(format!("unified-decisions-gate-{}", item.id)))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_1()
            .border_color(border)
            .rounded(radius)
            .child(kind_badge("Gate check"))
            .child(selectable_markdown(
                SharedString::from(format!("unified-decisions-gate-summary-{}", item.id)),
                item.summary.clone(),
                window,
                cx,
            ));
        if failing.is_empty() {
            return card.into_any_element();
        }
        card = card.children(failing.into_iter().map(|criterion| {
            let label = criterion.label.clone();
            div()
                .id(SharedString::from(format!(
                    "unified-decisions-gate-row-{}",
                    criterion.criterion_id
                )))
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
        }));
        card.into_any_element()
    }

    /// One attention item, dispatched by kind.
    fn render_attention_item(
        &self,
        item: &AttentionItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match item.kind {
            AttentionKind::Decision => {
                let Some(decision) = self.loaded.pending.iter().find(|d| d.id == item.id).cloned() else {
                    return div().into_any_element();
                };
                self.render_pending_decision(&decision, false, window, cx)
                    .into_any_element()
            }
            AttentionKind::PlanStep => self.render_plan_step_card(item, window, cx).into_any_element(),
            AttentionKind::Finding => self.render_finding_card(item, window, cx).into_any_element(),
            AttentionKind::Gate => self.render_gate_card(item, window, cx).into_any_element(),
        }
    }
}

/// A small muted label naming which of the four attention kinds a card is.
fn kind_badge(label: &'static str) -> impl IntoElement {
    div().text_xs().child(label)
}

/// "option 2 (\"per invoice\")" or "\"free text\"" for one log entry.
fn describe_answer(decision: &Decision, answer: &DecisionAnswer) -> String {
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

impl ColumnPanel for DecisionsPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "Decisions".into()
    }

    fn target_label(&self, cx: &App) -> SharedString {
        match self.node_id {
            Some(id) => self
                .fleet
                .get_task(&id.to_string())
                .ok()
                .flatten()
                .map(|t| t.title)
                .unwrap_or_else(|| id.to_string())
                .into(),
            None => {
                let _ = cx;
                "no node selected".into()
            }
        }
    }
}

impl EventEmitter<PanelOpenRequest> for DecisionsPanel {}

impl Focusable for DecisionsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DecisionsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_refresh {
            self.pending_refresh = false;
            self.reload(cx);
        }
        let muted = cx.theme().muted_foreground;

        let body = if self.node_id.is_none() {
            div()
                .p_3()
                .text_sm()
                .text_color(muted)
                .child("Select a node to see its decisions.")
                .into_any_element()
        } else {
            div()
                .id("unified-decisions-body")
                .flex()
                .flex_col()
                .gap_3()
                .p_3()
                .size_full()
                .overflow_y_scroll()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(self.loaded.items.iter().map(|item| {
                            self.render_attention_item(item, window, cx).into_any_element()
                        }))
                        .when(self.loaded.items.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("Nothing pending on this node."),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().text_xs().text_color(muted).child("Answer log"))
                        .children(self.loaded.log.iter().rev().map(|entry| {
                            self.render_log_entry(entry, window, cx).into_any_element()
                        }))
                        .when(self.loaded.log.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("No answers yet."),
                            )
                        }),
                )
                .into_any_element()
        };

        div()
            .id("unified-decisions-panel")
            .key_context(UNIFIED_DECISIONS_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::answer_option_key))
            .on_action(cx.listener(Self::on_activate))
            .on_action(cx.listener(Self::on_ctrl_activate))
            .on_action(cx.listener(Self::exit_freeform_edit))
            .on_action(cx.listener(Self::link_prev))
            .on_action(cx.listener(Self::link_next))
            .size_full()
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::agent::SharedAgent;
    use crate::views::rows::fixture::Fixture;
    use gpui::{TestAppContext, VisualTestContext};
    use gpui_component::Root;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Mutex;
    use tod_agent::MockAgentProvider;
    use tod_store::interview::{ACTOR_USER, InterviewCommand};

    fn mock_agent() -> SharedAgent {
        Arc::new(Mutex::new(Box::new(MockAgentProvider::new())))
    }

    fn ask(fixture: &Fixture, question: &str, options: &[&str]) -> Uuid {
        let id = fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AskDecision {
                    node_id: fixture.node_id,
                    conversation_id: None,
                    protocol: None,
                    decision: tod_store::decisions::NewDecision {
                        question: question.to_string(),
                        options: options.iter().map(|o| o.to_string()).collect(),
                        evidence: Vec::new(),
                    },
                },
            )
            .unwrap()
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap();
        id
    }

    fn open_panel<'a>(
        node_id: Option<Uuid>,
        fixture: &Fixture,
        cx: &'a mut TestAppContext,
    ) -> (Entity<DecisionsPanel>, Entity<AgentRuns>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let fleet = fixture.store.clone();
        let agent_runs = cx.new(|_| AgentRuns::new(fleet.clone(), mock_agent()));
        let agent_runs_for_view = agent_runs.clone();
        let lifecycle = cx.new(|_| LifecycleController::new(fleet.clone()));
        let slot = Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                DecisionsPanel::new(node_id, fleet, agent_runs_for_view, lifecycle, window, cx)
            });
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        draw(cx);
        (view, agent_runs, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn loads_pending_decisions_oldest_first(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let first = ask(&fixture, "First?", &["a", "b"]);
        let second = ask(&fixture, "Second?", &["a", "b"]);
        let (view, _agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);

        view.read_with(cx, |view, _| {
            let ids: Vec<_> = view.loaded.pending.iter().map(|d| d.id).collect();
            assert_eq!(ids, [first, second]);
        });
    }

    #[gpui::test]
    fn digit_key_answers_the_top_pending_decision(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let decision_id = ask(&fixture, "Round per line or per invoice?", &["per line", "per invoice"]);
        let (view, _agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);

        view.update_in(cx, |view, window, cx| {
            view.answer_option_key(&DecisionOptionKey(2), window, cx);
        });
        cx.run_until_parked();
        draw(cx);

        let with_answers = fixture
            .store
            .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))
            .unwrap()
            .unwrap();
        assert_eq!(with_answers.answers.len(), 1);
        assert_eq!(with_answers.answers[0].option, Some(2));
        view.read_with(cx, |view, _| {
            assert!(view.loaded.pending.is_empty(), "answered decision drops off the pending list");
            assert_eq!(view.loaded.log.len(), 1);
        });
    }

    #[gpui::test]
    fn changing_an_answer_appends_a_new_log_entry_without_touching_the_first(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let decision_id = ask(&fixture, "Which?", &["a", "b"]);
        let (view, agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);

        agent_runs
            .update(cx, |runs, cx| runs.answer_decision(decision_id, Some(1), None, cx))
            .unwrap();
        cx.run_until_parked();
        view.update(cx, |view, cx| view.reload(cx));
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.log.len(), 1);
        });

        view.update(cx, |view, cx| {
            view.start_change(decision_id, cx);
            view.click_option(
                view.loaded.log[0].decision.clone(),
                2,
                cx,
            );
        });
        cx.run_until_parked();
        draw(cx);

        let with_answers = fixture
            .store
            .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))
            .unwrap()
            .unwrap();
        assert_eq!(with_answers.answers.len(), 2, "the first answer is never overwritten");
        assert_eq!(with_answers.answers[0].option, Some(1));
        assert_eq!(with_answers.answers[1].option, Some(2));
        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.log.len(), 2);
            assert!(view.changing.is_none(), "answering clears the change-in-progress state");
        });
    }

    #[gpui::test]
    fn set_node_reloads_for_the_new_target(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let _first = ask(&fixture, "On node one?", &["a"]);
        let other_node = Uuid::new_v4();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AskDecision {
                    node_id: other_node,
                    conversation_id: None,
                    protocol: None,
                    decision: tod_store::decisions::NewDecision {
                        question: "won't be created: node missing".to_string(),
                        options: vec!["a".to_string()],
                        evidence: Vec::new(),
                    },
                },
            )
            .ok();
        let (view, _agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);
        view.read_with(cx, |view, _| assert_eq!(view.loaded.pending.len(), 1));

        view.update_in(cx, |view, window, cx| {
            view.set_node(None, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            assert!(view.node_id.is_none());
            assert!(view.loaded.pending.is_empty());
        });
    }

    /// W16: the panel shows every kind `tod_core::attention` knows about,
    /// not only `decisions` rows — a node whose only trouble is a plan step
    /// the agent handed back still shows up here, and answering it goes
    /// through `AgentRuns::answer_plan_step_handoff`, the same message
    /// `conversation::side_pane::answer_handoff` sends.
    #[gpui::test]
    fn a_blocked_plan_step_shows_and_can_be_answered(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        tod_store::paths::set_data_root(fixture.store.paths().root().to_path_buf());
        let step_id = fixture.steps[0];
        fixture
            .store
            .enqueue_outline(tod_store::outline::OutlineMutation::UpdatePlanStepStatus {
                step_id,
                status: tod_store::outline::repos::plan_steps::STATUS_BLOCKED.to_string(),
                note: Some("Needs a call on rounding.".to_string()),
                reason: Some(HandoffReason::Decision {
                    options: vec!["per line".to_string(), "per invoice".to_string()],
                }),
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();

        // The implementation conversation the step's handoff came from —
        // `AgentRuns::answer_plan_step_handoff` delivers the answer there.
        let conversation_id = Uuid::new_v4();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::CreateConversation {
                    id: conversation_id,
                    focus: tod_store::conversation::Focus::Node(fixture.node_id),
                    protocol: tod_store::conversation::ProtocolKind::Implementation,
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();

        let (view, _agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.items.len(), 1);
            assert_eq!(view.loaded.items[0].kind, AttentionKind::PlanStep);
            assert_eq!(view.loaded.items[0].id, step_id);
            assert_eq!(view.loaded.handoff_steps.len(), 1);
        });

        view.update(cx, |view, cx| {
            let step = view.loaded.handoff_steps[0].clone();
            view.answer_plan_step(&step, HandoffAnswer::Choose(0), Source::Click, cx);
        });
        cx.run_until_parked();
        draw(cx);

        let status = fixture
            .store
            .read(|conn| PlanStepRepo::new(conn).get(step_id))
            .unwrap()
            .unwrap()
            .status;
        assert_eq!(status, tod_store::outline::repos::plan_steps::STATUS_IN_PROGRESS);
        view.read_with(cx, |view, _| {
            assert!(view.loaded.items.is_empty(), "answered step drops off the pending list");
        });
    }

    /// A gate check that needs a human (`tod_core::attention::AttentionKind::Gate`)
    /// shows its failing criterion with a Waive button, sourced from the one
    /// [`LifecycleController`] the shell shares with the conversation view
    /// and the lifecycle panel.
    #[gpui::test]
    fn a_gate_item_with_a_failing_criterion_shows_a_waive_button(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        fixture
            .store
            .enqueue_outline(tod_store::outline::OutlineMutation::SetLifecycle {
                node_id: fixture.node_id,
                state: "design".to_string(),
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();

        // The seeded "design" -> "planning" criterion, failed so it shows as
        // a row to waive (`LifecycleController::load_persisted`).
        let criterion_id = fixture
            .store
            .read(|conn| {
                tod_store::outline::repos::GateRepo::new(conn)
                    .get_by_slug(tod_store::outline::repos::gate::BUILDABLE_CRITERION_SLUG)
            })
            .unwrap()
            .unwrap()
            .id;
        fixture
            .store
            .enqueue_outline(tod_store::outline::OutlineMutation::ApplyGateResults {
                node_id: fixture.node_id,
                results: vec![(
                    criterion_id,
                    tod_store::outline::repos::gate::OUTCOME_FAIL.to_string(),
                    Some("Not yet buildable.".to_string()),
                    tod_store::outline::repos::gate::ACTION_NONE.to_string(),
                )],
                forward_state: None,
                source: "agent".to_string(),
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();

        // A gate-check conversation whose report needs a human, so the item
        // shows up in the unified attention list too (`tod_core::attention`).
        let conversation_id = Uuid::new_v4();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::CreateConversation {
                    id: conversation_id,
                    focus: tod_store::conversation::Focus::Node(fixture.node_id),
                    protocol: tod_store::conversation::ProtocolKind::GateCheck,
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::SetConversationTransition {
                    conversation_id,
                    from_state: "design".to_string(),
                    to_state: "planning".to_string(),
                },
            )
            .unwrap();
        let report = serde_json::json!({
            "gate_check": {
                "result": "needs_human",
                "summary": "Buildable check needs your call.",
                "next": "",
                "blockers": [{
                    "kind": "criterion",
                    "reference": "c1",
                    "what": "Is it buildable?",
                    "action": "ask_user",
                }],
                "findings": "",
                "no_reasons": false,
                "advanced_to": null,
            }
        });
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::RecordConversationReport {
                    conversation_id,
                    body: report,
                },
            )
            .unwrap();

        let (view, _agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.items.len(), 1);
            assert_eq!(view.loaded.items[0].kind, AttentionKind::Gate);
        });

        view.update(cx, |view, cx| {
            let criterion = view
                .lifecycle
                .read(cx)
                .state(&fixture.node_id.to_string())
                .unwrap()
                .criteria_detail
                .iter()
                .find(|c| c.criterion_id == criterion_id)
                .cloned()
                .unwrap();
            assert!(criterion.is_failing());
            view.waive_criterion(fixture.node_id, &criterion, Source::Click, cx);
        });
        cx.run_until_parked();
        draw(cx);

        view.read_with(cx, |view, cx| {
            let outcome = view
                .lifecycle
                .read(cx)
                .state(&fixture.node_id.to_string())
                .unwrap()
                .criteria_detail
                .iter()
                .find(|c| c.criterion_id == criterion_id)
                .cloned()
                .unwrap();
            assert!(!outcome.is_failing(), "waiving clears the failing outcome");
        });
    }

    /// Mixed attention kinds on one node order oldest first, matching
    /// `tod_core::attention::for_node`.
    #[gpui::test]
    fn mixed_kinds_are_ordered_oldest_first(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let step_id = fixture.steps[0];
        fixture
            .store
            .enqueue_outline(tod_store::outline::OutlineMutation::UpdatePlanStepStatus {
                step_id,
                status: tod_store::outline::repos::plan_steps::STATUS_BLOCKED.to_string(),
                note: Some("Stuck.".to_string()),
                reason: None,
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();

        let _decision = ask(&fixture, "A or B?", &["A", "B"]);

        let (view, _agent_runs, cx) = open_panel(Some(fixture.node_id), &fixture, cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.items.len(), 2);
            assert!(view.loaded.items[0].since <= view.loaded.items[1].since);
            assert_eq!(view.loaded.items[0].kind, AttentionKind::PlanStep);
            assert_eq!(view.loaded.items[1].kind, AttentionKind::Decision);
        });
    }
}
