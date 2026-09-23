//! Lifecycle panel — shows a node's lifecycle and the controls that move it
//! (see `TaskListEvent::OpenLifecycle` / `handle_lifecycle_control` in
//! `views/task_list/mod.rs`). This is where Proceed/`L` always lands first,
//! for every phase including ones with an interview — the gate check gets a
//! chance to advance the node on its own before anything falls back to a
//! conversational interview.
//!
//! The gate check, waiving, advancing, and on-entry turns are run by the
//! shared [`LifecycleController`] (see its docs), which the conversation
//! view's lifecycle buttons drive too; this panel shows its state for the
//! selected node and offers every control, the manual ones included.
//!
//! Three manual escape hatches sit alongside the gate check, each a direct
//! lifecycle write that bypasses the gate agent entirely: **Open interview**
//! (jump into that phase's conversational interview even if its session
//! previously ran to exhaustion — a blocked gate check may need input the
//! state agent can't get on its own), **Force advance** (skip the criteria
//! when the user judges a failure isn't worth blocking on), and **Revert**
//! (step back one lifecycle state, e.g. to make a `planning` node re-run its
//! plan-step generation from `design`). Force advance and Revert both
//! require a confirming second click (`GateCheckState::force_advance_armed` /
//! `revert_armed`).
//!
//! When the node's state no longer holds — its obligations or plan changed
//! since it left `planning`, or verification failed
//! (`tod_core::lifecycle_validity`) — an orange callout at the top says why
//! and offers **Move back** to the latest state that still holds. The app
//! never moves it on its own: the change may yet be reversed, which clears
//! the callout.

use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::agent_chat::OpenConversation;
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, bind_modified_pane_nav};
use crate::ui::selectable_text::{selectable_markdown, selectable_text};
use crate::ui::style;
use crate::views::incoming_check::{IncomingCheck, outcome_line};
use crate::views::lifecycle_control::{
    GateCheckState, LifecycleController, enters_with_agent, implement_directory,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, Stateful, StatefulInteractiveElement, Styled,
    Subscription, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Disableable, StyledExt, h_flex, v_flex};
use std::collections::HashMap;
use std::sync::Arc;
use tod_core::conversation::implement::{PlanProgress, plan_progress};
use tod_core::gate::GateAction;
use tod_core::incoming::NodeOutcome;
use tod_core::lifecycle_next::{NextStep, Standing, next_step};
use tod_core::lifecycle_validity::{Regression, regression};
use tod_core::process::interview_phase_for_lifecycle;
use tod_core::task::model::{next_lifecycle, previous_lifecycle};
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind};
use tod_store::fleet::FleetStore;
use tod_store::outline::repos::plan_steps::{STATUS_FAILED, STATUS_VERIFIED};
use tod_store::outline::{OUTCOME_PASS, OUTCOME_WAIVED};
use tod_store::review::ReviewRepo;

const LIFECYCLE_PANEL_CONTEXT: &str = "LifecyclePanel";

actions!(
    lifecycle_panel,
    [
        LifecyclePanelClose,
        LifecyclePanelFocusUp,
        LifecyclePanelFocusDown,
        LifecyclePanelActivate,
    ]
);

#[derive(Debug, Clone)]
pub enum LifecyclePanelEvent {
    Close,
    /// Escape / Ctrl+Left — move keyboard focus back to the task tree, leaving
    /// the panel open. Mirrors the drawer-panel convention documented in
    /// CLAUDE.md.
    FocusTaskList,
    /// User asked to open the interview for `task_id` at `lifecycle` — an
    /// on-demand fallback, not something the gate check does automatically.
    /// See `TaskListView::open_interview_for_task`.
    OpenInterview {
        task_id: String,
        lifecycle: String,
    },
}

/// Keyboard-navigable stops within the panel, in visual order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecyclePanelStop {
    MoveBack,
    CheckIncoming,
    Implement,
    Verify,
    Review,
    RunGateCheck,
    OpenInterview,
    ForceAdvance,
    RevertLifecycle,
    Close,
}

/// In `active`, implementing and the gate check are one control, chosen by
/// where the node's plan stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveControl {
    /// Plan steps remain, or an implementation run is still going: Implement.
    Implement { remaining: usize, total: usize },
    /// No plan at all — nothing to implement and nothing to check yet.
    NoPlan,
    /// Every plan step is done: the gate check takes over.
    Complete { total: usize },
}

/// One net pending incoming change, as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
struct IncomingRow {
    /// E.g. "Constraint added on Parent (via ancestor)" or "Requirement
    /// changed on Card field (via reference)".
    headline: String,
    before: Option<String>,
    after: Option<String>,
}

impl IncomingRow {
    fn new(fleet: &FleetStore, change: tod_store::incoming::PendingChange) -> Self {
        use tod_store::conversation::{EntitySnapshot, NetOp};
        let source = fleet
            .get_node(&change.source_node.to_string())
            .ok()
            .flatten()
            .map(|n| n.title)
            .unwrap_or_else(|| change.source_node.to_string());
        let text = |snap: &Option<EntitySnapshot>| match snap {
            Some(EntitySnapshot::Obligation { kind, body, .. }) => Some(format!("[{kind}] {body}")),
            Some(other) => Some(format!("{other:?}")),
            None => None,
        };
        let op = match change.op {
            NetOp::Added => "added",
            NetOp::Deleted => "deleted",
            NetOp::Moved => "moved",
            NetOp::Reversed => "reversed",
            NetOp::Edited => "changed",
        };
        // An ancestor's change is always a constraint; a component's may be
        // any obligation.
        let kind = [&change.after, &change.before]
            .into_iter()
            .find_map(|s| match s {
                Some(EntitySnapshot::Obligation { kind, .. }) => Some(kind.as_str()),
                _ => None,
            });
        let what = match kind {
            Some(tod_store::outline::KIND_CONSTRAINT) => "Constraint",
            Some(tod_store::outline::KIND_REQUIREMENT) => "Requirement",
            Some(_) => "Obligation",
            None => "Item",
        };
        Self {
            headline: format!("{what} {op} on {source} (via {})", change.via.as_str()),
            before: text(&change.before),
            after: text(&change.after),
        }
    }
}

pub struct LifecyclePanelView {
    fleet: Arc<FleetStore>,
    controller: Entity<LifecycleController>,
    incoming_check: Entity<IncomingCheck>,
    /// Mirrors `incoming_check`'s running flag, for the keyboard stops.
    check_running: bool,
    task_id: Option<String>,
    title: String,
    lifecycle: String,
    /// Whether the currently displayed node has the Lifecycle capability.
    /// When false, the panel shows a generic message instead of gate-check UI.
    lifecycle_capable: bool,
    /// Status line for the Active-phase implementation launcher, keyed by
    /// task id.
    implement_status: HashMap<String, String>,
    /// `None` outside `active`. Refreshed on open and each render, so the
    /// keyboard stops and the rendered control agree.
    active_control: Option<ActiveControl>,
    /// The node's state no longer holds and should go back; re-read on open
    /// and whenever the store changes.
    regression: Option<Regression>,
    /// Where the node's work stands in the store, which decides which step
    /// the panel highlights next; re-read on open and whenever the store
    /// changes.
    standing: Option<Standing>,
    /// The node's pending incoming changes, netted per item
    /// (`doc/conversation/incoming-changes.md` §6); re-read on open and
    /// whenever the store changes.
    incoming: Vec<IncomingRow>,
    /// The node's stored `learn` retrospectives, one per completed pass, as
    /// `(pass, content)` (`doc/conversation/incoming-changes.md` §9).
    learnings: Vec<(i64, String)>,
    focus_handle: FocusHandle,
    focus_index: usize,
    _controller_subscription: Subscription,
    _incoming_check_subscription: Subscription,
}

impl LifecyclePanelView {
    pub fn new(
        cx: &mut Context<Self>,
        fleet: Arc<FleetStore>,
        controller: Entity<LifecycleController>,
        incoming_check: Entity<IncomingCheck>,
    ) -> Self {
        let incoming_check_subscription = cx.observe(&incoming_check, |this, check, cx| {
            this.check_running = check.read(cx).is_running();
            this.clamp_focus_index();
            cx.notify();
        });
        // The controller moves the lifecycle (a gate check that passed, an
        // Advance from the conversation view): re-read the node when it does.
        let subscription = cx.observe(&controller, |this, _, cx| {
            if let Some(task_id) = this.task_id.clone() {
                this.load_task(&task_id);
                this.clamp_focus_index();
            }
            cx.notify();
        });
        // Obligations, plan steps, and verdicts change from anywhere — a
        // conversation's agent, another panel: re-judge the state when they do.
        let poll_entity = cx.weak_entity();
        let fleet_for_poll = fleet.clone();
        cx.spawn(async move |_, cx| {
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
                    let Ok(()) = poll_entity.update(cx, |this: &mut Self, cx| {
                        let incoming_changed = this.refresh_incoming();
                        let learnings_changed = this.refresh_learnings();
                        let standing_changed = this.refresh_standing();
                        if this.refresh_regression()
                            | incoming_changed
                            | learnings_changed
                            | standing_changed
                        {
                            this.clamp_focus_index();
                            cx.notify();
                        }
                    }) else {
                        break;
                    };
                }
            }
        })
        .detach();
        Self {
            fleet,
            controller,
            incoming_check,
            check_running: false,
            task_id: None,
            title: String::new(),
            lifecycle: String::new(),
            lifecycle_capable: false,
            implement_status: HashMap::new(),
            active_control: None,
            regression: None,
            standing: None,
            incoming: Vec::new(),
            learnings: Vec::new(),
            focus_handle: cx.focus_handle(),
            focus_index: 0,
            _controller_subscription: subscription,
            _incoming_check_subscription: incoming_check_subscription,
        }
    }

    fn stops(&self) -> Vec<LifecyclePanelStop> {
        let mut stops = Vec::new();
        if self.regression.is_some() {
            stops.push(LifecyclePanelStop::MoveBack);
        }
        if self.check_offered() {
            stops.push(LifecyclePanelStop::CheckIncoming);
        }
        match self.active_control {
            Some(ActiveControl::Implement { .. }) => stops.push(LifecyclePanelStop::Implement),
            Some(ActiveControl::NoPlan) => {}
            Some(ActiveControl::Complete { .. }) | None => {
                if self.verification_offered() {
                    stops.push(LifecyclePanelStop::Verify);
                }
                if self.review_offered() {
                    stops.push(LifecyclePanelStop::Review);
                }
                if self.lifecycle_capable && next_lifecycle(&self.lifecycle).is_some() {
                    stops.push(LifecyclePanelStop::RunGateCheck);
                }
            }
        }
        if self.interview_available() {
            stops.push(LifecyclePanelStop::OpenInterview);
        }
        if self.lifecycle_capable && next_lifecycle(&self.lifecycle).is_some() {
            stops.push(LifecyclePanelStop::ForceAdvance);
        }
        if self.lifecycle_capable && previous_lifecycle(&self.lifecycle).is_some() {
            stops.push(LifecyclePanelStop::RevertLifecycle);
        }
        stops.push(LifecyclePanelStop::Close);
        stops
    }

    /// Whether this node's current lifecycle has an interview phase at all.
    /// The gate check runs first by default (see `handle_lifecycle_control`)
    /// and this is always offered alongside it as a manual fallback —
    /// deliberately not gated on whether that phase's interview session
    /// still looks "incomplete": a gate check can come back blocked for
    /// reasons the state agent could only resolve by asking the user.
    fn interview_available(&self) -> bool {
        self.lifecycle_capable && interview_phase_for_lifecycle(&self.lifecycle).is_some()
    }

    fn clamp_focus_index(&mut self) {
        let len = self.stops().len();
        if len == 0 {
            self.focus_index = 0;
        } else if self.focus_index >= len {
            self.focus_index = len - 1;
        }
    }

    fn focused_stop(&self) -> Option<LifecyclePanelStop> {
        self.stops().get(self.focus_index).copied()
    }

    fn is_focused(&self, stop: LifecyclePanelStop) -> bool {
        self.focused_stop() == Some(stop)
    }

    fn move_focus(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let stops = self.stops();
        if stops.is_empty() {
            return;
        }
        let len = stops.len() as i32;
        self.focus_index = ((self.focus_index as i32 + delta).rem_euclid(len)) as usize;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn activate_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.focused_stop() {
            Some(LifecyclePanelStop::MoveBack) => self.move_back(cx),
            Some(LifecyclePanelStop::CheckIncoming) => self.check_incoming(cx),
            Some(LifecyclePanelStop::Implement) => self.launch_implementation(window, cx),
            Some(LifecyclePanelStop::Verify) => self.launch_verification(window, cx),
            Some(LifecyclePanelStop::Review) => self.launch_review(window, cx),
            Some(LifecyclePanelStop::RunGateCheck) => self.run_gate_check(window, cx),
            Some(LifecyclePanelStop::OpenInterview) => {
                if let Some(task_id) = self.task_id.clone() {
                    cx.emit(LifecyclePanelEvent::OpenInterview {
                        task_id,
                        lifecycle: self.lifecycle.clone(),
                    });
                }
            }
            Some(LifecyclePanelStop::ForceAdvance) => self.force_advance(window, cx),
            Some(LifecyclePanelStop::RevertLifecycle) => self.revert_lifecycle(cx),
            Some(LifecyclePanelStop::Close) => self.close(cx),
            None => {}
        }
    }

    /// Run `f` on the controller for the panel's node.
    fn with_controller<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut LifecycleController, &str, &mut Context<LifecycleController>) -> R,
    ) -> Option<R> {
        let task_id = self.task_id.clone()?;
        Some(
            self.controller
                .update(cx, |controller, cx| f(controller, &task_id, cx)),
        )
    }

    /// Check the gate to the next state in a conversation, which shows what
    /// the state's agent concluded and what to do about it.
    fn run_gate_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.launch_node_conversation(ProtocolKind::GateCheck, window, cx);
    }

    fn force_advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entered = self.with_controller(cx, |c, id, cx| c.force_advance(id, cx));
        self.enter_state(entered.flatten(), window, cx);
    }

    /// A node landed in `state`: when that state has on-entry work for its
    /// agent, start it in a conversation.
    fn enter_state(
        &mut self,
        state: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if state.is_some_and(enters_with_agent) {
            self.launch_node_conversation(ProtocolKind::OnEntry, window, cx);
        }
    }

    /// Re-judge whether the node's state still holds. `true` when the answer
    /// changed.
    fn refresh_regression(&mut self) -> bool {
        let found = self
            .task_id
            .as_deref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .filter(|_| self.lifecycle_capable)
            .and_then(|node| {
                self.fleet
                    .read(|conn| regression(conn, node))
                    .ok()
                    .flatten()
            });
        let changed = found != self.regression;
        self.regression = found;
        changed
    }

    /// Re-read where the node's work stands. `true` when it changed.
    fn refresh_standing(&mut self) -> bool {
        let found = self
            .task_id
            .as_deref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .filter(|_| self.lifecycle_capable)
            .and_then(|node| {
                self.fleet
                    .read(|conn| Standing::load(conn, node, &self.lifecycle))
                    .ok()
            });
        let changed = found != self.standing;
        self.standing = found;
        changed
    }

    /// The step the stored state recommends next, which the panel shows as
    /// its primary button.
    fn recommended(&self) -> Option<NextStep> {
        self.standing.as_ref().and_then(next_step)
    }

    /// Re-read the node's pending incoming changes. `true` when they changed.
    fn refresh_incoming(&mut self) -> bool {
        let rows = self
            .task_id
            .as_deref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .and_then(|node| self.fleet.incoming_net_pending(node).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|change| IncomingRow::new(&self.fleet, change))
            .collect::<Vec<_>>();
        let changed = rows != self.incoming;
        self.incoming = rows;
        changed
    }

    /// Re-read the node's stored retrospectives. `true` when they changed.
    fn refresh_learnings(&mut self) -> bool {
        let rows = self
            .node_uuid()
            .and_then(|node| {
                self.fleet
                    .read(|conn| tod_store::learn::LearnRepo::new(conn).outputs(node))
                    .ok()
            })
            .unwrap_or_default()
            .into_iter()
            .map(|output| (output.pass, output.content))
            .collect::<Vec<_>>();
        let changed = rows != self.learnings;
        self.learnings = rows;
        changed
    }

    fn node_uuid(&self) -> Option<uuid::Uuid> {
        self.task_id
            .as_deref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
    }

    /// Check now is offered while the node has pending changes and no check
    /// is running (a check of other nodes blocks it too: one at a time).
    fn check_offered(&self) -> bool {
        !self.incoming.is_empty() && !self.check_running
    }

    /// Evaluate the node against its pending incoming changes.
    fn check_incoming(&mut self, cx: &mut Context<Self>) {
        let Some(node) = self.node_uuid() else {
            return;
        };
        self.incoming_check
            .update(cx, |check, cx| check.start(vec![node], cx));
    }

    /// Send the node back to the latest state that still holds.
    fn move_back(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.regression.as_ref().map(|r| r.target) else {
            return;
        };
        self.with_controller(cx, |c, id, cx| c.revert_to(id, target, cx));
    }

    fn revert_lifecycle(&mut self, cx: &mut Context<Self>) {
        self.with_controller(cx, |c, id, cx| c.revert(id, cx));
    }

    fn waive_criterion(&mut self, criterion_id: uuid::Uuid, cx: &mut Context<Self>) {
        self.with_controller(cx, |c, id, cx| c.waive(id, criterion_id, cx));
    }

    fn advance_after_criteria(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entered = self.with_controller(cx, |c, id, cx| c.advance_after_criteria(id, cx));
        self.enter_state(entered.flatten(), window, cx);
    }

    /// Where implementation would run: the node needs a resolved Agent and a
    /// ready Files directory — what the `ready` → `active` gate requires
    /// (`tod_core::gate::derived`). `Err` carries the user-facing reason.
    fn implement_directory(&self) -> Result<tod_store::fleet::Workdir, String> {
        let Some(task_id) = self.task_id.as_ref() else {
            return Err(String::new());
        };
        implement_directory(&self.fleet, task_id)
    }

    /// The node's live `implementation`-kind run, if any — the one-at-a-time
    /// lock. Other sessions on the node (e.g. a plain chat launched from the
    /// Action panel) are ignored.
    fn implementation_run_live(&self) -> Option<String> {
        let task_id = self.task_id.as_ref()?;
        self.fleet
            .live_implementation_session_for_node(task_id)
            .ok()
            .flatten()
            .map(|run| run.id)
    }

    /// Recompute `active_control` from the plan and any live run. A live run
    /// keeps Implement up even once every step is closed: its loop may still
    /// be getting the tests green.
    fn refresh_active_control(&mut self) {
        self.active_control = (|| {
            if !self.lifecycle_capable || self.lifecycle != "active" {
                return None;
            }
            let node_id = uuid::Uuid::parse_str(self.task_id.as_ref()?).ok()?;
            let running = self.implementation_run_live().is_some();
            Some(match plan_progress(&self.fleet, node_id) {
                PlanProgress::NoPlan => ActiveControl::NoPlan,
                PlanProgress::Remaining { remaining, total } => {
                    ActiveControl::Implement { remaining, total }
                }
                PlanProgress::Complete { total } if running => ActiveControl::Implement {
                    remaining: 0,
                    total,
                },
                PlanProgress::Complete { total } => ActiveControl::Complete { total },
            })
        })();
    }

    /// Open the node's implementation conversation — one per node, reopened
    /// however many times this is pressed. The conversation view runs it
    /// under the implementation protocol. See `doc/conversation/protocols.md`.
    fn launch_implementation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        if let Err(reason) = self.implement_directory() {
            self.implement_status.insert(task_id, reason);
            cx.notify();
            return;
        }
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return;
        };
        // The done-signal is "no plan step still open", so a node with no
        // plan has nothing to drive the loop, and a finished one would only
        // be sent round again. The button is not offered for either; this
        // guards against the plan changing since the last render.
        match plan_progress(&self.fleet, node_id) {
            PlanProgress::Remaining { .. } => {}
            PlanProgress::NoPlan | PlanProgress::Complete { .. }
                if self.implementation_run_live().is_none() =>
            {
                self.refresh_active_control();
                self.clamp_focus_index();
                cx.notify();
                return;
            }
            // A run still going is reopened, not restarted.
            _ => {}
        }
        self.implement_status.remove(&task_id);
        window.dispatch_action(
            Box::new(OpenConversation {
                focus: Focus::Node(node_id),
                protocol: ProtocolKind::Implementation,
                start: true,
            }),
            cx,
        );
        cx.notify();
    }

    /// Whether the Verification section's Verify button is shown: the node
    /// is in `verifying` and has a plan to check.
    fn verification_offered(&self) -> bool {
        self.lifecycle_capable
            && self.lifecycle == "verifying"
            && self
                .task_id
                .as_deref()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .is_some_and(|node| {
                    !self
                        .fleet
                        .list_plan_steps_for_node(node)
                        .unwrap_or_default()
                        .is_empty()
                })
    }

    /// Start a new verification conversation on the node and send it "Verify the requirements.", as Implement does for implementation. See
    /// `doc/conversation/protocols.md`.
    fn launch_verification(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.launch_node_conversation(ProtocolKind::Verification, window, cx);
    }

    /// Whether the Code review section's Review button is shown: the node is
    /// in `review`.
    fn review_offered(&self) -> bool {
        self.lifecycle_capable && self.lifecycle == "review"
    }

    /// Start a new review conversation on the node and send it "Review the
    /// change.". The conversation view runs it under the review
    /// protocol: an agent that did not build the change reviews it in the
    /// node's worktree and records each finding through `tod-cli review`, and
    /// the side pane lists them. See `doc/conversation/protocols.md` §4c.
    fn launch_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.launch_node_conversation(ProtocolKind::Review, window, cx);
    }

    /// Start a new `protocol` conversation on the node, which runs in the
    /// node's worktree, and send the protocol's starter. Every run gets a
    /// fresh agent; one still going is shown instead of started again.
    fn launch_node_conversation(
        &mut self,
        protocol: ProtocolKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        // The state agents (gate check, on entry) work through `tod-cli`
        // wherever the node's files are; the rest need its worktree.
        if !protocol.has_transition() {
            if let Err(reason) = self.implement_directory() {
                self.implement_status.insert(task_id, reason);
                cx.notify();
                return;
            }
        }
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return;
        };
        self.implement_status.remove(&task_id);
        window.dispatch_action(
            Box::new(OpenConversation {
                focus: Focus::Node(node_id),
                protocol,
                start: true,
            }),
            cx,
        );
        cx.notify();
    }

    /// The `verifying` section: how the plan steps stand against verification,
    /// and — when any failed — the way back to implementation. Failed steps
    /// are fixed in `active`, where implementation works each one again from
    /// its note; they cannot be fixed from here.
    /// "Past learnings": the retrospective each completed pass stored, read
    /// only. Earlier passes' work history is summed up here, not re-shown.
    fn render_learnings(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut section = v_flex()
            .gap_2()
            .child(div().text_xs().font_semibold().child("Past learnings"));
        for (pass, content) in self.learnings.iter().rev() {
            let mut item = v_flex()
                .gap_1()
                .pl_2()
                .border_l_2()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("Pass {pass}")),
                );
            item = if content.is_empty() {
                item.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("No retrospective was recorded."),
                )
            } else {
                item.child(div().text_xs().child(selectable_markdown(
                    format!("lifecycle-panel-learning-{pass}"),
                    content.clone(),
                    window,
                    cx,
                )))
            };
            section = section.child(item);
        }
        section.into_any_element()
    }

    /// "N incoming changes": each net pending change the node inherits and
    /// has not been checked against.
    fn render_incoming(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let n = self.incoming.len();
        let mut section = v_flex()
            .gap_2()
            .child(div().text_xs().font_semibold().child(if n == 1 {
                "1 incoming change".to_string()
            } else {
                format!("{n} incoming changes")
            }));
        for (i, row) in self.incoming.iter().enumerate() {
            let mut item = v_flex()
                .gap_1()
                .pl_2()
                .border_l_2()
                .border_color(style::color::incoming_text())
                .child(div().text_xs().child(selectable_text(
                    format!("lifecycle-panel-incoming-{i}-head"),
                    row.headline.clone(),
                    window,
                    cx,
                )));
            if let Some(before) = &row.before {
                item = item.child(div().text_xs().text_color(muted).child(selectable_text(
                    format!("lifecycle-panel-incoming-{i}-before"),
                    format!("Before: {before}"),
                    window,
                    cx,
                )));
            }
            if let Some(after) = &row.after {
                item = item.child(div().text_xs().child(selectable_text(
                    format!("lifecycle-panel-incoming-{i}-after"),
                    format!("After: {after}"),
                    window,
                    cx,
                )));
            }
            section = section.child(item);
        }
        let check = self.incoming_check.read(cx);
        let node = self.node_uuid();
        if node.is_some_and(|n| check.covers(n)) {
            section = section.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Checking this node against these changes…"),
            );
        } else if check.is_running() {
            let (done, total) = check.progress().unwrap_or_default();
            section = section.child(div().text_xs().text_color(muted).child(format!(
                "Another incoming-changes check is running ({done} of {total}); Check now is available when it finishes."
            )));
        } else {
            if let Some(failed) = check
                .results()
                .iter()
                .find(|r| Some(r.node) == node && matches!(r.outcome, NodeOutcome::Failed(_)))
            {
                section = section.child(div().text_xs().text_color(cx.theme().danger).child(
                    selectable_text(
                        "lifecycle-panel-incoming-failed",
                        outcome_line(failed),
                        window,
                        cx,
                    ),
                ));
            }
            let focused = self.is_focused(LifecyclePanelStop::CheckIncoming);
            let list_active_border = cx.theme().list_active_border;
            section = section.child(
                div()
                    .w_full()
                    .rounded_md()
                    .when(focused, |el| el.border_1().border_color(list_active_border))
                    .child(
                        Button::new("lifecycle-panel-check-incoming")
                            .label("Check now")
                            .w_full()
                            .on_click(cx.listener(|this, _, _, cx| this.check_incoming(cx))),
                    ),
            );
        }
        section.into_any_element()
    }

    fn render_verification(
        &self,
        mut body: Stateful<Div>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let (muted, danger, list_active_border) = (
            theme.muted_foreground,
            theme.danger,
            theme.list_active_border,
        );
        let Some(node_id) = self
            .task_id
            .as_deref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
        else {
            return body;
        };
        let steps = self
            .fleet
            .list_plan_steps_for_node(node_id)
            .unwrap_or_default();
        let verified = steps.iter().filter(|s| s.status == STATUS_VERIFIED).count();
        let failed = steps.iter().filter(|s| s.status == STATUS_FAILED).count();
        let standings = tod_core::conversation::verify::standings(&self.fleet, node_id);
        let obligations_verified = standings.iter().filter(|s| s.is_verified()).count();
        let obligations_failed = standings.iter().filter(|s| s.is_failed()).count();
        // "Verify" while verification owes a verdict — never checked, or
        // reopened by a change since — then "Verify again".
        let due = self
            .standing
            .as_ref()
            .is_some_and(Standing::verification_due);
        let status = self
            .task_id
            .as_ref()
            .and_then(|id| self.implement_status.get(id))
            .cloned();
        let revert_armed = self.current_state(cx, |s| s.revert_armed);

        body = body.child(div().text_xs().font_semibold().child("Verification"));
        let mut summary = format!(
            "{obligations_verified} of {} requirements verified",
            standings.len()
        );
        if obligations_failed > 0 {
            summary.push_str(&format!(", {obligations_failed} failed"));
        }
        summary.push_str(&format!(
            "; {verified} of {} plan steps verified",
            steps.len()
        ));
        if failed > 0 {
            summary.push_str(&format!(", {failed} failed"));
        }
        body = body.child(div().text_xs().text_color(muted).child(selectable_text(
            "lifecycle-panel-verification-summary",
            summary,
            window,
            cx,
        )));
        if !steps.is_empty() {
            body = body.child(
                div()
                    .w_full()
                    .rounded_md()
                    .when(self.is_focused(LifecyclePanelStop::Verify), |el| {
                        el.border_1().border_color(list_active_border)
                    })
                    .child(
                        Button::new("lifecycle-panel-verify")
                            .label(if due { "Verify" } else { "Verify again" })
                            .when(due, |b| b.primary())
                            .when(!due, |b| b.ghost())
                            .w_full()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.launch_verification(window, cx);
                            })),
                    ),
            );
        }
        if let Some(status) = status {
            body = body.child(div().text_xs().text_color(muted).child(selectable_text(
                "lifecycle-panel-verify-status",
                status,
                window,
                cx,
            )));
        }
        if failed > 0 {
            let steps_word = if failed == 1 { "step" } else { "steps" };
            body = body
                .child(div().text_xs().text_color(danger).child(format!(
                    "{failed} plan {steps_word} failed verification. Move the node back to \
                     active and implement again: each failed step goes back to the agent \
                     with its note saying what to fix."
                )))
                .child(
                    Button::new("lifecycle-panel-back-to-active")
                        .label(if revert_armed {
                            "Confirm: back to active".to_string()
                        } else {
                            format!("Back to active to fix {failed} failed {steps_word}")
                        })
                        // Once verification has finished; until then, Verify.
                        .when(!due, |b| b.primary())
                        .w_full()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.revert_lifecycle(cx);
                        })),
                );
        }
        body
    }

    /// The `review` section: how the node's review findings stand, and the
    /// Review button that runs a code review in the conversation view, where
    /// the findings are listed and answered.
    fn render_review(
        &self,
        mut body: Stateful<Div>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let (muted, danger, list_active_border) = (
            theme.muted_foreground,
            theme.danger,
            theme.list_active_border,
        );
        let Some(node_id) = self
            .task_id
            .as_deref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
        else {
            return body;
        };
        let (findings, reviewed) = self
            .fleet
            .read(|conn| {
                let findings = ReviewRepo::new(conn).list_for_node(node_id)?;
                let reviewed = ConversationRepo::new(conn)
                    .latest_for_focus_with_protocol(Focus::Node(node_id), ProtocolKind::Review)?
                    .is_some();
                Ok((findings, reviewed))
            })
            .unwrap_or_default();
        let open = findings.iter().filter(|f| f.is_open()).count();
        let status = self
            .task_id
            .as_ref()
            .and_then(|id| self.implement_status.get(id))
            .cloned();

        body = body.child(div().text_xs().font_semibold().child("Code review"));
        let summary = match (reviewed, findings.len()) {
            (false, 0) => "Not reviewed yet".to_string(),
            (true, 0) => "No findings".to_string(),
            (_, 1) => format!("1 finding, {open} open"),
            (_, n) => format!("{n} findings, {open} open"),
        };
        body = body
            .child(div().text_xs().text_color(muted).child(selectable_text(
                "lifecycle-panel-review-summary",
                summary,
                window,
                cx,
            )))
            .child(
                div()
                    .w_full()
                    .rounded_md()
                    .when(self.is_focused(LifecyclePanelStop::Review), |el| {
                        el.border_1().border_color(list_active_border)
                    })
                    .child(
                        Button::new("lifecycle-panel-review")
                            .label(if reviewed { "Review again" } else { "Review" })
                            .when(!reviewed, |b| b.primary())
                            .when(reviewed, |b| b.ghost())
                            .w_full()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.launch_review(window, cx);
                            })),
                    ),
            );
        if let Some(status) = status {
            body = body.child(div().text_xs().text_color(muted).child(selectable_text(
                "lifecycle-panel-review-status",
                status,
                window,
                cx,
            )));
        }
        if open > 0 {
            let needs = if open == 1 {
                "1 finding needs".to_string()
            } else {
                format!("{open} findings need")
            };
            body = body.child(div().text_xs().text_color(danger).child(format!(
                "{needs} a response before approval: fixed, out of scope, declined, or \
                 rejected. Fix in the conversation view resolves them, or answer each \
                 in the findings pane."
            )));
        }
        body
    }

    pub fn is_open(&self) -> bool {
        self.task_id.is_some()
    }

    fn load_task(&mut self, task_id: &str) -> bool {
        // `get_node`, not `get_task`: the panel follows the tree onto nodes
        // without the Agent capability too, and says when one has no lifecycle.
        match self.fleet.get_node(task_id) {
            Ok(Some(task)) => {
                self.title = task.title;
                self.lifecycle = task.lifecycle;
                self.lifecycle_capable = uuid::Uuid::parse_str(task_id)
                    .ok()
                    .and_then(|node_id| self.fleet.list_node_capabilities(node_id).ok())
                    .is_some_and(|caps| {
                        caps.contains(&tod_store::outline::types::Capability::Lifecycle)
                    });
                self.refresh_active_control();
                self.refresh_regression();
                self.refresh_standing();
                self.refresh_incoming();
                self.refresh_learnings();
                true
            }
            _ => false,
        }
    }

    pub fn open(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.task_id = Some(task_id.to_string());
        if !self.load_task(task_id) {
            self.task_id = None;
            return;
        }
        self.controller.update(cx, |controller, _| {
            controller.disarm(task_id);
            controller.load_persisted(task_id);
        });
        self.focus_index = 0;
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.focus_handle.focus(window, cx);
            cx.notify();
        });
    }

    /// Move keyboard focus onto the panel without changing what it targets.
    /// Used when the panel is already open and the user asks to open it
    /// again (e.g. pressing `L` from the node tree) — that should move
    /// focus over, not no-op.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.on_next_frame(window, |this, window, cx| {
            this.focus_handle.focus(window, cx);
            cx.notify();
        });
    }

    /// Switch the panel to a different node. Any gate check already running
    /// (or already completed) for either node is left untouched in
    /// the controller — it keeps running in the background and its status is
    /// shown again if the selection comes back before it finishes.
    pub fn retarget(&mut self, task_id: &str, cx: &mut Context<Self>) {
        if self.task_id.as_deref() == Some(task_id) {
            return;
        }
        let previous = self.task_id.clone();
        self.task_id = Some(task_id.to_string());
        if !self.load_task(task_id) {
            // Never keep showing a node that is no longer selected.
            self.task_id = previous;
            self.close(cx);
            return;
        }
        self.controller.update(cx, |controller, _| {
            controller.disarm(task_id);
            controller.load_persisted(task_id);
        });
        self.focus_index = 0;
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.task_id.is_none() {
            return;
        }
        // Drop finished gate-check state for the closed node, but keep it if
        // a check is still in flight so it can keep running and be polled.
        if let Some(id) = self.task_id.clone() {
            self.controller
                .update(cx, |controller, _| controller.forget(&id));
        }
        self.task_id = None;
        self.title.clear();
        self.lifecycle.clear();
        cx.emit(LifecyclePanelEvent::Close);
        cx.notify();
    }

    /// The controller's state for the panel's node, copied out for render.
    fn current_state<R>(&self, cx: &App, f: impl FnOnce(&GateCheckState) -> R) -> R {
        let empty = GateCheckState::default();
        let controller = self.controller.read(cx);
        let state = self
            .task_id
            .as_deref()
            .and_then(|id| controller.state(id))
            .unwrap_or(&empty);
        f(state)
    }

    fn on_close(&mut self, _: &LifecyclePanelClose, _: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
    }
}

impl EventEmitter<LifecyclePanelEvent> for LifecyclePanelView {}

impl Focusable for LifecyclePanelView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for LifecyclePanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.is_open() {
            return div().size_full().into_any_element();
        }
        self.refresh_active_control();
        self.clamp_focus_index();

        let theme = cx.theme();
        let border = theme.border;
        let background = theme.background;
        let secondary = theme.secondary;
        let muted = theme.muted_foreground;
        let accent = theme.primary;
        let danger = theme.danger;
        let list_active_border = theme.list_active_border;

        let next_state = next_lifecycle(&self.lifecycle);
        let (gate_status, gate_error, criteria_detail, force_advance_armed, revert_armed) = self
            .current_state(cx, |s| {
                (
                    s.gate_status.clone(),
                    s.gate_error.clone(),
                    s.criteria_detail.clone(),
                    s.force_advance_armed,
                    s.revert_armed,
                )
            });

        let mut body = v_flex()
            .id("lifecycle-panel-body")
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_3()
            .overflow_y_scroll()
            .child(div().text_sm().font_semibold().child(self.title.clone()));

        let run_gate_check_focused = self.is_focused(LifecyclePanelStop::RunGateCheck);
        let gate_check_recommended = matches!(self.recommended(), Some(NextStep::GateCheck));
        if !self.lifecycle_capable {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Current selection doesn't have lifecycle capability."),
            );
        } else {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("Current: {}", self.lifecycle)),
            );

            if let Some(found) = self.regression.clone() {
                let focused = self.is_focused(LifecyclePanelStop::MoveBack);
                let mut callout = style::callout_stale(div().w_full())
                    .child(style::callout_stale_title(div()).child(format!(
                        "This node is no longer {} — move it back to {}",
                        self.lifecycle, found.target
                    )))
                    .child(div().child(found.explanation()));
                for (i, reason) in found.reasons.iter().enumerate() {
                    callout = callout.child(selectable_text(
                        format!("lifecycle-panel-regression-{i}"),
                        format!("• {reason}"),
                        window,
                        cx,
                    ));
                }
                body = body.child(
                    callout.child(
                        div()
                            .w_full()
                            .rounded_md()
                            .when(focused, |el| el.border_1().border_color(list_active_border))
                            .child(
                                Button::new("lifecycle-panel-move-back")
                                    .label(format!("Move back to {}", found.target))
                                    .primary()
                                    .w_full()
                                    .on_click(cx.listener(|this, _, _, cx| this.move_back(cx))),
                            ),
                    ),
                );
            }

            if !self.incoming.is_empty() {
                body = body.child(self.render_incoming(window, cx));
            }

            if !self.learnings.is_empty() {
                body = body.child(self.render_learnings(window, cx));
            }

            if let Some(control) = self.active_control {
                body = body.child(div().text_xs().font_semibold().child("Implementation"));
                match control {
                    ActiveControl::NoPlan => {
                        body = body.child(div().text_xs().text_color(danger).child(
                            "This node has no plan steps, so there is nothing to implement \
                         and nothing to check. It should not have got this far: \
                         revert it to planning and give it a plan first.",
                        ));
                    }
                    ActiveControl::Complete { total } => {
                        body = body.child(div().text_xs().text_color(muted).child(format!(
                        "Plan implementation complete — {} done. Run the gate check to advance.",
                        if total == 1 {
                            "the 1 plan step is".to_string()
                        } else {
                            format!("all {total} plan steps are")
                        }
                    )));
                    }
                    ActiveControl::Implement { remaining, total } => {
                        let running = self.implementation_run_live().is_some();
                        let implement_status = self
                            .task_id
                            .as_ref()
                            .and_then(|id| self.implement_status.get(id))
                            .cloned();
                        let (directory, blocked) = match self.implement_directory() {
                            Ok(dir) => (format!("Runs in {dir}"), false),
                            Err(reason) => (reason, true),
                        };
                        let progress = if remaining == 0 {
                            format!("All {total} plan steps closed; the run is finishing up.")
                        } else {
                            format!("{remaining} of {total} plan steps not done yet.")
                        };
                        body =
                            body.child(div().text_xs().text_color(muted).child(selectable_text(
                                "lifecycle-panel-implement-detail",
                                format!("{progress} {directory}"),
                                window,
                                cx,
                            )));
                        body = body.child(
                            div()
                                .w_full()
                                .rounded_md()
                                .when(self.is_focused(LifecyclePanelStop::Implement), |el| {
                                    el.border_1().border_color(list_active_border)
                                })
                                .child(
                                    Button::new("lifecycle-panel-implement")
                                        .label(if running {
                                            "Implementing…"
                                        } else {
                                            "Implement"
                                        })
                                        .primary()
                                        .w_full()
                                        .disabled(blocked)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.launch_implementation(window, cx);
                                        })),
                                ),
                        );
                        if let Some(status) = implement_status {
                            body = body.child(div().text_xs().text_color(muted).child(
                                selectable_text(
                                    "lifecycle-panel-implement-status",
                                    status,
                                    window,
                                    cx,
                                ),
                            ));
                        }
                    }
                }
            }

            if self.lifecycle == "verifying" {
                body = self.render_verification(body, window, cx);
            }
            if self.review_offered() {
                body = self.render_review(body, window, cx);
            }

            // In `active`, the gate check only takes over once the plan is done.
            let gate_check_offered = matches!(
                self.active_control,
                None | Some(ActiveControl::Complete { .. })
            );
            body = match next_state.filter(|_| gate_check_offered) {
                None if !gate_check_offered => body,
                Some(next) => body.child(
                    div()
                        .w_full()
                        .rounded_md()
                        .when(run_gate_check_focused, |el| {
                            el.border_1().border_color(list_active_border)
                        })
                        .child(
                            Button::new("lifecycle-panel-run-gate-check")
                                .label(format!("Run gate check to advance to {next}"))
                                // Only the recommendation once nothing earlier
                                // (verifying, fixing failures, review) is owed.
                                .when(gate_check_recommended, |b| b.primary())
                                .w_full()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.run_gate_check(window, cx);
                                })),
                        ),
                ),
                None => body.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("No further lifecycle state to advance to."),
                ),
            };
        }

        if self.interview_available() {
            let open_interview_focused = self.is_focused(LifecyclePanelStop::OpenInterview);
            body = body.child(
                div()
                    .w_full()
                    .rounded_md()
                    .when(open_interview_focused, |el| {
                        el.border_1().border_color(list_active_border)
                    })
                    .child(
                        Button::new("lifecycle-panel-open-interview")
                            .label(format!(
                                "Open {}",
                                tod_core::process::spec_view_label(&self.lifecycle)
                                    .unwrap_or("Interview")
                                    .to_lowercase()
                            ))
                            .ghost()
                            .w_full()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(task_id) = this.task_id.clone() {
                                    cx.emit(LifecyclePanelEvent::OpenInterview {
                                        task_id,
                                        lifecycle: this.lifecycle.clone(),
                                    });
                                }
                            })),
                    ),
            );
        }

        if let Some(next) = next_state.filter(|_| self.lifecycle_capable) {
            let force_focused = self.is_focused(LifecyclePanelStop::ForceAdvance);
            let armed = force_advance_armed;
            body = body.child(
                div()
                    .w_full()
                    .rounded_md()
                    .when(force_focused, |el| {
                        el.border_1().border_color(list_active_border)
                    })
                    .child(
                        Button::new("lifecycle-panel-force-advance")
                            .label(if armed {
                                format!("Confirm force advance to {next}")
                            } else {
                                format!("Force advance to {next} (bypass gate)")
                            })
                            .ghost()
                            .w_full()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.force_advance(window, cx);
                            })),
                    ),
            );
        }

        if let Some(prev) = previous_lifecycle(&self.lifecycle).filter(|_| self.lifecycle_capable) {
            let revert_focused = self.is_focused(LifecyclePanelStop::RevertLifecycle);
            let armed = revert_armed;
            body = body.child(
                div()
                    .w_full()
                    .rounded_md()
                    .when(revert_focused, |el| {
                        el.border_1().border_color(list_active_border)
                    })
                    .child(
                        Button::new("lifecycle-panel-revert")
                            .label(if armed {
                                format!("Confirm revert to {prev}")
                            } else {
                                format!("Revert to {prev}")
                            })
                            .ghost()
                            .w_full()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.revert_lifecycle(cx);
                            })),
                    ),
            );
        }

        if self.lifecycle_capable {
            if !gate_status.is_empty() {
                body = body.child(div().text_xs().text_color(muted).child(selectable_text(
                    "lifecycle-panel-gate-status",
                    gate_status.clone(),
                    window,
                    cx,
                )));
            }

            if let Some(error) = gate_error {
                body = body.child(div().text_xs().text_color(danger).child(selectable_text(
                    "lifecycle-panel-gate-error",
                    error,
                    window,
                    cx,
                )));
            }

            if !criteria_detail.is_empty() {
                // A real three-column table — Action | Criteria | Explanation
                // — not one run-on wrapped sentence per row. Each column has
                // its own fixed width (the last one flexes to fill what's
                // left) and wraps independently (`whitespace_normal`), so a
                // long criterion label or a long detail string only grows
                // that cell's height, never bleeds into the next column or
                // pushes a button off-panel.
                const ACTION_COL: f32 = 84.0;
                const CRITERION_COL: f32 = 120.0;

                let header = h_flex()
                    .gap_2()
                    .items_start()
                    .w_full()
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(ACTION_COL))
                            .text_xs()
                            .font_semibold()
                            .child("Action"),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(CRITERION_COL))
                            .text_xs()
                            .font_semibold()
                            .child("Criteria"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_semibold()
                            .child("Explanation"),
                    );

                let mut list = v_flex().gap_2().w_full().child(header);
                for (index, row) in criteria_detail.iter().enumerate() {
                    let waivable = row.outcome != OUTCOME_PASS && row.outcome != OUTCOME_WAIVED;
                    let criterion_id = row.criterion_id;
                    let action = row.action;
                    let outcome_color = if row.outcome == OUTCOME_PASS {
                        muted
                    } else if row.outcome == OUTCOME_WAIVED {
                        accent
                    } else {
                        danger
                    };

                    let action_cell = if waivable {
                        let mut buttons = v_flex().gap_1();
                        if action == GateAction::Interview {
                            buttons = buttons.child(
                                Button::new(("lifecycle-panel-criterion-interview", index))
                                    .label(
                                        tod_core::process::spec_view_label(&self.lifecycle)
                                            .unwrap_or("Interview"),
                                    )
                                    .ghost()
                                    .compact()
                                    .w_full()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(task_id) = this.task_id.clone() {
                                            cx.emit(LifecyclePanelEvent::OpenInterview {
                                                task_id,
                                                lifecycle: this.lifecycle.clone(),
                                            });
                                        }
                                    })),
                            );
                        }
                        buttons = buttons.child(
                            Button::new(("lifecycle-panel-waive", index))
                                .label("Waive")
                                .ghost()
                                .compact()
                                .w_full()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.waive_criterion(criterion_id, cx);
                                })),
                        );
                        div().flex_shrink_0().w(px(ACTION_COL)).child(buttons)
                    } else {
                        div()
                            .flex_shrink_0()
                            .w(px(ACTION_COL))
                            .text_xs()
                            .font_semibold()
                            .text_color(outcome_color)
                            .whitespace_normal()
                            .child(if row.outcome == OUTCOME_WAIVED {
                                "Waived"
                            } else {
                                "Pass"
                            })
                    };

                    let row_el = h_flex()
                        .gap_2()
                        .items_start()
                        .w_full()
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(border)
                        .child(action_cell)
                        .child(
                            div()
                                .flex_shrink_0()
                                .w(px(CRITERION_COL))
                                .text_xs()
                                .whitespace_normal()
                                .child(selectable_text(
                                    ("lifecycle-panel-criteria-label", index),
                                    row.label.clone(),
                                    window,
                                    cx,
                                )),
                        )
                        .child({
                            let mut explanation = row.detail.clone().unwrap_or_default();
                            // The agent reports `action: none` both for "nothing to
                            // resolve" (pass/waived, not reached here) and for "no
                            // in-app tool exists for this yet" — a waivable row with
                            // no resolve button and no hint would just look broken,
                            // so make the lack of in-app support explicit rather than
                            // leaving the human to guess why only Waive showed up.
                            if waivable && action == GateAction::None {
                                let note =
                                    "No in-app resolution yet — waive or resolve outside the app.";
                                explanation = if explanation.trim().is_empty() {
                                    note.to_string()
                                } else {
                                    format!("{explanation}\n\n{note}")
                                };
                            }
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(muted)
                                .whitespace_normal()
                                .child(selectable_text(
                                    ("lifecycle-panel-criteria-detail", index),
                                    explanation,
                                    window,
                                    cx,
                                ))
                        });
                    list = list.child(row_el);
                }
                body = body.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_xs().font_semibold().child(
                            "Criteria — Waive lets you accept a specific failure without fixing it",
                        ))
                        .child(list),
                );

                if let Some(next) = next_state {
                    let all_clear = criteria_detail
                        .iter()
                        .all(|r| r.outcome == OUTCOME_PASS || r.outcome == OUTCOME_WAIVED);
                    body = body.child(
                        Button::new("lifecycle-panel-advance-after-criteria")
                            .label(format!("Advance to {next}"))
                            .primary()
                            .w_full()
                            .disabled(!all_clear)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.advance_after_criteria(window, cx);
                            })),
                    );
                }
            }
        }

        v_flex()
            .key_context(LIFECYCLE_PANEL_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .h_full()
            .bg(background)
            .border_l_2()
            .border_color(accent)
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(|_, _: &PaneFocusLeft, _, cx| {
                cx.emit(LifecyclePanelEvent::FocusTaskList);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &LifecyclePanelFocusUp, window, cx| {
                this.move_focus(-1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(
                cx.listener(|this, _: &LifecyclePanelFocusDown, window, cx| {
                    this.move_focus(1, window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_action(cx.listener(|this, _: &LifecyclePanelActivate, window, cx| {
                this.activate_focused(window, cx);
                cx.stop_propagation();
            }))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .bg(secondary)
                    .child(div().text_sm().font_semibold().child(
                        if self.lifecycle_capable && !self.lifecycle.is_empty() {
                            format!("Lifecycle: {}", self.lifecycle)
                        } else {
                            "Lifecycle".to_string()
                        },
                    ))
                    .child(div().flex_1())
                    .child(
                        div()
                            .rounded_md()
                            .when(self.is_focused(LifecyclePanelStop::Close), |el| {
                                el.border_1().border_color(list_active_border)
                            })
                            .child(chrome_control_with_shortcut(
                                Button::new("lifecycle-panel-close")
                                    .label("Close")
                                    .ghost()
                                    .compact()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close(cx);
                                    })),
                                window,
                                &LifecyclePanelClose,
                                LIFECYCLE_PANEL_CONTEXT,
                                cx,
                            )),
                    ),
            )
            .child(body)
            .into_any_element()
    }
}

pub fn register_lifecycle_panel_keyboard_bindings(cx: &mut App) {
    key_context::bind_panel_escape(cx, LifecyclePanelClose, LIFECYCLE_PANEL_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", LifecyclePanelFocusUp, Some(LIFECYCLE_PANEL_CONTEXT)),
        KeyBinding::new(
            "down",
            LifecyclePanelFocusDown,
            Some(LIFECYCLE_PANEL_CONTEXT),
        ),
        KeyBinding::new(
            "enter",
            LifecyclePanelActivate,
            Some(LIFECYCLE_PANEL_CONTEXT),
        ),
        KeyBinding::new(
            "space",
            LifecyclePanelActivate,
            Some(LIFECYCLE_PANEL_CONTEXT),
        ),
    ]);
    bind_modified_pane_nav(cx, LIFECYCLE_PANEL_CONTEXT);
}
