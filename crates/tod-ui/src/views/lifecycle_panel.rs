//! Lifecycle panel — runs an agent-driven gate check to advance a task's
//! lifecycle state (see `TaskListEvent::OpenLifecycle` / `handle_lifecycle_control`
//! in `views/task_list/mod.rs`). This is where Proceed/`L` always lands first,
//! for every phase including ones with an interview — the gate check gets a
//! chance to advance the node on its own before anything falls back to a
//! conversational interview.
//!
//! A click on **Run gate check** sends one one-shot agent turn (mirroring
//! `tod_core::gate`'s context/response split): the state agent for the node's
//! *current* lifecycle evaluates its forward gate and replies with YAML front
//! matter plus, when criteria exist for the transition, a `gate_results`
//! section. The reply is parsed and only applied — `OutlineMutation::SetLifecycle`
//! bundled with the gate_results write — when the agent reports `result: pass`.
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

use crate::interview::agent::{AgentRunState, RunId, SharedAgent};
use crate::interview::{TodPaths, TodSettings};
use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, bind_modified_pane_nav};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Render, StatefulInteractiveElement, Styled, Timer, Window, actions,
    div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Disableable, Sizable as _, Size, StyledExt, h_flex, v_flex};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tod_agent::{SessionOpening, SessionPurpose, SessionTurn};
use tod_core::gate::{
    GateCheckRequest, PlanStepWithLinks, build_gate_check_message, parse_gate_reply,
};
use tod_core::process::interview_phase_for_lifecycle;
use tod_core::task::model::{next_lifecycle, previous_lifecycle};
use tod_store::AgentRole;
use tod_store::fleet::{FleetStore, ensure_interview_agent_for_node};
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::OutlineMutation;

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
    OpenInterview { task_id: String, lifecycle: String },
    /// Clicked the visual-design affordance on the
    /// `design-planning.visual-packages-accepted-or-waived` criterion row.
    OpenVisualDesign { task_id: String },
}

/// The `design-planning.visual-packages-accepted-or-waived` gate criterion's
/// id (see `tod_store::outline::GATE_CRITERIA`) — `apply_gate_reply` stores a
/// criterion's id (not its slug) as `CriterionOutcome::label`, so this is
/// resolved once and matched against that field to show the visual-design
/// affordance only on that row.
fn visual_design_criterion_id() -> Option<String> {
    tod_store::outline::GATE_CRITERIA
        .iter()
        .find(|c| c.slug == "design-planning.visual-packages-accepted-or-waived")
        .map(|c| c.id_str.to_string())
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
    label: String,
    outcome: String,
    detail: Option<String>,
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
    /// Set after one click on **Revert** — a second click while armed
    /// actually applies it. Keeps an accidental click from reverting a
    /// node's lifecycle without confirmation.
    revert_armed: bool,
    /// Same two-click confirm as `revert_armed`, for **Force advance** —
    /// bypassing the gate criteria entirely rather than stepping back.
    force_advance_armed: bool,
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
                Timer::after(POLL_INTERVAL).await;
                let _ = poll_entity.update(cx, |this, cx| {
                    if this.gate_states.values().any(|s| s.pending.is_some()) {
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
            focus_handle: cx.focus_handle(),
            focus_index: 0,
            _poll_task,
        }
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
        self.focus_handle.focus(window);
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
            state.gate_status =
                format!("Click Force advance again to confirm — skips the gate criteria, moves to {next}.");
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

    pub fn is_open(&self) -> bool {
        self.task_id.is_some()
    }

    fn load_task(&mut self, task_id: &str) -> bool {
        match self.fleet.get_task(task_id) {
            Ok(Some(task)) => {
                self.title = task.title;
                self.lifecycle = task.lifecycle;
                self.lifecycle_capable = uuid::Uuid::parse_str(task_id)
                    .ok()
                    .and_then(|node_id| self.fleet.list_node_capabilities(node_id).ok())
                    .is_some_and(|caps| caps.contains(&tod_store::outline::types::Capability::Lifecycle));
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
        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.revert_armed = false;
            state.force_advance_armed = false;
        }
        self.focus_index = 0;
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.focus_handle.focus(window);
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
            self.task_id = previous;
            return;
        }
        if let Some(state) = self.gate_states.get_mut(task_id) {
            state.revert_armed = false;
            state.force_advance_armed = false;
        }
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
            if self.gate_states.get(id).is_some_and(|s| s.pending.is_none()) {
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
        self.task_id.as_deref().and_then(|id| self.gate_states.get(id))
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

        let result: anyhow::Result<(SessionTurn, String)> = (|| {
            let settings = TodSettings::load(&self.paths).unwrap_or_default();
            let agent_ctx =
                ensure_interview_agent_for_node(&self.fleet, &self.paths, &settings, &task_id)?;
            let criteria =
                self.fleet
                    .gate_criteria_for_transition(node_id, &from_state, &to_state)?;
            let purposes = self.fleet.ancestor_purposes(node_id).unwrap_or_default();
            let body = self
                .fleet
                .get_extra_content(node_id, EXTRA_CONTENT_DETAILS)
                .ok()
                .flatten();
            let obligations = self.fleet.resolve_obligations_for_node(node_id).unwrap_or_default();
            let plan_steps = self
                .fleet
                .list_plan_steps_for_node(node_id)
                .unwrap_or_default()
                .into_iter()
                .map(|step| {
                    let depends_on = self.fleet.list_plan_step_dependencies(step.id).unwrap_or_default();
                    let satisfies = self.fleet.list_plan_step_obligations(step.id).unwrap_or_default();
                    PlanStepWithLinks { step, depends_on, satisfies }
                })
                .collect();
            let media = tod_core::media::MediaPaths::discover()?;
            let message = build_gate_check_message(
                &media,
                &GateCheckRequest {
                    data_root: self.paths.data_root(),
                    node_id,
                    node_title: self.title.clone(),
                    node_lifecycle: from_state.clone(),
                    node_body: body,
                    purposes,
                    obligations,
                    plan_steps,
                    from_state: from_state.clone(),
                    to_state: to_state.clone(),
                    criteria,
                },
            )?;
            let session_title = format!("{from_state}-to-{to_state} gate");
            let turn = SessionTurn {
                key: format!("gate-check-{}", uuid::Uuid::new_v4()),
                agent_config_id: agent_ctx.agent.id.clone(),
                cwd: agent_ctx.cwd,
                options: settings.launch_options_for(AgentRole::Default),
                resume_session_id: None,
                opening: Some(SessionOpening {
                    title: session_title,
                    context: None,
                }),
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
            .filter(|(_, s)| s.pending.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for task_id in pending_ids {
            self.poll_gate_check(&task_id, cx);
        }
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

        let to_state = match self.gate_states.get(task_id).and_then(|s| s.pending.as_ref()) {
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
                crate::ui::agent_permission::queue_permission_request(
                    self.agent.clone(),
                    request,
                );
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
        _cx: &mut Context<Self>,
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

        let results: Vec<(uuid::Uuid, String, Option<String>)> = reply
            .gate_results
            .iter()
            .map(|row| (row.criterion_id, row.outcome.clone(), row.detail.clone()))
            .collect();
        let criteria_detail = reply
            .gate_results
            .iter()
            .map(|row| CriterionOutcome {
                label: row.criterion_id.to_string(),
                outcome: row.outcome.clone(),
                detail: row.detail.clone(),
            })
            .collect();

        let advances = reply.result.advances();
        let forward_state = advances.then(|| to_state.to_string());

        if !results.is_empty() || advances {
            if let Err(err) = self.fleet.enqueue_outline(OutlineMutation::ApplyGateResults {
                node_id,
                results,
                forward_state: forward_state.clone(),
            }) {
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
        // that just finished belongs to the node currently on screen.
        if let Some(new_state) = forward_state {
            if self.task_id.as_deref() == Some(task_id) {
                self.lifecycle = new_state;
                self.clamp_focus_index();
            }
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

        let next_state = next_lifecycle(&self.lifecycle);
        let in_flight = self.in_flight();
        let empty_state = GateCheckState::default();
        let gate_state = self.current_state().unwrap_or(&empty_state);
        let gate_status = gate_state.gate_status.clone();
        let gate_error = gate_state.gate_error.clone();
        let criteria_detail = gate_state.criteria_detail.clone();

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
            body = match next_state {
                Some(next) => body.child(
                    div()
                        .w_full()
                        .rounded_md()
                        .when(run_gate_check_focused, |el| {
                            el.border_1().border_color(theme.list_active_border)
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
                        el.border_1().border_color(theme.list_active_border)
                    })
                    .child(
                        Button::new("lifecycle-panel-open-interview")
                            .label("Open interview")
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
                        el.border_1().border_color(theme.list_active_border)
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
                        el.border_1().border_color(theme.list_active_border)
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
                        .child(div().text_xs().text_color(muted).child(gate_status.clone())),
                );
            } else if !gate_status.is_empty() {
                body = body.child(div().text_xs().text_color(muted).child(gate_status.clone()));
            }

            if let Some(error) = gate_error {
                body = body.child(div().text_xs().text_color(danger).child(error));
            }

            if !criteria_detail.is_empty() {
                let visual_design_id = visual_design_criterion_id();
                let mut list = v_flex().gap_1().w_full();
                for row in &criteria_detail {
                    let mut line = format!("{}: {}", row.label, row.outcome);
                    if let Some(detail) = row.detail.as_deref() {
                        line.push_str(&format!(" — {detail}"));
                    }
                    let mut item = h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().text_xs().text_color(muted).child(line));
                    if visual_design_id.as_deref() == Some(row.label.as_str()) {
                        item = item.child(
                            Button::new("lifecycle-panel-open-visual-design")
                                .label("Visual design")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(task_id) = this.task_id.clone() {
                                        cx.emit(LifecyclePanelEvent::OpenVisualDesign { task_id });
                                    }
                                })),
                        );
                    }
                    list = list.child(item);
                }
                body = body.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_xs().font_semibold().child("Criteria"))
                        .child(list),
                );
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
            .on_action(cx.listener(|this, _: &LifecyclePanelFocusDown, window, cx| {
                this.move_focus(1, window, cx);
                cx.stop_propagation();
            }))
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
                                el.border_1().border_color(theme.list_active_border)
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
        KeyBinding::new("down", LifecyclePanelFocusDown, Some(LIFECYCLE_PANEL_CONTEXT)),
        KeyBinding::new("enter", LifecyclePanelActivate, Some(LIFECYCLE_PANEL_CONTEXT)),
        KeyBinding::new("space", LifecyclePanelActivate, Some(LIFECYCLE_PANEL_CONTEXT)),
    ]);
    bind_modified_pane_nav(cx, LIFECYCLE_PANEL_CONTEXT);
}
