//! The task panel (`doc/ui/task-panel.md`): the default panel for a **task
//! node** (Lifecycle on itself, Agent on itself or inherited; see
//! `tod_store::fleet::node_actions::is_task_node`).
//!
//! Layout, top to bottom, ordered by permanence:
//!
//! 1. **Identity** — title and Linear ticket id (`render_identity`).
//! 2. **Artifacts** — Obligations and Plan links, each opening its panel by
//!    the column rule (`render_artifacts`). The Changes link (T8) joins this
//!    strip.
//! 3. **Runner line** (T2) — `render_runner_line`, a placeholder for now.
//! 4. **Requests** (T4) — the only part that scrolls: the shared
//!    [`crate::unified::requests::Requests`], oldest first, no heading.
//! 5. **Answered drawer** (T5) — anchored to the bottom;
//!    `render_answered_drawer`, a placeholder for now.
//!
//! The first three are fixed. Everything reloads on store change, as the
//! details panel does.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
};
use tod_store::fleet::FleetStore;
use tod_store::outline::repos::plan_steps::STATUS_VERIFIED;
use uuid::Uuid;

use crate::ui::selectable_text::selectable_text;
use crate::ui::style;

// T2: the runner line.
use gpui::{Entity, Subscription};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use tod_core::conversation::{ConversationStatus, SharedAgentAccess};
use tod_core::runner_status::{RunnerStatus, format_elapsed, format_tokens};
use tod_store::conversation::Focus;

use crate::ui::agent_runs::AgentRuns;
use crate::unified::columns::PanelKind;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};
use crate::unified::panels::changes::{ChangesWatch, HasChangesWatch};

// T4: the requests.
use gpui::AppContext as _;
use crate::unified::requests::{Requests, bind_request_actions};
use crate::views::lifecycle_control::LifecycleController;

/// Loaded, denormalized data for the task panel's header. Re-fetched
/// whenever the store changes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct TaskHeader {
    pub title: String,
    /// The external (Linear) ticket id, when the node carries one.
    pub ticket_id: Option<String>,
    pub obligation_count: usize,
    /// Obligations whose latest verdict is `failed`.
    pub obligations_failed: usize,
    pub plan_done: usize,
    pub plan_total: usize,
}

impl TaskHeader {
    pub(crate) fn obligations_label(&self) -> String {
        if self.obligations_failed > 0 {
            format!(
                "Obligations {} · {} failed",
                self.obligation_count, self.obligations_failed
            )
        } else {
            format!("Obligations {}", self.obligation_count)
        }
    }

    pub(crate) fn plan_label(&self) -> String {
        format!("Plan {}/{}", self.plan_done, self.plan_total)
    }
}

fn load(fleet: &FleetStore, node_id: Uuid) -> TaskHeader {
    let mut header = TaskHeader::default();
    if let Ok(Some(node)) = fleet.get_node(&node_id.to_string()) {
        header.title = node.title;
    }
    header.ticket_id = fleet
        .read(|conn| {
            Ok(tod_store::outline::repos::NodeRepo::new(conn).get_ticket_id(node_id)?)
        })
        .ok()
        .flatten()
        .filter(|id| !id.is_empty());
    if let Ok(obligations) = fleet.list_obligations_for_node(node_id) {
        header.obligation_count = obligations.len();
    }
    if let Ok(latest) = fleet.read(|conn| {
        Ok(tod_store::verification::VerdictRepo::new(conn).latest_for_node(node_id)?)
    }) {
        header.obligations_failed = latest.values().filter(|v| v.is_failed()).count();
    }
    if let Ok(steps) = fleet.list_plan_steps_for_node(node_id) {
        header.plan_total = steps.len();
        header.plan_done = steps.iter().filter(|s| s.status == STATUS_VERIFIED).count();
    }
    header
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// What the runner line knows beyond the header (T2). The store half
/// (`lifecycle`, `waiting_since`) is read off the UI thread on store change;
/// `run_since` is when this panel first saw the node's conversation running.
#[derive(Debug, Default, Clone)]
struct RunnerLine {
    lifecycle: String,
    waiting_since: Option<i64>,
    /// `(slot id, ms)`: the running slot and when it was first seen running.
    run_since: Option<(u64, i64)>,
    /// The status has an elapsed time, so the ticker re-renders each second.
    ticking: bool,
}

/// The node's lifecycle state and how long it has waited on the user.
fn load_runner(fleet: &FleetStore, node_id: Uuid) -> (String, Option<i64>) {
    let lifecycle = fleet
        .get_node(&node_id.to_string())
        .ok()
        .flatten()
        .map(|n| n.lifecycle)
        .unwrap_or_default();
    let waiting_since = fleet
        .read(|conn| tod_core::attention::for_node(conn, node_id))
        .ok()
        .and_then(|a| a.waiting_since);
    (lifecycle, waiting_since)
}

pub struct TaskPanel {
    node_id: Uuid,
    fleet: Arc<FleetStore>,
    focus_handle: FocusHandle,
    header: TaskHeader,
    pending_refresh: bool,
    /// T8: the files-changed count behind the Changes link.
    changes: ChangesWatch,
    _poll: gpui::Task<()>,
    agent_runs: Entity<AgentRuns>,
    runner: RunnerLine,
    _runner_tick: gpui::Task<()>,
    _agent_runs_sub: Subscription,
    /// T4: what the task is waiting on the user for.
    pub(crate) requests: Entity<Requests>,
    _requests_subs: Vec<Subscription>,
}

impl HasChangesWatch for TaskPanel {
    fn changes_watch(&mut self) -> &mut ChangesWatch {
        &mut self.changes
    }
}

impl TaskPanel {
    pub fn new(
        node_id: Uuid,
        fleet: Arc<FleetStore>,
        agent_runs: Entity<AgentRuns>,
        lifecycle: Entity<LifecycleController>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Store changes mark the header stale; it reloads on the next render
        // (the same event-driven refresh as `DetailsPanel`).
        let weak = cx.weak_entity();
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
                    let Ok(()) = weak.update(cx, |this: &mut TaskPanel, cx| {
                        this.pending_refresh = true;
                        this.refresh_runner(cx);
                        cx.notify();
                    }) else {
                        break;
                    };
                }
            }
        });
        let header = load(&fleet, node_id);
        // T2: the runner line follows `AgentRuns`' notifications, and ticks
        // its elapsed time once a second while it shows one.
        let _agent_runs_sub = cx.observe(&agent_runs, |this, _, cx| {
            this.track_run_since(cx);
            cx.notify();
        });
        let _runner_tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let Ok(()) = this.update(cx, |this: &mut TaskPanel, cx| {
                    if this.runner.ticking {
                        cx.notify();
                    }
                }) else {
                    break;
                };
            }
        });
        let changes = ChangesWatch::new(node_id, fleet.clone(), cx);
        let focus_handle = cx.focus_handle();
        let requests = {
            let (fleet, agent_runs, focus) = (fleet.clone(), agent_runs.clone(), focus_handle.clone());
            cx.new(|cx| Requests::new(Some(node_id), fleet, agent_runs, lifecycle, focus, window, cx))
        };
        let _requests_subs = vec![
            cx.subscribe(&requests, |_, _, event: &PanelOpenRequest, cx| cx.emit(event.clone())),
            cx.observe(&requests, |_, _, cx| cx.notify()),
        ];
        let mut panel = Self {
            node_id,
            fleet,
            focus_handle,
            requests,
            _requests_subs,
            header,
            pending_refresh: false,
            changes,
            _poll,
            agent_runs,
            runner: RunnerLine::default(),
            _runner_tick,
            _agent_runs_sub,
        };
        panel.track_run_since(cx);
        panel.refresh_runner(cx);
        ChangesWatch::recompute(&mut panel, node_id, cx);
        panel
    }

    #[cfg(test)]
    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    /// Retarget this column to a different task, in place.
    pub fn set_node(&mut self, node_id: Uuid, cx: &mut Context<Self>) {
        if node_id == self.node_id {
            return;
        }
        self.node_id = node_id;
        self.runner = RunnerLine::default();
        self.track_run_since(cx);
        self.refresh_runner(cx);
        ChangesWatch::recompute(self, node_id, cx);
        self.requests.update(cx, |requests, cx| requests.set_node(Some(node_id), cx));
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.header = load(&self.fleet, self.node_id);
        cx.notify();
    }

    fn open(&self, target: PanelKind, ctrl: bool, cx: &mut Context<Self>) {
        cx.emit(PanelOpenRequest { target, ctrl });
    }

    /// Title and ticket id: am I where I think I am?
    fn render_identity(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_start()
            .justify_between()
            .gap_2()
            .child(
                style::text_title(div().flex_1().min_w_0()).child(selectable_text(
                    "unified-task-title",
                    self.header.title.clone(),
                    window,
                    cx,
                )),
            )
            .when_some(self.header.ticket_id.clone(), |el, ticket| {
                el.child(style::text_muted(div().flex_none().child(selectable_text(
                    "unified-task-ticket",
                    ticket,
                    window,
                    cx,
                ))))
            })
            .into_any_element()
    }

    /// One link on the artifact strip: click opens `target` by the column
    /// rule, Ctrl+click as usual.
    fn artifact_link(
        &self,
        id: &'static str,
        label: String,
        target: PanelKind,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        style::text_link(div().id(id))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let ctrl = event.modifiers.control || event.modifiers.platform;
                    this.open(target, ctrl, cx);
                }),
            )
            .child(label)
            .into_any_element()
    }

    /// The task's artifacts: what is it made of, and how far along is it?
    fn render_artifacts(&self, cx: &mut Context<Self>) -> AnyElement {
        let node_id = self.node_id;
        let obligations = self.artifact_link(
            "unified-task-obligations",
            self.header.obligations_label(),
            PanelKind::Obligations(node_id),
            cx,
        );
        let plan = self.artifact_link(
            "unified-task-plan",
            self.header.plan_label(),
            PanelKind::Plan(node_id),
            cx,
        );
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .child(obligations)
            .child(plan)
            // Only a current count: none while unknown or recomputing.
            .children(self.changes.state.label().map(|label| {
                self.artifact_link("unified-task-changes", label, PanelKind::Changes(node_id), cx)
            }))
            .into_any_element()
    }

    /// Re-read the lifecycle state and attention off the UI thread.
    fn refresh_runner(&mut self, cx: &mut Context<Self>) {
        let fleet = self.fleet.clone();
        let node_id = self.node_id;
        cx.spawn(async move |this, cx| {
            let (lifecycle, waiting_since) = cx
                .background_executor()
                .spawn(async move { load_runner(&fleet, node_id) })
                .await;
            let _ = this.update(cx, |this: &mut TaskPanel, cx| {
                if this.node_id == node_id {
                    this.runner.lifecycle = lifecycle;
                    this.runner.waiting_since = waiting_since;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The slot on this node whose status matters most: a running one, else
    /// one whose last turn failed.
    fn node_slot(&self, cx: &App) -> Option<(u64, ConversationStatus)> {
        let runs = self.agent_runs.read(cx);
        let focus = Focus::Node(self.node_id);
        let mut failed = None;
        let mut ix = 0;
        while let Some(slot) = runs.slot_by_index(ix) {
            ix += 1;
            if slot.focus != focus {
                continue;
            }
            if slot.status.running {
                return Some((slot.id, slot.status.clone()));
            }
            if failed.is_none() && slot.status.last_error.is_some() {
                failed = Some((slot.id, slot.status.clone()));
            }
        }
        failed
    }

    /// Note when this node's conversation started running, for the elapsed
    /// time: `ConversationStatus` carries no start time of its own.
    fn track_run_since(&mut self, cx: &mut Context<Self>) {
        let running = self.node_slot(cx).filter(|(_, s)| s.running).map(|(id, _)| id);
        self.runner.run_since = match (running, self.runner.run_since) {
            (Some(id), Some((seen, at))) if seen == id => Some((seen, at)),
            (Some(id), _) => Some((id, now_ms())),
            (None, _) => None,
        };
    }

    fn runner_status(&self, cx: &App) -> (Option<u64>, RunnerStatus) {
        let slot = self.node_slot(cx);
        let status = RunnerStatus::derive(
            &self.runner.lifecycle,
            self.runner.waiting_since,
            slot.as_ref().map(|(_, s)| s),
            self.runner.run_since.map(|(_, at)| at),
        );
        (slot.map(|(id, _)| id), status)
    }

    /// Stop the node's running turn: the conversation view's own stop path.
    fn stop_runner(&mut self, slot: u64, cx: &mut Context<Self>) {
        self.agent_runs.update(cx, |runs, cx| {
            let fleet = runs.fleet().clone();
            let agent = runs.agent().clone();
            match runs.driver_mut(slot) {
                Some(driver) => {
                    if let Err(err) = driver.cancel(&fleet, &mut SharedAgentAccess(&agent)) {
                        tracing::warn!("task panel: stopping the turn failed: {err:#}");
                    }
                    let status = driver.status();
                    runs.set_status(slot, status);
                }
                None => runs.set_cancel(slot),
            }
            cx.notify();
        });
    }

    /// T2: the runner line — lifecycle state, then what the runner is doing
    /// (`doc/ui/task-panel.md`, "Runner"), with Stop while an agent runs.
    fn render_runner_line(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (slot, status) = self.runner_status(cx);
        self.runner.ticking = status.since().is_some();
        let elapsed = status.since().map(|since| format_elapsed(now_ms() - since));
        let sep = || style::text_muted(div().flex_none()).child("·");

        let mut line = div().flex().items_center().gap_2().min_w_0();
        if status != RunnerStatus::Done {
            line = line.child(style::text(div().flex_none()).child(self.runner.lifecycle.clone()));
        }
        match &status {
            RunnerStatus::Running { activity, tokens, .. } => {
                if let Some(activity) = activity {
                    line = line.child(sep()).child(style::text_muted(div().flex_1().min_w_0()).child(
                        selectable_text("unified-task-runner-activity", activity.clone(), window, cx),
                    ));
                } else {
                    line = line.child(sep()).child(style::text_muted(div().flex_none()).child("running"));
                }
                if let Some(elapsed) = elapsed {
                    line = line.child(sep()).child(style::text_muted(div().flex_none()).child(elapsed));
                }
                if let Some(tokens) = tokens {
                    line = line
                        .child(sep())
                        .child(style::text_muted(div().flex_none()).child(format_tokens(*tokens)));
                }
            }
            RunnerStatus::Waiting { .. } => {
                line = line.child(sep()).child(
                    style::text_muted(div().flex_none())
                        .child(format!("waiting {}", elapsed.unwrap_or_default())),
                );
            }
            RunnerStatus::Failed { error } => {
                line = line.child(sep()).child(style::text_error(div().flex_1().min_w_0()).child(
                    selectable_text("unified-task-runner-error", error.clone(), window, cx),
                ));
            }
            RunnerStatus::Idle => {}
            RunnerStatus::Done => {
                line = line.child(style::text_muted(div().flex_none()).child("done"));
            }
        }
        if status.is_running()
            && let Some(slot) = slot
        {
            line = line.child(div().flex_1()).child(
                Button::new("unified-task-runner-stop")
                    .label("Stop")
                    .ghost()
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| this.stop_runner(slot, cx))),
            );
        }
        Some(line.id("unified-task-runner").into_any_element())
    }

    /// T4: the requests waiting on the user, oldest first — the only part
    /// that scrolls. Rendering, answering, and keys are the shared
    /// [`Requests`]'s.
    fn render_requests(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        Some(div().px_3().pb_3().child(self.requests.clone()).into_any_element())
    }

    /// T5: the Answered drawer, anchored to the bottom of the column.
    /// Not implemented yet.
    fn render_answered_drawer(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        None
    }
}

impl ColumnPanel for TaskPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "Task".into()
    }

    fn target_label(&self, _cx: &App) -> SharedString {
        self.header.title.clone().into()
    }
}

impl EventEmitter<PanelOpenRequest> for TaskPanel {}

impl Focusable for TaskPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TaskPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_refresh {
            self.pending_refresh = false;
            self.header = load(&self.fleet, self.node_id);
        }
        let identity = self.render_identity(window, cx);
        let artifacts = self.render_artifacts(cx);
        let runner = self.render_runner_line(window, cx);
        let requests = self.render_requests(cx);
        let drawer = self.render_answered_drawer(cx);

        bind_request_actions(div().id("unified-task-panel"), &self.requests)
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            // Fixed header: identity, artifacts, runner.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .flex_none()
                    .child(identity)
                    .child(artifacts)
                    .children(runner),
            )
            // Requests scroll; the drawer stays at the bottom.
            .child(
                div()
                    .id("unified-task-requests")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(requests),
            )
            .children(drawer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_show_failed_only_when_any() {
        let mut header = TaskHeader {
            obligation_count: 7,
            plan_done: 5,
            plan_total: 6,
            ..Default::default()
        };
        assert_eq!(header.obligations_label(), "Obligations 7");
        assert_eq!(header.plan_label(), "Plan 5/6");
        header.obligations_failed = 1;
        assert_eq!(header.obligations_label(), "Obligations 7 · 1 failed");
    }

    /// T2: a run starting in `AgentRuns` shows on the runner line as
    /// running, with the slot's activity and a start time, and Stop reaches its slot.
    #[gpui::test]
    fn runner_line_follows_agent_runs(cx: &mut gpui::TestAppContext) {
        use crate::views::rows::fixture::Fixture;
        use gpui::AppContext as _;
        use std::cell::RefCell;
        use std::rc::Rc;
        use tod_store::conversation::ProtocolKind;

        let fixture = Fixture::new();
        cx.update(gpui_component::init);
        let agent: crate::interview::agent::SharedAgent =
            Arc::new(std::sync::Mutex::new(Box::new(tod_agent::MockAgentProvider::new())));
        let agent_runs = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));
        let (node, fleet, runs_in) = (fixture.node_id, fixture.store.clone(), agent_runs.clone());
        let lifecycle = cx.new(|_| LifecycleController::new(fixture.store.clone()));
        let slot = Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| TaskPanel::new(node, fleet, runs_in, lifecycle, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        cx.run_until_parked();
        let status = view.read_with(cx, |view, cx| view.runner_status(cx).1);
        assert!(matches!(status, RunnerStatus::Idle | RunnerStatus::Waiting { .. }), "{status:?}");

        let focus = Focus::Node(node);
        let slot_id = agent_runs.update(cx, |registry, cx| {
            let config = tod_core::conversation::ConversationConfig {
                data_root: fixture.store.paths().root().to_path_buf(),
                media: tod_core::media::MediaPaths::discover().expect("media paths"),
                launch: tod_agent::AgentLaunchOptions::for_platform(tod_agent::AgentPlatform::Claude),
                context: Default::default(),
            };
            let driver = tod_core::conversation::ConversationDriver::new(
                config,
                focus,
                ProtocolKind::Implementation,
            );
            let ix = registry
                .ensure(focus, ProtocolKind::Implementation, None, || Ok(driver))
                .unwrap();
            let (id, driver) = registry.take_to_send(ix).unwrap();
            drop(driver);
            cx.notify();
            id
        });
        cx.run_until_parked();
        let (seen, status) = view.read_with(cx, |view, cx| view.runner_status(cx));
        assert_eq!(seen, Some(slot_id));
        match status {
            RunnerStatus::Running { activity, since, .. } => {
                assert!(activity.is_some());
                assert!(since.is_some());
            }
            other => panic!("expected running, got {other:?}"),
        }

        // Stop while the driver is away marks the slot to cancel.
        view.update(cx, |view, cx| view.stop_runner(slot_id, cx));
        assert!(agent_runs.update(cx, |registry, _| registry.take_cancel(slot_id)));
    }

    /// T4: a pending decision shows among the task panel's requests, and
    /// the number key answers it through the shared module.
    #[gpui::test]
    fn a_pending_decision_renders_and_can_be_answered(cx: &mut gpui::TestAppContext) {
        use crate::views::rows::fixture::Fixture;
        use std::cell::RefCell;
        use std::rc::Rc;
        use tod_store::decisions::DecisionRepo;
        use tod_store::interview::{ACTOR_USER, InterviewCommand};

        let fixture = Fixture::new();
        let decision_id = fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AskDecision {
                    node_id: fixture.node_id,
                    conversation_id: None,
                    protocol: None,
                    decision: tod_store::decisions::NewDecision {
                        question: "Round per line or per invoice?".to_string(),
                        options: vec!["per line".to_string(), "per invoice".to_string()],
                        evidence: Vec::new(),
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
            let view = cx.new(|cx| TaskPanel::new(node, fleet, agent_runs, lifecycle, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let view: Entity<TaskPanel> = slot.borrow_mut().take().unwrap();
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let requests = view.read_with(cx, |view, _| view.requests.clone());
        requests.read_with(cx, |requests, _| {
            assert_eq!(requests.items().len(), 1);
            assert_eq!(requests.items()[0].id, decision_id);
        });

        // Answer with the panel gone: T8's `ChangesWatch` awaits store
        // changes directly, which the test scheduler rejects as a wake from
        // the store's own thread. The answer path is the shared module's.
        drop(view);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
        let decision = requests.read_with(cx, |requests, _| requests.loaded.pending[0].clone());
        requests.update(cx, |requests, cx| requests.click_option(decision, 2, cx));
        cx.run_until_parked();

        let answers = fixture
            .store
            .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))
            .unwrap()
            .unwrap()
            .answers;
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].option, Some(2));
        requests.read_with(cx, |requests, _| assert!(requests.items().is_empty()));
    }
}
