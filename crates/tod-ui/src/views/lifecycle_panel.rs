//! Lifecycle panel — runs an agent-driven gate check to advance a task's
//! lifecycle state (see `TaskListEvent::OpenLifecycle` / `handle_lifecycle_control`
//! in `views/task_list/mod.rs`). This is where Proceed/`L` always lands first,
//! for every phase including ones with an interview — the gate check gets a
//! chance to advance the node on its own before anything falls back to a
//! conversational interview.
//!
//! A click on **Run gate check** sends one one-shot agent turn (mirroring
//! `tod_core::gate`'s context/response split): the state agent for the node's
//! *current* lifecycle evaluates its forward gate and replies with one
//! strict YAML document — `result`, plus, when criteria exist for the
//! transition, a `gate_results` list with per-row `outcome` and `action`.
//! When criteria are present, the reply's `gate_results` are always just
//! recorded (never auto-advances the lifecycle, even on `result: pass`):
//! they render as a table with a Waive button per failing row (and an Open
//! interview button when the agent reports `action: interview`), and a
//! separate **Advance** button — enabled only once every row reads
//! pass/waived — makes the actual lifecycle transition. A prose-only gate
//! (no criteria for the transition) has no table and advances directly off
//! the agent's `result: pass`.
//!
//! Criteria the app can answer from its own data
//! (`tod_core::gate::evaluate_derived_criterion` — e.g. `ready` → `active`'s
//! "has Agent and Files configured") are evaluated directly and saved as `derived`; they
//! are never sent to the agent, and a transition with only such criteria runs
//! no agent turn at all.
//!
//! Three manual escape hatches sit alongside the gate check, each a direct
//! `OutlineMutation::SetLifecycle` that bypasses the gate agent entirely:
//! **Open interview** (jump into that phase's conversational interview even
//! if its session previously ran to exhaustion — a blocked gate check may
//! need input the state agent can't get on its own), **Force advance**
//! (skip the criteria when the user judges a failure isn't worth blocking
//! on), and **Revert** (step back one lifecycle state, e.g. to make a
//! `planning` node re-run its plan-step generation from `design`). Force
//! advance and Revert both require a confirming second click
//! (`GateCheckState::force_advance_armed` / `revert_armed`).

use crate::ui::agent_chat::OpenConversation;
use tod_store::conversation::{Focus, ProtocolKind};
use crate::interview::agent::{AgentRunState, RunId, SharedAgent};
use crate::interview::{TodPaths, TodSettings};
use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, bind_modified_pane_nav};
use crate::ui::selectable_text::selectable_text;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Render, StatefulInteractiveElement, Styled, Window, actions, div,
    px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Disableable, Sizable as _, Size, StyledExt, h_flex, v_flex};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tod_agent::{SessionOpening, SessionPurpose, SessionTurn};
use tod_core::gate::{
    GateAction, GateCheckRequest, PlanStepWithLinks, build_gate_check_message,
    build_on_entry_message, evaluate_derived_criterion, parse_gate_reply,
};
use tod_core::process::interview_phase_for_lifecycle;
use tod_core::process_bundle::{ProcessManifest, TodInstallPaths, state_role_doc};
use tod_core::task::model::{next_lifecycle, previous_lifecycle, state_has_agent};
use tod_store::AgentRole;
use tod_store::fleet::{FleetStore, resolve_launch_cwd};
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::OutlineMutation;
use tod_store::outline::repos::NodeRepo;
use tod_store::outline::{
    GateCriterion, NodeGateEvaluation, OUTCOME_PASS, OUTCOME_WAIVED, SOURCE_AGENT, SOURCE_DERIVED,
    SOURCE_HUMAN,
};

const LIFECYCLE_PANEL_CONTEXT: &str = "LifecyclePanel";
const POLL_INTERVAL: Duration = Duration::from_millis(300);

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
    RunGateCheck,
    OpenInterview,
    ForceAdvance,
    RevertLifecycle,
    Close,
}

/// One row of per-criterion detail shown after a gate check completes.
#[derive(Debug, Clone)]
struct CriterionOutcome {
    criterion_id: uuid::Uuid,
    label: String,
    outcome: String,
    detail: Option<String>,
    /// How the user can resolve this row in-app if it's failing — reported
    /// by the agent per row. `Interview` shows an Open interview button
    /// alongside Waive; `None` leaves Waive as the only option.
    action: GateAction,
}

struct PendingGateCheck {
    run_id: Option<RunId>,
    to_state: String,
}

/// Gate-check state for one node, kept alive independent of whether that
/// node is currently selected — a run started while a node was selected
/// keeps running (and is polled) even after the selection moves away, and
/// is shown again if the selection comes back before it finishes.
#[derive(Default)]
struct GateCheckState {
    pending: Option<PendingGateCheck>,
    gate_status: String,
    gate_error: Option<String>,
    criteria_detail: Vec<CriterionOutcome>,
    /// The criteria catalog fetched for the most recent gate check on this
    /// node, kept around so criteria_detail rows can show a real label
    /// (and so waiving one knows the full set to decide whether every
    /// criterion is now pass/waived).
    criteria_catalog: Vec<GateCriterion>,
    /// Rows the app evaluated itself (`tod_core::gate::evaluate_derived_criterion`)
    /// while an agent turn evaluates the rest — merged into `criteria_detail`
    /// when that reply lands.
    derived_detail: Vec<CriterionOutcome>,
    /// Set after one click on **Revert** — a second click while armed
    /// actually applies it. Keeps an accidental click from reverting a
    /// node's lifecycle without confirmation.
    revert_armed: bool,
    /// Same two-click confirm as `revert_armed`, for **Force advance** —
    /// bypassing the gate criteria entirely rather than stepping back.
    force_advance_armed: bool,
    /// Run id of an in-flight on-entry turn (see `run_on_entry`) — fired
    /// automatically whenever this node's lifecycle actually changes, distinct
    /// from `pending` (a gate check evaluating the *forward* gate). Tracked
    /// separately since it can be running at the same time a fresh gate check
    /// is kicked off for the state just entered.
    on_entry_run: Option<RunId>,
    /// Status line for the on-entry turn, shown alongside `gate_status`.
    on_entry_status: String,
}

pub struct LifecyclePanelView {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    paths: TodPaths,
    task_id: Option<String>,
    title: String,
    lifecycle: String,
    /// Whether the currently displayed node has the Lifecycle capability.
    /// When false, the panel shows a generic message instead of gate-check UI.
    lifecycle_capable: bool,
    gate_states: HashMap<String, GateCheckState>,
    /// Status line for the Active-phase implementation launcher, keyed by
    /// task id (mirrors `gate_states`' per-task keying).
    implement_status: HashMap<String, String>,
    focus_handle: FocusHandle,
    focus_index: usize,
    _poll_task: gpui::Task<()>,
}

impl LifecyclePanelView {
    pub fn new(
        cx: &mut Context<Self>,
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
        paths: TodPaths,
    ) -> Self {
        let poll_entity = cx.weak_entity();
        let _poll_task = cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let _ = poll_entity.update(cx, |this, cx| {
                    if this
                        .gate_states
                        .values()
                        .any(|s| s.pending.is_some() || s.on_entry_run.is_some())
                    {
                        this.poll_gate_checks(cx);
                    }
                });
            }
        });
        Self {
            fleet,
            agent,
            paths,
            task_id: None,
            title: String::new(),
            lifecycle: String::new(),
            lifecycle_capable: false,
            gate_states: HashMap::new(),
            implement_status: HashMap::new(),
            focus_handle: cx.focus_handle(),
            focus_index: 0,
            _poll_task,
        }
    }

    /// Task ids with a gate check (or on-entry run) currently in flight,
    /// independent of which task is selected — used by the app shell to
    /// warn before closing the window while one is still running.
    pub fn running_gate_check_task_ids(&self) -> Vec<String> {
        self.gate_states
            .iter()
            .filter(|(_, state)| state.pending.is_some() || state.on_entry_run.is_some())
            .map(|(task_id, _)| task_id.clone())
            .collect()
    }

    /// Task ids with a gate check or on-entry run currently in flight, paired
    /// with a short human-readable status — for the task list to show a
    /// "running" badge on the row. These turns aren't recorded as agent runs,
    /// so without this a gate check would leave no visible trace anywhere in
    /// the task list while it runs.
    pub fn in_flight_activity(&self) -> HashMap<String, String> {
        self.gate_states
            .iter()
            .filter_map(|(task_id, state)| {
                if state.pending.is_some() {
                    Some((task_id.clone(), state.gate_status.clone()))
                } else if state.on_entry_run.is_some() {
                    Some((task_id.clone(), state.on_entry_status.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    fn stops(&self) -> Vec<LifecyclePanelStop> {
        let mut stops = Vec::new();
        if self.lifecycle_capable && next_lifecycle(&self.lifecycle).is_some() {
            stops.push(LifecyclePanelStop::RunGateCheck);
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
    /// reasons the state agent could only resolve by asking the user, even
    /// on a phase whose interview had previously run to exhaustion (e.g.
    /// after a revert, or once new obligations/plan gaps surface it needs
    /// input on again).
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

    fn activate_focused(&mut self, cx: &mut Context<Self>) {
        match self.focused_stop() {
            Some(LifecyclePanelStop::RunGateCheck) => {
                if let Some(next) = next_lifecycle(&self.lifecycle) {
                    self.run_gate_check(next, cx);
                }
            }
            Some(LifecyclePanelStop::OpenInterview) => {
                if let Some(task_id) = self.task_id.clone() {
                    cx.emit(LifecyclePanelEvent::OpenInterview {
                        task_id,
                        lifecycle: self.lifecycle.clone(),
                    });
                }
            }
            Some(LifecyclePanelStop::ForceAdvance) => self.force_advance(cx),
            Some(LifecyclePanelStop::RevertLifecycle) => self.revert_lifecycle(cx),
            Some(LifecyclePanelStop::Close) => self.close(cx),
            None => {}
        }
    }

    /// Advance the node to the next lifecycle state directly, bypassing the
    /// gate criteria — for when a gate check comes back blocked on failures
    /// the user has judged not worth blocking on. First click arms; a
    /// second click while armed applies it, same as `revert_lifecycle`.
    fn force_advance(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        let Some(next) = next_lifecycle(&self.lifecycle) else {
            return;
        };
        let armed = self
            .gate_states
            .get(&task_id)
            .is_some_and(|s| s.force_advance_armed);
        if !armed {
            let state = self.gate_states.entry(task_id).or_default();
            state.force_advance_armed = true;
            state.gate_error = None;
            state.gate_status = format!(
                "Click Force advance again to confirm — skips the gate criteria, moves to {next}."
            );
            cx.notify();
            return;
        }
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return;
        };
        let state = self.gate_states.entry(task_id.clone()).or_default();
        state.force_advance_armed = false;
        if let Err(err) = self.fleet.enqueue_outline(OutlineMutation::SetLifecycle {
            node_id,
            state: next.to_string(),
        }) {
            let state = self.gate_states.entry(task_id).or_default();
            state.gate_error = Some(format!("Failed to advance lifecycle: {err:#}"));
            return;
        }
        let _ = self.fleet.writer().flush();
        self.lifecycle = next.to_string();
        self.clamp_focus_index();
        if let Some(state) = self.gate_states.get_mut(&task_id) {
            state.gate_status = format!("Advanced to {next} (gate bypassed).");
            state.criteria_detail.clear();
        }
        self.run_on_entry(&task_id, next, cx);
        cx.notify();
    }

    /// Move the node one lifecycle state back, bypassing the forward gate —
    /// e.g. to send a `planning` node back to `design` so the next
    /// transition regenerates its plan steps. First click arms; a second
    /// click while armed applies it (see `GateCheckState::revert_armed`).
    fn revert_lifecycle(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        let Some(prev) = previous_lifecycle(&self.lifecycle) else {
            return;
        };
        let armed = self
            .gate_states
            .get(&task_id)
            .is_some_and(|s| s.revert_armed);
        if !armed {
            let state = self.gate_states.entry(task_id).or_default();
            state.revert_armed = true;
            state.gate_error = None;
            state.gate_status = format!("Click Revert again to confirm — moves back to {prev}.");
            cx.notify();
            return;
        }
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return;
        };
        let state = self.gate_states.entry(task_id.clone()).or_default();
        state.revert_armed = false;
        if let Err(err) = self.fleet.enqueue_outline(OutlineMutation::SetLifecycle {
            node_id,
            state: prev.to_string(),
        }) {
            let state = self.gate_states.entry(task_id).or_default();
            state.gate_error = Some(format!("Failed to revert lifecycle: {err:#}"));
            return;
        }
        let _ = self.fleet.writer().flush();
        self.lifecycle = prev.to_string();
        self.clamp_focus_index();
        if let Some(state) = self.gate_states.get_mut(&task_id) {
            state.gate_status = format!("Reverted to {prev}.");
            state.criteria_detail.clear();
        }
        cx.notify();
    }

    /// Waive one failing gate criterion directly — the fine-grained
    /// alternative to `force_advance` when some failures are fine to ignore
    /// and others genuinely need fixing. Persists as `SOURCE_HUMAN` so it
    /// reads distinctly from an agent's own outcome. This never advances the
    /// lifecycle by itself — once every row reads pass/waived, the user
    /// still clicks the separate Advance button (`advance_after_criteria`)
    /// to make the transition, so a waive can never sneak a node forward
    /// without an explicit confirming click.
    fn waive_criterion(&mut self, criterion_id: uuid::Uuid, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return;
        };
        let Some(state) = self.gate_states.get_mut(&task_id) else {
            return;
        };
        let Some(row) = state
            .criteria_detail
            .iter_mut()
            .find(|r| r.criterion_id == criterion_id)
        else {
            return;
        };
        row.outcome = OUTCOME_WAIVED.to_string();
        row.detail = Some("Waived by user".to_string());

        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::ApplyGateResults {
                node_id,
                results: vec![(
                    criterion_id,
                    OUTCOME_WAIVED.to_string(),
                    Some("Waived by user".to_string()),
                    tod_store::outline::repos::gate::ACTION_NONE.to_string(),
                )],
                forward_state: None,
                source: SOURCE_HUMAN.to_string(),
            })
        {
            if let Some(state) = self.gate_states.get_mut(&task_id) {
                state.gate_error = Some(format!("Failed to waive criterion: {err:#}"));
            }
            cx.notify();
            return;
        }
        let _ = self.fleet.writer().flush();

        if let Some(state) = self.gate_states.get_mut(&task_id) {
            let all_clear = state
                .criteria_detail
                .iter()
                .all(|r| r.outcome == OUTCOME_PASS || r.outcome == OUTCOME_WAIVED);
            state.gate_status = if all_clear {
                "All criteria satisfied — advance when ready.".into()
            } else {
                "Criterion waived.".into()
            };
        }
        cx.notify();
    }

    /// Advance the node to `next_lifecycle`, called only once every row in
    /// the criteria table reads pass/waived (the button that triggers this
    /// is disabled otherwise — see the render below). A pure lifecycle
    /// write, no criteria results to persist since they're already recorded.
    fn advance_after_criteria(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return;
        };
        let Some(next) = next_lifecycle(&self.lifecycle) else {
            return;
        };
        let next = next.to_string();

        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::ApplyGateResults {
                node_id,
                results: Vec::new(),
                forward_state: Some(next.clone()),
                source: SOURCE_HUMAN.to_string(),
            })
        {
            if let Some(state) = self.gate_states.get_mut(&task_id) {
                state.gate_error = Some(format!("Failed to advance lifecycle: {err:#}"));
            }
            cx.notify();
            return;
        }
        let _ = self.fleet.writer().flush();

        if let Some(state) = self.gate_states.get_mut(&task_id) {
            state.gate_status = format!("Advanced to {next}.");
            state.criteria_detail.clear();
        }
        self.lifecycle = next.clone();
        self.clamp_focus_index();
        self.run_on_entry(&task_id, &next, cx);
        cx.notify();
    }

    /// Where implementation would run: the node needs a resolved Agent and a
    /// ready Files directory — what the `ready` → `active` gate requires
    /// (`tod_core::gate::derived`). `Err` carries the user-facing reason.
    fn implement_directory(&self) -> Result<std::path::PathBuf, String> {
        let Some(task_id) = self.task_id.as_ref() else {
            return Err(String::new());
        };
        if self
            .fleet
            .resolve_agent_for_node(task_id)
            .ok()
            .flatten()
            .is_none()
        {
            return Err(
                "Enable the Agent capability on this node (or an ancestor) to implement.".into(),
            );
        }
        resolve_launch_cwd(&self.fleet, task_id).map_err(|err| format!("{err:#}"))
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

    /// Open the node's implementation conversation — one per node, reopened
    /// however many times this is pressed. The conversation view runs it
    /// under the implementation protocol: the agent works in the node's
    /// worktree, its replies are read as reports, and the app keeps sending
    /// it back to the remaining plan steps until the plan is done. See
    /// `doc/conversation/protocols.md`.
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
        // plan has nothing to drive the loop. The lifecycle gate is meant to
        // guarantee one by `ready`; nothing stops a plan being emptied after.
        if !tod_core::conversation::implement::has_plan_steps(&self.fleet, node_id) {
            self.implement_status.insert(
                task_id,
                "This node has no plan steps. Add a plan before implementing.".to_string(),
            );
            cx.notify();
            return;
        }
        self.implement_status.remove(&task_id);
        window.dispatch_action(
            Box::new(OpenConversation {
                focus: Focus::Node(node_id),
                protocol: ProtocolKind::Implementation,
            }),
            cx,
        );
        cx.notify();
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
                true
            }
            _ => false,
        }
    }

    /// Repopulate `criteria_detail` for `task_id` from the most recently
    /// persisted gate-check evaluations for its current forward transition,
    /// so a check run before an app restart (or before the panel was ever
    /// opened this session) reappears without forcing the user to run it
    /// again just to see it. Only fills in state that's still empty — an
    /// in-flight check, or one already populated in memory this session, is
    /// left untouched. The per-row `action` (e.g. an Open-interview button)
    /// isn't persisted, so a reloaded row always falls back to `None`; Waive
    /// is still available, and Open interview remains reachable from the
    /// panel's own button.
    fn load_persisted_gate_state(&mut self, task_id: &str) {
        if self
            .gate_states
            .get(task_id)
            .is_some_and(|s| s.pending.is_some() || !s.criteria_detail.is_empty())
        {
            return;
        }
        let Ok(node_id) = uuid::Uuid::parse_str(task_id) else {
            return;
        };
        let Some(to_state) = next_lifecycle(&self.lifecycle) else {
            return;
        };
        let Ok(rows) = self
            .fleet
            .gate_criteria_for_transition(node_id, &self.lifecycle, to_state)
        else {
            return;
        };
        if !rows.iter().any(|(_, eval)| eval.is_some()) {
            return;
        }

        let criteria_catalog: Vec<GateCriterion> = rows.iter().map(|(c, _)| c.clone()).collect();
        let criteria_detail: Vec<CriterionOutcome> = rows
            .iter()
            .filter_map(|(c, eval): &(GateCriterion, Option<NodeGateEvaluation>)| {
                eval.as_ref().map(|e| CriterionOutcome {
                    criterion_id: c.id,
                    label: c.label.clone(),
                    outcome: e.outcome.clone(),
                    detail: e.detail.clone(),
                    action: GateAction::None,
                })
            })
            .collect();
        if criteria_detail.is_empty() {
            return;
        }

        let all_clear = criteria_detail
            .iter()
            .all(|r| r.outcome == OUTCOME_PASS || r.outcome == OUTCOME_WAIVED);

        let state = self.gate_states.entry(task_id.to_string()).or_default();
        state.criteria_catalog = criteria_catalog;
        state.criteria_detail = criteria_detail;
        state.gate_status = if all_clear {
            "All criteria satisfied — advance when ready.".into()
        } else {
            "Gate check (from last run) — see criteria below.".into()
        };
    }

    pub fn open(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.task_id = Some(task_id.to_string());
        if !self.load_task(task_id) {
            self.task_id = None;
            return;
        }
        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.revert_armed = false;
            state.force_advance_armed = false;
        }
        self.load_persisted_gate_state(task_id);
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
    /// `gate_states` — it keeps running in the background and its status is
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
        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.revert_armed = false;
            state.force_advance_armed = false;
        }
        self.load_persisted_gate_state(task_id);
        self.focus_index = 0;
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.task_id.is_none() {
            return;
        }
        // Drop finished gate-check state for the closed node, but keep it if
        // a check is still in flight so it can keep running and be polled.
        if let Some(id) = self.task_id.as_deref() {
            if self
                .gate_states
                .get(id)
                .is_some_and(|s| s.pending.is_none())
            {
                self.gate_states.remove(id);
            }
        }
        self.task_id = None;
        self.title.clear();
        self.lifecycle.clear();
        cx.emit(LifecyclePanelEvent::Close);
        cx.notify();
    }

    fn current_state(&self) -> Option<&GateCheckState> {
        self.task_id
            .as_deref()
            .and_then(|id| self.gate_states.get(id))
    }

    fn in_flight(&self) -> bool {
        self.current_state().is_some_and(|s| s.pending.is_some())
    }

    fn fail_gate_check(&mut self, task_id: &str, message: String, cx: &mut Context<Self>) {
        let state = self.gate_states.entry(task_id.to_string()).or_default();
        state.pending = None;
        state.gate_error = Some(message);
        state.gate_status = "Gate check failed".into();
        cx.notify();
    }

    /// Kick off one gate-check agent turn for the transition to `to_state`.
    fn run_gate_check(&mut self, to_state: &str, cx: &mut Context<Self>) {
        if self.in_flight() {
            return;
        }
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return;
        };
        let from_state = self.lifecycle.clone();
        let to_state = to_state.to_string();

        {
            let state = self.gate_states.entry(task_id.clone()).or_default();
            state.gate_error = None;
            state.criteria_detail.clear();
            state.gate_status = "Preparing gate check…".into();
            state.pending = Some(PendingGateCheck {
                run_id: None,
                to_state: to_state.clone(),
            });
        }
        cx.notify();

        let criteria =
            match self
                .fleet
                .gate_criteria_for_transition(node_id, &from_state, &to_state)
            {
                Ok(criteria) => criteria,
                Err(err) => return self.fail_gate_check(&task_id, format!("{err:#}"), cx),
            };
        let criteria_catalog: Vec<GateCriterion> =
            criteria.iter().map(|(c, _)| c.clone()).collect();

        // Criteria the app can answer from its own data never reach the
        // agent: it isn't shown that data, so it could only guess.
        let mut derived_detail = Vec::new();
        let mut agent_criteria = Vec::new();
        for (criterion, eval) in criteria {
            match self
                .fleet
                .read(|conn| evaluate_derived_criterion(conn, node_id, &criterion))
            {
                Ok(Some(derived)) => derived_detail.push(CriterionOutcome {
                    criterion_id: criterion.id,
                    label: criterion.label.clone(),
                    outcome: derived.outcome.to_string(),
                    detail: Some(derived.detail),
                    action: GateAction::None,
                }),
                Ok(None) => agent_criteria.push((criterion, eval)),
                Err(err) => return self.fail_gate_check(&task_id, format!("{err:#}"), cx),
            }
        }
        if !derived_detail.is_empty() {
            let results = derived_detail
                .iter()
                .map(|row| {
                    (
                        row.criterion_id,
                        row.outcome.clone(),
                        row.detail.clone(),
                        tod_store::outline::repos::gate::ACTION_NONE.to_string(),
                    )
                })
                .collect();
            if let Err(err) = self
                .fleet
                .enqueue_outline(OutlineMutation::ApplyGateResults {
                    node_id,
                    results,
                    forward_state: None,
                    source: SOURCE_DERIVED.to_string(),
                })
            {
                return self.fail_gate_check(
                    &task_id,
                    format!("Failed to save gate check: {err:#}"),
                    cx,
                );
            }
            let _ = self.fleet.writer().flush();

            // Nothing left for an agent to judge — the table is complete.
            if agent_criteria.is_empty() {
                let all_clear = derived_detail
                    .iter()
                    .all(|r| r.outcome == OUTCOME_PASS || r.outcome == OUTCOME_WAIVED);
                let state = self.gate_states.entry(task_id).or_default();
                state.pending = None;
                state.criteria_catalog = criteria_catalog;
                state.derived_detail.clear();
                state.criteria_detail = derived_detail;
                state.gate_status = if all_clear {
                    "All criteria satisfied — advance when ready.".into()
                } else {
                    "Gate check: blocked — see criteria below.".into()
                };
                cx.notify();
                return;
            }
        }
        if let Some(state) = self.gate_states.get_mut(&task_id) {
            state.derived_detail = derived_detail;
        }

        let result: anyhow::Result<(SessionTurn, String)> = (|| {
            let settings = TodSettings::load(&self.paths).unwrap_or_default();
            let cwd = self.fleet.files_dir_or_data_root(&task_id);
            let options = self
                .fleet
                .resolve_agent_for_node(&task_id)
                .ok()
                .flatten()
                .map(|agent| agent.launch_options(&settings, AgentRole::Default))
                .unwrap_or_else(|| settings.launch_options_for(AgentRole::Default));
            let body = self
                .fleet
                .get_extra_content(node_id, EXTRA_CONTENT_DETAILS)
                .ok()
                .flatten();
            let obligations = self
                .fleet
                .list_obligations_for_node(node_id)
                .unwrap_or_default();
            let ancestor_context = self
                .fleet
                .read(|conn| {
                    tod_core::node_context::render_inherited_context(
                        conn,
                        &NodeRepo::new(conn),
                        node_id,
                        None,
                    )
                })
                .unwrap_or_default();
            let plan_steps = self
                .fleet
                .list_plan_steps_for_node(node_id)
                .unwrap_or_default()
                .into_iter()
                .map(|step| {
                    let depends_on = self
                        .fleet
                        .list_plan_step_dependencies(step.id)
                        .unwrap_or_default();
                    let satisfies = self
                        .fleet
                        .list_plan_step_obligations(step.id)
                        .unwrap_or_default();
                    PlanStepWithLinks {
                        step,
                        depends_on,
                        satisfies,
                    }
                })
                .collect();
            let media = tod_core::media::MediaPaths::discover()?;
            let install = TodInstallPaths::discover()?;
            let manifest = ProcessManifest::load(&install)?;
            let role_doc = state_role_doc(&manifest, &from_state)?;
            let node_title = self.title.clone();
            let message = build_gate_check_message(
                &media,
                &GateCheckRequest {
                    data_root: self.paths.data_root(),
                    node_id,
                    node_title: node_title.clone(),
                    node_lifecycle: from_state.clone(),
                    node_body: body,
                    obligations,
                    ancestor_context,
                    plan_steps,
                    from_state: from_state.clone(),
                    to_state: to_state.clone(),
                    criteria: agent_criteria,
                },
                &role_doc,
            )?;
            let session_title =
                format!("Gate check: {node_title} ({from_state} \u{2192} {to_state})");
            let turn = SessionTurn {
                key: format!("gate-check-{}", uuid::Uuid::new_v4()),
                owner_id: task_id.to_string(),
                title: session_title,
                cwd,
                options,
                resume_session_id: None,
                opening: Some(SessionOpening { context: None }),
                message,
                purpose: SessionPurpose::Chat,
                env: Vec::new(),
            };
            Ok((turn, to_state.clone()))
        })();

        match result {
            Ok((turn, _)) => {
                let sent = match self.agent.lock() {
                    Ok(mut provider) => provider.send_session_turn(turn),
                    Err(_) => Err(anyhow::anyhow!("Agent busy — try again shortly")),
                };
                match sent {
                    Ok(handle) => {
                        if let Some(state) = self.gate_states.get_mut(&task_id) {
                            if let Some(pending) = state.pending.as_mut() {
                                pending.run_id = Some(handle.id);
                            }
                            state.gate_status = "Running gate check…".into();
                            state.criteria_catalog = criteria_catalog;
                        }
                    }
                    Err(err) => {
                        self.fail_gate_check(&task_id, format!("Launch agent failed: {err:#}"), cx)
                    }
                }
            }
            Err(err) => self.fail_gate_check(&task_id, format!("{err:#}"), cx),
        }
        cx.notify();
    }

    /// Poll every node with an in-flight gate check, not just the currently
    /// selected one — a check keeps running after the selection moves away.
    fn poll_gate_checks(&mut self, cx: &mut Context<Self>) {
        let pending_ids: Vec<String> = self
            .gate_states
            .iter()
            .filter(|(_, s)| s.pending.is_some() || s.on_entry_run.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for task_id in pending_ids {
            self.poll_gate_check(&task_id, cx);
            self.poll_on_entry(&task_id, cx);
        }
    }

    /// Fire the on-entry turn for `task_id`, which just landed in `lifecycle` —
    /// the new state's own agent doing its state's "On entry" responsibilities
    /// (e.g. `planning` writing plan steps), per `assets/process/agents/state/base.md`.
    /// Called automatically from every place a lifecycle transition actually
    /// lands: an agent's own gate-check pass, the human Advance button after
    /// waiving criteria, and Force advance. Idempotent by design (the prompt
    /// tells the agent to add only what's missing), so it's safe to fire again
    /// later if this node re-enters the same state. Looks up the node's own
    /// title/repo rather than trusting `self.title` — a transition can land
    /// while a different node is selected in the panel (see `apply_gate_reply`).
    fn run_on_entry(&mut self, task_id: &str, lifecycle: &str, cx: &mut Context<Self>) {
        if !state_has_agent(lifecycle) {
            return;
        }
        if self
            .gate_states
            .get(task_id)
            .is_some_and(|s| s.on_entry_run.is_some())
        {
            return;
        }
        let Ok(node_id) = uuid::Uuid::parse_str(task_id) else {
            return;
        };
        let task_id = task_id.to_string();
        let lifecycle = lifecycle.to_string();
        let title = self
            .fleet
            .get_task(&task_id)
            .ok()
            .flatten()
            .map(|t| t.title)
            .unwrap_or_default();
        let data_root = self.paths.data_root().to_path_buf();

        let result: anyhow::Result<SessionTurn> = (|| {
            let settings = TodSettings::load(&self.paths).unwrap_or_default();
            let cwd = self.fleet.files_dir_or_data_root(&task_id);
            let options = self
                .fleet
                .resolve_agent_for_node(&task_id)
                .ok()
                .flatten()
                .map(|agent| agent.launch_options(&settings, AgentRole::Default))
                .unwrap_or_else(|| settings.launch_options_for(AgentRole::Default));
            let body = self
                .fleet
                .get_extra_content(node_id, EXTRA_CONTENT_DETAILS)
                .ok()
                .flatten();
            let obligations = self
                .fleet
                .list_obligations_for_node(node_id)
                .unwrap_or_default();
            let ancestor_context = self
                .fleet
                .read(|conn| {
                    tod_core::node_context::render_inherited_context(
                        conn,
                        &NodeRepo::new(conn),
                        node_id,
                        None,
                    )
                })
                .unwrap_or_default();
            let plan_steps = self
                .fleet
                .list_plan_steps_for_node(node_id)
                .unwrap_or_default()
                .into_iter()
                .map(|step| {
                    let depends_on = self
                        .fleet
                        .list_plan_step_dependencies(step.id)
                        .unwrap_or_default();
                    let satisfies = self
                        .fleet
                        .list_plan_step_obligations(step.id)
                        .unwrap_or_default();
                    PlanStepWithLinks {
                        step,
                        depends_on,
                        satisfies,
                    }
                })
                .collect();
            let media = tod_core::media::MediaPaths::discover()?;
            let install = TodInstallPaths::discover()?;
            let manifest = ProcessManifest::load(&install)?;
            let role_doc = state_role_doc(&manifest, &lifecycle)?;
            let message = build_on_entry_message(
                &media,
                &GateCheckRequest {
                    data_root: &data_root,
                    node_id,
                    node_title: title.clone(),
                    node_lifecycle: lifecycle.clone(),
                    node_body: body,
                    obligations,
                    ancestor_context,
                    plan_steps,
                    from_state: lifecycle.clone(),
                    to_state: lifecycle.clone(),
                    criteria: Vec::new(),
                },
                &role_doc,
            )?;
            Ok(SessionTurn {
                key: format!("on-entry-{}", uuid::Uuid::new_v4()),
                owner_id: task_id.to_string(),
                title: format!("On entry: {title} ({lifecycle})"),
                cwd,
                options,
                resume_session_id: None,
                opening: Some(SessionOpening { context: None }),
                message,
                purpose: SessionPurpose::Chat,
                env: Vec::new(),
            })
        })();

        match result {
            Ok(turn) => {
                let sent = match self.agent.lock() {
                    Ok(mut provider) => provider.send_session_turn(turn),
                    Err(_) => Err(anyhow::anyhow!("Agent busy — try again shortly")),
                };
                let state = self.gate_states.entry(task_id).or_default();
                match sent {
                    Ok(handle) => {
                        state.on_entry_run = Some(handle.id);
                        state.on_entry_status = format!("Running {lifecycle} on-entry setup…");
                    }
                    Err(err) => {
                        state.on_entry_status = format!("On-entry setup failed to launch: {err:#}");
                    }
                }
            }
            Err(err) => {
                let state = self.gate_states.entry(task_id).or_default();
                state.on_entry_status = format!("On-entry setup failed: {err:#}");
            }
        }
        cx.notify();
    }

    fn poll_on_entry(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let Some(run_id) = self.gate_states.get(task_id).and_then(|s| s.on_entry_run) else {
            return;
        };
        let Ok(mut agent) = self.agent.try_lock() else {
            return;
        };
        let Some(run_state) = agent.poll_run(run_id) else {
            return;
        };
        drop(agent);

        match run_state {
            AgentRunState::InFlight(activity) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    state.on_entry_status =
                        activity.unwrap_or_else(|| "Running on-entry setup…".into());
                }
            }
            AgentRunState::NeedsPermission(request) => {
                crate::ui::agent_permission::queue_permission_request(self.agent.clone(), request);
            }
            AgentRunState::Success(response) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    state.on_entry_run = None;
                    let summary = response.unwrap_or_default();
                    let summary = summary.trim();
                    state.on_entry_status = if summary.is_empty() {
                        "On-entry setup complete.".into()
                    } else {
                        format!("On-entry setup: {summary}")
                    };
                }
            }
            AgentRunState::Failure(message) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    state.on_entry_run = None;
                    state.on_entry_status = format!("On-entry setup failed: {message}");
                }
            }
        }
        cx.notify();
    }

    fn poll_gate_check(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let Some(run_id) = self
            .gate_states
            .get(task_id)
            .and_then(|s| s.pending.as_ref())
            .and_then(|p| p.run_id)
        else {
            return;
        };
        let Ok(mut agent) = self.agent.try_lock() else {
            return;
        };
        let Some(run_state) = agent.poll_run(run_id) else {
            return;
        };
        drop(agent);

        let to_state = match self
            .gate_states
            .get(task_id)
            .and_then(|s| s.pending.as_ref())
        {
            Some(p) => p.to_state.clone(),
            None => return,
        };

        match run_state {
            AgentRunState::InFlight(activity) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    state.gate_status = activity.unwrap_or_else(|| "Running gate check…".into());
                }
            }
            AgentRunState::NeedsPermission(request) => {
                crate::ui::agent_permission::queue_permission_request(self.agent.clone(), request);
            }
            AgentRunState::Success(response) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    state.pending = None;
                }
                self.apply_gate_reply(task_id, response.unwrap_or_default(), &to_state, cx);
            }
            AgentRunState::Failure(message) => {
                if let Some(state) = self.gate_states.get_mut(task_id) {
                    state.pending = None;
                    state.gate_error = Some(message);
                    state.gate_status = "Gate check failed".into();
                }
            }
        }
        cx.notify();
    }

    fn apply_gate_reply(
        &mut self,
        task_id: &str,
        text: String,
        to_state: &str,
        cx: &mut Context<Self>,
    ) {
        let Ok(node_id) = uuid::Uuid::parse_str(task_id) else {
            return;
        };

        let reply = match parse_gate_reply(&text) {
            Ok(reply) => reply,
            Err(err) => {
                let state = self.gate_states.entry(task_id.to_string()).or_default();
                state.gate_error = Some(format!("Could not parse agent reply: {err:#}"));
                state.gate_status = "Gate check reply was not understood".into();
                return;
            }
        };

        let catalog = self
            .gate_states
            .get(task_id)
            .map(|s| s.criteria_catalog.clone())
            .unwrap_or_default();
        let label_for = |id: uuid::Uuid| {
            catalog
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.label.clone())
                .unwrap_or_else(|| id.to_string())
        };
        // Whether this transition has structured criteria at all — decides
        // whether advancing needs a separate user click on the criteria
        // table (built below) or can follow the agent's own verdict
        // directly, for a prose-only gate with nothing to show a table for.
        let has_criteria = !catalog.is_empty();

        let results: Vec<(uuid::Uuid, String, Option<String>, String)> = reply
            .gate_results
            .iter()
            .map(|row| {
                let action = if row.action == GateAction::Interview {
                    tod_store::outline::repos::gate::ACTION_INTERVIEW
                } else {
                    tod_store::outline::repos::gate::ACTION_NONE
                };
                (
                    row.criterion_id,
                    row.outcome.clone(),
                    row.detail.clone(),
                    action.to_string(),
                )
            })
            .collect();
        // Rows the app evaluated itself (already saved when the check began)
        // join the agent's rows, back in catalog order.
        let derived_detail = self
            .gate_states
            .get_mut(task_id)
            .map(|s| std::mem::take(&mut s.derived_detail))
            .unwrap_or_default();
        let mut criteria_detail: Vec<CriterionOutcome> = derived_detail
            .into_iter()
            .chain(reply.gate_results.iter().map(|row| CriterionOutcome {
                criterion_id: row.criterion_id,
                label: label_for(row.criterion_id),
                outcome: row.outcome.clone(),
                detail: row.detail.clone(),
                action: row.action,
            }))
            .collect();
        criteria_detail.sort_by_key(|r| catalog.iter().position(|c| c.id == r.criterion_id));

        let all_clear = !criteria_detail.is_empty()
            && criteria_detail
                .iter()
                .all(|r| r.outcome == OUTCOME_PASS || r.outcome == OUTCOME_WAIVED);
        // A gate with structured criteria always stops here and shows the
        // table — even a `result: pass` reply only records the agent's
        // per-row verdicts; advancing the lifecycle is a separate, explicit
        // click on the Advance button once every row is pass/waived. Only a
        // prose-only gate (no criteria at all) can advance directly off the
        // agent's own result.
        let advances = reply.result.advances() && !has_criteria;
        let forward_state = advances.then(|| to_state.to_string());

        if !results.is_empty() || advances {
            if let Err(err) = self
                .fleet
                .enqueue_outline(OutlineMutation::ApplyGateResults {
                    node_id,
                    results,
                    forward_state: forward_state.clone(),
                    source: SOURCE_AGENT.to_string(),
                })
            {
                let state = self.gate_states.entry(task_id.to_string()).or_default();
                state.gate_error = Some(format!("Failed to save gate check: {err:#}"));
                return;
            }
            let _ = self.fleet.writer().flush();
        }

        let state = self.gate_states.entry(task_id.to_string()).or_default();
        state.criteria_detail = criteria_detail;
        if let Some(new_state) = forward_state.clone() {
            state.gate_status = format!("Advanced to {new_state}.");
        } else if has_criteria {
            state.gate_status = if all_clear {
                "All criteria satisfied — advance when ready.".into()
            } else if reply.paused {
                "Gate check: blocked — see criteria below.".into()
            } else {
                "Gate check did not advance the lifecycle — see criteria below.".into()
            };
        } else {
            state.gate_status = if reply.paused {
                "Gate check: blocked — see findings below.".into()
            } else {
                "Gate check did not advance the lifecycle.".into()
            };
        }
        if !reply.findings.trim().is_empty() {
            state.gate_error = None;
        }
        let _ = reply.findings; // surfaced via gate_status/criteria_detail for now

        // Only update the live lifecycle label / focus stops when the check
        // that just finished belongs to the node currently on screen — but
        // fire the new state's on-entry turn regardless of selection, same as
        // the gate check itself kept running in the background for it.
        if let Some(new_state) = forward_state {
            if self.task_id.as_deref() == Some(task_id) {
                self.lifecycle = new_state.clone();
                self.clamp_focus_index();
            }
            self.run_on_entry(task_id, &new_state, cx);
        }
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

        let theme = cx.theme();
        let border = theme.border;
        let background = theme.background;
        let secondary = theme.secondary;
        let muted = theme.muted_foreground;
        let accent = theme.primary;
        let danger = theme.danger;
        let list_active_border = theme.list_active_border;

        let next_state = next_lifecycle(&self.lifecycle);
        let in_flight = self.in_flight();
        let empty_state = GateCheckState::default();
        let gate_state = self.current_state().unwrap_or(&empty_state);
        let gate_status = gate_state.gate_status.clone();
        let gate_error = gate_state.gate_error.clone();
        let criteria_detail = gate_state.criteria_detail.clone();
        let on_entry_running = gate_state.on_entry_run.is_some();
        let on_entry_status = gate_state.on_entry_status.clone();

        let mut body = v_flex()
            .id("lifecycle-panel-body")
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_3()
            .overflow_y_scroll()
            .child(div().text_sm().font_semibold().child(self.title.clone()));

        let run_gate_check_focused = self.is_focused(LifecyclePanelStop::RunGateCheck);
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

            if self.lifecycle == "active" {
                let directory = self.implement_directory();
                let live_run = self.implementation_run_live();
                let implement_status = self
                    .task_id
                    .as_ref()
                    .and_then(|id| self.implement_status.get(id))
                    .cloned();
                body = body.child(div().text_xs().font_semibold().child("Implementation"));
                let (detail, blocked) = match &directory {
                    Ok(dir) => (format!("Runs in {}", dir.display()), false),
                    Err(reason) => (reason.clone(), true),
                };
                let label = if live_run.is_some() {
                    "Implementing…"
                } else {
                    "Implement"
                };
                body =
                    body.child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().text_xs().text_color(muted).child(
                                selectable_text(
                                    "lifecycle-panel-implement-detail",
                                    detail,
                                    window,
                                    cx,
                                ),
                            ))
                            .child(
                                Button::new("lifecycle-panel-implement")
                                    .label(label)
                                    .ghost()
                                    .disabled(blocked)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.launch_implementation(window, cx);
                                    })),
                            ),
                    );
                if let Some(status) = implement_status {
                    body = body.child(div().text_xs().text_color(muted).child(selectable_text(
                        "lifecycle-panel-implement-status",
                        status,
                        window,
                        cx,
                    )));
                }
            }

            body = match next_state {
                Some(next) => body.child(
                    div()
                        .w_full()
                        .rounded_md()
                        .when(run_gate_check_focused, |el| {
                            el.border_1().border_color(list_active_border)
                        })
                        .child(
                            Button::new("lifecycle-panel-run-gate-check")
                                .label(if in_flight {
                                    format!("Running gate check to advance to {next}…")
                                } else {
                                    format!("Run gate check to advance to {next}")
                                })
                                .primary()
                                .w_full()
                                .disabled(in_flight)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.run_gate_check(next, cx);
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
            let armed = gate_state.force_advance_armed;
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
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.force_advance(cx);
                            })),
                    ),
            );
        }

        if let Some(prev) = previous_lifecycle(&self.lifecycle).filter(|_| self.lifecycle_capable) {
            let revert_focused = self.is_focused(LifecyclePanelStop::RevertLifecycle);
            let armed = gate_state.revert_armed;
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
            if in_flight {
                body = body.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().with_size(Size::Small))
                        .child(div().text_xs().text_color(muted).child(selectable_text(
                            "lifecycle-panel-gate-status",
                            gate_status.clone(),
                            window,
                            cx,
                        ))),
                );
            } else if !gate_status.is_empty() {
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

            if on_entry_running {
                body = body.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().with_size(Size::Small))
                        .child(div().text_xs().text_color(muted).child(selectable_text(
                            "lifecycle-panel-on-entry-status",
                            on_entry_status.clone(),
                            window,
                            cx,
                        ))),
                );
            } else if !on_entry_status.is_empty() {
                body = body.child(div().text_xs().text_color(muted).child(selectable_text(
                    "lifecycle-panel-on-entry-status",
                    on_entry_status.clone(),
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
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.advance_after_criteria(cx);
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
            .on_action(cx.listener(|this, _: &LifecyclePanelActivate, _, cx| {
                this.activate_focused(cx);
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
                    .child(div().text_sm().font_semibold().child("Lifecycle"))
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
