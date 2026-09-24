//! The decisions panel (`doc/ui/unified-view.md` "Decisions"): the
//! singleton panel where the user answers whatever a node's agents are
//! waiting on. It always shows the currently selected node's decisions —
//! `UnifiedView` retargets it (`DecisionsPanel::set_node`) whenever the tree
//! selection changes, so this column does not need its own target in
//! `PanelKind`.
//!
//! Pending decisions are listed oldest first; number keys 1-9 answer the
//! *top* pending decision with that option (`doc/ui/unified-view.md`
//! "Keys"). Below them sits the append-only answer log: one entry per
//! answer ever given (never per decision — a change of mind adds a new
//! entry, the old one stays), each with **Change** (re-ask the same
//! decision; the new answer is a new `decision_answers` row, nothing is
//! reversed) and a link to the conversation that asked.
//!
//! Every answer goes through `AgentRuns::answer_decision`, which records it
//! and delivers it to the asking conversation, and records a journey
//! `UserAction` with a `Presented` snapshot of the options that were shown
//! (`crate::conversation::lifecycle` is the pattern this follows).

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    Styled, Subscription, Window, actions, div, prelude::FluentBuilder,
};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable};
use tod_journey::{Presented, PresentedAction};
use tod_store::decisions::{DECISION_PENDING, Decision, DecisionAnswer, DecisionRepo, EvidenceRef};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::ui::agent_runs::AgentRuns;
use crate::ui::journey::{Source, record_action};
use crate::ui::key_context;
use crate::ui::selectable_text::{selectable_markdown, selectable_text};
use crate::unified::columns::PanelKind;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};

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
        DecisionsFreeformEnterEdit,
        DecisionsFreeformEscape,
        DecisionsLinkPrev,
        DecisionsLinkNext,
    ]
);

/// Registers the decisions panel's own keys: digits 1-9 (answer the top
/// pending decision), and the freeform field's edit-mode Enter/Escape. Call
/// once alongside `unified::register_unified_keyboard_bindings`. Evidence
/// links reuse the panel-wide `PanelActivateFocusedLink` /
/// `PanelCtrlActivateFocusedLink` bindings already registered there.
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
        KeyBinding::new("enter", DecisionsFreeformEnterEdit, outside_input),
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
            anyhow::Ok(Loaded { pending, log })
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
    _poll: gpui::Task<()>,
}

impl DecisionsPanel {
    pub fn new(
        node_id: Option<Uuid>,
        fleet: Arc<FleetStore>,
        agent_runs: Entity<AgentRuns>,
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

        Self {
            node_id,
            fleet,
            agent_runs,
            focus_handle: cx.focus_handle(),
            loaded,
            freeform_editing: None,
            freeform_input,
            changing: None,
            selected_link: 0,
            pending_refresh: false,
            _freeform_subscription,
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

    /// Answer the top pending decision with option `n` (1-based) —
    /// `doc/ui/unified-view.md` "Keys": "1, 2, 3 … Answer the top pending
    /// decision with that option."
    fn answer_option_key(&mut self, action: &DecisionOptionKey, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(decision) = self.loaded.pending.first().cloned() else {
            return;
        };
        if action.0 == 0 || action.0 > decision.options.len() {
            return;
        }
        self.answer(decision, Some(action.0), None, Source::Keyboard, cx);
    }

    fn click_option(&mut self, decision: Decision, option: usize, cx: &mut Context<Self>) {
        self.answer(decision, Some(option), None, Source::Click, cx);
    }

    fn enter_freeform_edit(
        &mut self,
        _: &DecisionsFreeformEnterEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(decision) = self.top_freeform_target() else {
            return;
        };
        self.freeform_editing = Some(decision);
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

    /// The decision the freeform field would edit if Enter were pressed
    /// right now: whichever decision is being changed, else the top pending
    /// one.
    fn top_freeform_target(&self) -> Option<Uuid> {
        self.changing.or_else(|| self.loaded.pending.first().map(|d| d.id))
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

    fn link_prev(&mut self, _: &DecisionsLinkPrev, _window: &mut Window, cx: &mut Context<Self>) {
        self.selected_link = self.selected_link.saturating_sub(1);
        cx.notify();
    }

    fn link_next(&mut self, _: &DecisionsLinkNext, _window: &mut Window, cx: &mut Context<Self>) {
        self.selected_link = self.selected_link.saturating_add(1);
        cx.notify();
    }

    fn activate_selected_link(&mut self, decision: &Decision, ctrl: bool, cx: &mut Context<Self>) {
        let node_id = decision.node_id;
        let links = evidence_links(node_id, &decision.evidence);
        if let Some(Some(target)) = links.get(self.selected_link).map(|l| l.target) {
            self.open(target, ctrl, cx);
        }
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
                    Button::new(("unified-decisions-option", decision.id, option))
                        .label(format!("{option}. {label}"))
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.click_option(decision.clone(), option, cx);
                        }))
                }),
        )
    }

    fn render_evidence(&self, decision: &Decision, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let links = evidence_links(decision.node_id, &decision.evidence);
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(links.into_iter().enumerate().map(|(ix, link)| {
                let selected = ix == self.selected_link;
                match link.target {
                    Some(target) => div()
                        .id(("unified-decisions-evidence", decision.id, ix))
                        .text_xs()
                        .when(selected, |el| el.text_color(theme.accent))
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
            .id(("unified-decisions-freeform", decision_id))
            .key_context(FREEFORM_TAG)
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.freeform_editing = Some(decision_id);
                    this.freeform_input.update(cx, |input, cx| {
                        input.set_value(String::new(), window, cx);
                    });
                    cx.notify();
                    let input = this.freeform_input.clone();
                    cx.on_next_frame(window, move |_, window, cx| {
                        input.update(cx, |input, cx| input.focus(window, cx));
                    });
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
        let theme = cx.theme();
        div()
            .id(("unified-decisions-pending", decision.id))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .child(selectable_markdown(
                ("unified-decisions-question", decision.id),
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
        let theme = cx.theme();
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
            .border_color(theme.border)
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
                            .text_color(theme.muted_foreground)
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
        let theme = cx.theme();
        let muted = theme.muted_foreground;

        let body = if self.node_id.is_none() {
            div()
                .p_3()
                .text_sm()
                .text_color(muted)
                .child("Select a node to see its decisions.")
                .into_any_element()
        } else {
            div()
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
                        .children(self.loaded.pending.iter().enumerate().map(|(ix, d)| {
                            self.render_pending_decision(d, ix == 0, cx).into_any_element()
                        }))
                        .when(self.loaded.pending.is_empty(), |el| {
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
                        .children(
                            self.loaded
                                .log
                                .iter()
                                .rev()
                                .map(|entry| self.render_log_entry(entry, cx).into_any_element()),
                        )
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
            .on_action(cx.listener(Self::enter_freeform_edit))
            .on_action(cx.listener(Self::exit_freeform_edit))
            .on_action(cx.listener(Self::link_prev))
            .on_action(cx.listener(Self::link_next))
            .size_full()
            .child(body)
    }
}
