//! Lifecycle panel — runs an agent-driven gate check to advance a task's
//! lifecycle state (see `TaskListEvent::OpenLifecycle` / `handle_lifecycle_control`
//! in `views/task_list/mod.rs`).
//!
//! A click on **Run gate check** sends one one-shot agent turn (mirroring
//! `tod_core::gate`'s context/response split): the state agent for the node's
//! *current* lifecycle evaluates its forward gate and replies with YAML front
//! matter plus, when criteria exist for the transition, a `gate_results`
//! section. The reply is parsed and only applied — `OutlineMutation::SetLifecycle`
//! bundled with the gate_results write — when the agent reports `result: pass`.

use crate::interview::agent::{AgentRunState, RunId, SharedAgent};
use crate::interview::{TodPaths, TodSettings};
use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, bind_modified_pane_nav};
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Timer, Window, actions, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Disableable, Sizable as _, Size, StyledExt, h_flex, v_flex};
use std::sync::Arc;
use std::time::Duration;
use tod_agent::{SessionOpening, SessionPurpose, SessionTurn};
use tod_core::gate::{GateCheckRequest, build_gate_check_message, parse_gate_reply};
use tod_core::task::model::next_lifecycle;
use tod_store::AgentRole;
use tod_store::fleet::{FleetStore, ensure_interview_agent_for_node};
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::OutlineMutation;

const LIFECYCLE_PANEL_CONTEXT: &str = "LifecyclePanel";
const POLL_INTERVAL: Duration = Duration::from_millis(300);

actions!(lifecycle_panel, [LifecyclePanelClose]);

#[derive(Debug, Clone)]
pub enum LifecyclePanelEvent {
    Close,
    /// Escape / Ctrl+Left — move keyboard focus back to the task tree, leaving
    /// the panel open. Mirrors the drawer-panel convention documented in
    /// CLAUDE.md.
    FocusTaskList,
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

pub struct LifecyclePanelView {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    paths: TodPaths,
    task_id: Option<String>,
    title: String,
    lifecycle: String,
    pending: Option<PendingGateCheck>,
    gate_status: String,
    gate_error: Option<String>,
    criteria_detail: Vec<CriterionOutcome>,
    focus_handle: FocusHandle,
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
                    if this.pending.is_some() {
                        this.poll_gate_check(cx);
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
            pending: None,
            gate_status: String::new(),
            gate_error: None,
            criteria_detail: Vec::new(),
            focus_handle: cx.focus_handle(),
            _poll_task,
        }
    }

    pub fn is_open(&self) -> bool {
        self.task_id.is_some()
    }

    fn load_task(&mut self, task_id: &str) -> bool {
        match self.fleet.get_task(task_id) {
            Ok(Some(task)) => {
                self.title = task.title;
                self.lifecycle = task.lifecycle;
                true
            }
            _ => false,
        }
    }

    pub fn open(&mut self, task_id: &str, cx: &mut Context<Self>) {
        self.task_id = Some(task_id.to_string());
        if !self.load_task(task_id) {
            self.task_id = None;
            return;
        }
        self.reset_gate_state();
        cx.notify();
    }

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
        self.reset_gate_state();
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.task_id.is_none() {
            return;
        }
        self.task_id = None;
        self.title.clear();
        self.lifecycle.clear();
        self.reset_gate_state();
        cx.emit(LifecyclePanelEvent::Close);
        cx.notify();
    }

    fn reset_gate_state(&mut self) {
        self.pending = None;
        self.gate_status.clear();
        self.gate_error = None;
        self.criteria_detail.clear();
    }

    fn in_flight(&self) -> bool {
        self.pending.is_some()
    }

    fn fail_gate_check(&mut self, message: String, cx: &mut Context<Self>) {
        self.pending = None;
        self.gate_error = Some(message);
        self.gate_status = "Gate check failed".into();
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

        self.gate_error = None;
        self.criteria_detail.clear();
        self.gate_status = "Preparing gate check…".into();
        self.pending = Some(PendingGateCheck {
            run_id: None,
            to_state: to_state.clone(),
        });
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
                        if let Some(pending) = self.pending.as_mut() {
                            pending.run_id = Some(handle.id);
                        }
                        self.gate_status = "Running gate check…".into();
                    }
                    Err(err) => self.fail_gate_check(format!("Launch agent failed: {err:#}"), cx),
                }
            }
            Err(err) => self.fail_gate_check(format!("{err:#}"), cx),
        }
        cx.notify();
    }

    fn poll_gate_check(&mut self, cx: &mut Context<Self>) {
        let Some(run_id) = self.pending.as_ref().and_then(|p| p.run_id) else {
            return;
        };
        let Ok(mut agent) = self.agent.try_lock() else {
            return;
        };
        let Some(state) = agent.poll_run(run_id) else {
            return;
        };
        drop(agent);

        let to_state = match self.pending.as_ref() {
            Some(p) => p.to_state.clone(),
            None => return,
        };

        match state {
            AgentRunState::InFlight(activity) => {
                self.gate_status = activity.unwrap_or_else(|| "Running gate check…".into());
            }
            AgentRunState::Success(response) => {
                self.pending = None;
                self.apply_gate_reply(response.unwrap_or_default(), &to_state, cx);
            }
            AgentRunState::Failure(message) => {
                self.pending = None;
                self.gate_error = Some(message);
                self.gate_status = "Gate check failed".into();
            }
        }
        cx.notify();
    }

    fn apply_gate_reply(&mut self, text: String, to_state: &str, _cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return;
        };

        let reply = match parse_gate_reply(&text) {
            Ok(reply) => reply,
            Err(err) => {
                self.gate_error = Some(format!("Could not parse agent reply: {err:#}"));
                self.gate_status = "Gate check reply was not understood".into();
                return;
            }
        };

        let results: Vec<(uuid::Uuid, String, Option<String>)> = reply
            .gate_results
            .iter()
            .map(|row| (row.criterion_id, row.outcome.clone(), row.detail.clone()))
            .collect();
        self.criteria_detail = reply
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
                self.gate_error = Some(format!("Failed to save gate check: {err:#}"));
                return;
            }
            let _ = self.fleet.writer().flush();
        }

        if let Some(new_state) = forward_state {
            self.lifecycle = new_state.clone();
            self.gate_status = format!("Advanced to {new_state}.");
        } else {
            self.gate_status = if reply.paused {
                "Gate check: blocked — see findings below.".into()
            } else {
                "Gate check did not advance the lifecycle.".into()
            };
        }
        if !reply.findings.trim().is_empty() {
            self.gate_error = None;
        }
        let _ = reply.findings; // surfaced via gate_status/criteria_detail for now
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

        let mut body = v_flex()
            .id("lifecycle-panel-body")
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_3()
            .overflow_y_scroll()
            .child(div().text_sm().font_semibold().child(self.title.clone()))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("Current: {}", self.lifecycle)),
            );

        body = match next_state {
            Some(next) => body.child(
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
            None => body.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("No further lifecycle state to advance to."),
            ),
        };

        if in_flight {
            body = body.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Spinner::new().with_size(Size::Small))
                    .child(div().text_xs().text_color(muted).child(self.gate_status.clone())),
            );
        } else if !self.gate_status.is_empty() {
            body = body.child(div().text_xs().text_color(muted).child(self.gate_status.clone()));
        }

        if let Some(error) = self.gate_error.clone() {
            body = body.child(div().text_xs().text_color(danger).child(error));
        }

        if !self.criteria_detail.is_empty() {
            let mut list = v_flex().gap_1().w_full();
            for row in &self.criteria_detail {
                let mut line = format!("{}: {}", row.label, row.outcome);
                if let Some(detail) = row.detail.as_deref() {
                    line.push_str(&format!(" — {detail}"));
                }
                list = list.child(div().text_xs().text_color(muted).child(line));
            }
            body = body.child(
                v_flex()
                    .gap_1()
                    .child(div().text_xs().font_semibold().child("Criteria"))
                    .child(list),
            );
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
            )
            .child(body)
            .into_any_element()
    }
}

pub fn register_lifecycle_panel_keyboard_bindings(cx: &mut App) {
    key_context::bind_panel_escape(cx, LifecyclePanelClose, LIFECYCLE_PANEL_CONTEXT);
    bind_modified_pane_nav(cx, LIFECYCLE_PANEL_CONTEXT);
}
