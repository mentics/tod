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
//! 3. **Runner line** (T2) — `render_runner_line`: what the task's runner
//!    (`crate::unified::runners`, or its supervisor in the cloud) is doing,
//!    and a split button whose default action moves it along (Start,
//!    Pause, Resume) with the rest in its menu.
//! 4. **Requests** (T4) — the only part that scrolls: the shared
//!    [`crate::unified::requests::Requests`], oldest first, no heading.
//! 5. **Answered drawer** (T5) — anchored to the bottom, collapsed by
//!    default; the shared [`crate::unified::requests::Requests`] answer log.
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
use gpui::{ElementId, Entity, Subscription};
use gpui_component::Sizable as _;
use gpui_component::Disableable as _;
use gpui_component::button::Button;
use tod_core::conversation::{ConversationStatus, SharedAgentAccess};
use tod_core::runner_status::{RunnerStatus, format_elapsed, format_tokens};
use tod_core::autopilot::Outcome;
use tod_core::cloud_sync::CloudNode;
use gpui_component::button::DropdownButton;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use tod_journey::{Presented, PresentedAction};
use crate::unified::runners::NodeRunners;
use crate::views::cloud_node::CloudUpdate;
use tod_store::conversation::{Focus, ProtocolKind};

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
    /// The Linear ticket's browser URL, when both the id and the workspace
    /// slug are known.
    pub ticket_url: Option<String>,
    /// The pull requests linked to the node (its Ticket capability's links,
    /// where `tod-cli pr open` adds the one it opens).
    pub pull_requests: Vec<tod_store::github::NodePr>,
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
    if let Some(ticket) = header.ticket_id.as_deref() {
        let metadata = fleet
            .get_extra_content(node_id, tod_store::outline::types::EXTRA_CONTENT_METADATA)
            .ok()
            .flatten()
            .and_then(|json_str| serde_json::from_str::<serde_json::Value>(&json_str).ok());
        header.ticket_url =
            tod_integration::linear_issue_url(metadata.as_ref(), fleet.paths().root(), ticket);
    }
    header.pull_requests = fleet
        .read(|conn| Ok(tod_store::github::NodePrRepo::new(conn).read(node_id)?))
        .map(|links| links.prs)
        .unwrap_or_default();
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
    /// Set when the node runs in the cloud: its supervisor is its runner.
    cloud: Option<CloudNode>,
    /// A cloud job (Run in the cloud, Sync now, leaving) is in progress.
    cloud_busy: bool,
    /// The latest word from a cloud job; an error when `cloud_failed`.
    cloud_note: Option<String>,
    cloud_failed: bool,
}

/// What the runner line's split button offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunnerAction {
    Start,
    Resume,
    ResumeFreshBudget,
    Pause,
    StopNow,
    /// Stop a conversation the user started by hand.
    StopTurn,
    RunInCloud,
    SyncCloud,
    LeaveCloud,
}

impl RunnerAction {
    fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Resume => "Resume",
            Self::ResumeFreshBudget => "Resume with a fresh budget",
            Self::Pause => "Pause",
            Self::StopNow => "Stop now",
            Self::StopTurn => "Stop",
            Self::RunInCloud => "Run in the cloud",
            Self::SyncCloud => "Sync now",
            Self::LeaveCloud => "Stop running in the cloud",
        }
    }
}

/// The split button: its default action (with its label and whether it is
/// disabled) and the menu's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RunnerActions {
    primary: Option<(RunnerAction, &'static str, bool)>,
    menu: Vec<RunnerAction>,
}

/// The node's lifecycle state, how long it has waited on the user, and
/// whether it runs in the cloud.
fn load_runner(fleet: &FleetStore, node_id: Uuid) -> (String, Option<i64>, Option<CloudNode>) {
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
    let cloud = tod_core::cloud_sync::cloud_node(fleet, &node_id.to_string());
    (lifecycle, waiting_since, cloud)
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
    runners: Entity<NodeRunners>,
    _runner_tick: gpui::Task<()>,
    _agent_runs_sub: Subscription,
    _runners_sub: Subscription,
    /// T4: what the task is waiting on the user for.
    pub(crate) requests: Entity<Requests>,
    _requests_subs: Vec<Subscription>,
    /// T5: whether the Answered drawer is open; kept for the session only.
    pub(crate) answered_open: bool,
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
        runners: Entity<NodeRunners>,
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
        let _runners_sub = cx.observe(&runners, |_, _, cx| cx.notify());
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
            answered_open: false,
            header,
            pending_refresh: false,
            changes,
            _poll,
            agent_runs,
            runner: RunnerLine::default(),
            runners,
            _runner_tick,
            _agent_runs_sub,
            _runners_sub,
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
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .when_some(self.header.ticket_id.clone(), |el, ticket| {
                        el.child(match self.header.ticket_url.clone() {
                            Some(url) => self.external_link("unified-task-ticket", ticket, url, cx),
                            None => style::text_muted(div()).child(ticket).into_any_element(),
                        })
                    })
                    .children(self.header.pull_requests.iter().enumerate().map(|(ix, pr)| {
                        let label = format!("{}#{}", pr.repo, pr.pr_number);
                        self.external_link(("unified-task-pr", ix), label, pr.url.clone(), cx)
                    })),
            )
            .into_any_element()
    }

    /// A label that opens `url` in the browser (`styles.text-link-external`).
    fn external_link(
        &self,
        id: impl Into<ElementId>,
        label: String,
        url: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        style::text_link_external(div().id(id))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_this, _event: &MouseDownEvent, _, cx| {
                    cx.open_url(&url);
                }),
            )
            .child(label)
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
            // Placeholder until there is a journey viewer to open
            // (`doc/ui/task-panel-plan.md`, "Left for later"); then it
            // becomes an `artifact_link` like the others.
            .child(style::text_muted(div().id("unified-task-journey").ml_auto()).child("Journey (not built yet)"))
            .into_any_element()
    }

    /// Re-read the lifecycle state and attention off the UI thread.
    fn refresh_runner(&mut self, cx: &mut Context<Self>) {
        let fleet = self.fleet.clone();
        let node_id = self.node_id;
        cx.spawn(async move |this, cx| {
            let (lifecycle, waiting_since, cloud) = cx
                .background_executor()
                .spawn(async move { load_runner(&fleet, node_id) })
                .await;
            let _ = this.update(cx, |this: &mut TaskPanel, cx| {
                if this.node_id == node_id {
                    this.runner.lifecycle = lifecycle;
                    this.runner.waiting_since = waiting_since;
                    this.runner.cloud = cloud;
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
            // Chat-panel conversations are not the lifecycle processor's:
            // their status never shows here.
            if slot.focus != focus
                || matches!(slot.protocol, ProtocolKind::Outline | ProtocolKind::Chat)
            {
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

    /// The lifecycle conversation to watch: the one running (or failed) now,
    /// else the most recently updated one. Chat-panel conversations are not
    /// the lifecycle processor's.
    fn watch_target(&self, cx: &App) -> Option<uuid::Uuid> {
        let runs = self.agent_runs.read(cx);
        if let Some((id, _)) = self.node_slot(cx) {
            if let Some(conversation) = runs.slot_by_id(id).and_then(|s| s.conversation_id) {
                return Some(conversation);
            }
        }
        let node = self.node_id;
        self.fleet
            .read(|conn| {
                tod_store::conversation::ConversationRepo::new(conn).list_for_focus(Focus::Node(node))
            })
            .ok()?
            .into_iter()
            .map(|s| s.conversation)
            .filter(|c| !matches!(c.protocol, ProtocolKind::Outline | ProtocolKind::Chat))
            .max_by_key(|c| c.updated_at)
            .map(|c| c.id)
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
        let status = RunnerStatus::with_runner(
            self.runners.read(cx).runner(self.node_id),
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

    /// What the split button offers for `status`.
    fn runner_actions(&self, status: &RunnerStatus, cx: &App) -> RunnerActions {
        use RunnerAction as A;
        let runners = self.runners.read(cx);
        let node = self.node_id;
        if self.runner.cloud.is_some() {
            return RunnerActions {
                primary: Some((A::SyncCloud, A::SyncCloud.label(), self.runner.cloud_busy)),
                menu: if self.runner.cloud_busy { Vec::new() } else { vec![A::LeaveCloud] },
            };
        }
        if self.runner.cloud_busy {
            return RunnerActions {
                primary: Some((A::RunInCloud, "Starting in the cloud…", true)),
                menu: Vec::new(),
            };
        }
        if runners.is_running(node) {
            let pausing = runners.is_pausing(node);
            return RunnerActions {
                primary: Some((A::Pause, if pausing { "Pausing…" } else { A::Pause.label() }, pausing)),
                menu: vec![A::StopNow],
            };
        }
        if status.is_running() {
            return RunnerActions {
                primary: Some((A::StopTurn, A::StopTurn.label(), false)),
                menu: Vec::new(),
            };
        }
        if *status == RunnerStatus::Done {
            return RunnerActions::default();
        }
        match runners.outcome(node) {
            Some(Outcome::BudgetExhausted { .. }) => RunnerActions {
                primary: Some((A::ResumeFreshBudget, A::ResumeFreshBudget.label(), false)),
                menu: vec![A::RunInCloud],
            },
            Some(outcome) if *outcome != Outcome::Done => RunnerActions {
                primary: Some((A::Resume, A::Resume.label(), false)),
                menu: vec![A::ResumeFreshBudget, A::RunInCloud],
            },
            _ => RunnerActions {
                primary: Some((A::Start, A::Start.label(), false)),
                menu: vec![A::RunInCloud],
            },
        }
    }

    /// The journey's snapshot of the split button and the line beside it.
    fn presented(&self, actions: &RunnerActions, status: &RunnerStatus) -> Presented {
        let mut presented: Vec<PresentedAction> = actions
            .primary
            .iter()
            .map(|(action, label, disabled)| PresentedAction {
                id: format!("{action:?}"),
                label: label.to_string(),
                primary: true,
                disabled: *disabled,
            })
            .collect();
        presented.extend(actions.menu.iter().map(|action| PresentedAction {
            id: format!("{action:?}"),
            label: action.label().to_string(),
            primary: false,
            disabled: false,
        }));
        Presented {
            actions: presented,
            focused: None,
            notices: vec![format!("{} · {status:?}", self.runner.lifecycle)],
        }
    }

    /// The split button's click handler, its default action's and its
    /// menu's: records the `UserAction`, then does it.
    fn perform(&mut self, action: RunnerAction, cx: &mut Context<Self>) {
        use RunnerAction as A;
        let (slot, status) = self.runner_status(cx);
        let actions = self.runner_actions(&status, cx);
        let presented = self.presented(&actions, &status);
        let node = self.node_id;
        crate::ui::journey::record_action(
            cx,
            Focus::Node(node),
            format!("{action:?}"),
            crate::ui::journey::Source::Click,
            "task_panel",
            presented,
        );
        match action {
            A::Start | A::Resume => {
                let _ = self.runners.update(cx, |r, cx| r.start(node, false, cx));
            }
            A::ResumeFreshBudget => {
                let _ = self.runners.update(cx, |r, cx| r.start(node, true, cx));
            }
            A::Pause => self.runners.update(cx, |r, cx| r.pause(node, cx)),
            A::StopNow => self.runners.update(cx, |r, cx| r.stop_now(node, cx)),
            A::StopTurn => {
                if let Some(slot) = slot {
                    self.stop_runner(slot, cx);
                }
            }
            A::RunInCloud => {
                self.cloud_started(cx);
                crate::views::cloud_node::run_in_cloud(
                    self.fleet.clone(),
                    node.to_string(),
                    cx,
                    move |this, update, cx| this.on_cloud_update(node, update, cx),
                );
            }
            A::SyncCloud => {
                self.cloud_started(cx);
                crate::views::cloud_node::sync_now(self.fleet.clone(), cx, move |this, update, cx| {
                    this.on_cloud_update(node, update, cx)
                });
            }
            A::LeaveCloud => {
                self.cloud_started(cx);
                crate::views::cloud_node::stop_running(
                    self.fleet.clone(),
                    node.to_string(),
                    false,
                    cx,
                    move |this, update, cx| this.on_cloud_update(node, update, cx),
                );
            }
        }
        cx.notify();
    }

    /// The phase chip: an exceptional, deliberate override, so the menu
    /// lists every other phase and the pick is applied at once, bypassing
    /// the gates. Off the UI thread; the store change refreshes the line.
    fn set_phase(&mut self, target: &'static str, cx: &mut Context<Self>) {
        let node = self.node_id;
        let presented = Presented {
            actions: Vec::new(),
            focused: None,
            notices: vec![format!("{} → {target}", self.runner.lifecycle)],
        };
        crate::ui::journey::record_action(
            cx,
            Focus::Node(node),
            format!("SetPhase({target})"),
            crate::ui::journey::Source::Click,
            "task_panel",
            presented,
        );
        self.runner.lifecycle = target.to_string();
        let fleet = self.fleet.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { tod_core::lifecycle::set_lifecycle(&fleet, node, target) })
                .await;
            let _ = this.update(cx, |this: &mut TaskPanel, cx| {
                if let Err(err) = result {
                    this.runner.cloud_note = Some(format!("Could not change the phase: {err:#}"));
                    this.runner.cloud_failed = true;
                }
                this.refresh_runner(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The lifecycle state as a chip; clicking it lists the other phases.
    fn render_phase_chip(&self, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.weak_entity();
        let current = self.runner.lifecycle.clone();
        let menu_current = current.clone();
        Button::new("unified-task-phase")
            .label(current)
            .outline()
            .xsmall()
            .rounded(gpui::px(999.))
            .dropdown_caret(true)
            .dropdown_menu(move |mut popup, _, _| {
                for state in tod_core::task::model::LIFECYCLE_STATES {
                    if state == menu_current {
                        continue;
                    }
                    let weak = weak.clone();
                    popup = popup.item(PopupMenuItem::new(state).on_click(move |_, _, cx| {
                        let _ = weak.update(cx, |this: &mut TaskPanel, cx| this.set_phase(state, cx));
                    }));
                }
                popup
            })
            .into_any_element()
    }

    fn cloud_started(&mut self, cx: &mut Context<Self>) {
        self.runner.cloud_busy = true;
        self.runner.cloud_note = None;
        self.runner.cloud_failed = false;
        cx.notify();
    }

    fn on_cloud_update(&mut self, node: Uuid, update: CloudUpdate, cx: &mut Context<Self>) {
        if self.node_id != node {
            return;
        }
        let (note, failed, done) = match update {
            CloudUpdate::Progress(msg) => (msg, false, false),
            CloudUpdate::Accepted(cloud) => {
                self.runner.cloud = Some(cloud);
                (String::new(), false, true)
            }
            CloudUpdate::Synced(msg) => (msg, false, true),
            CloudUpdate::Left(msg) => {
                self.runner.cloud = None;
                (msg, false, true)
            }
            CloudUpdate::Failed(msg) => (msg, true, true),
        };
        self.runner.cloud_note = (!note.is_empty()).then_some(note);
        self.runner.cloud_failed = failed;
        if done {
            self.runner.cloud_busy = false;
            self.refresh_runner(cx);
        }
        cx.notify();
    }

    /// The split button: the default action, with the rest in its menu.
    fn render_runner_button(&self, actions: RunnerActions, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (primary, label, disabled) = actions.primary?;
        let button = Button::new("unified-task-runner-action")
            .label(label)
            .disabled(disabled)
            .on_click(cx.listener(move |this, _, _, cx| this.perform(primary, cx)));
        let mut split = DropdownButton::new("unified-task-runner").small().button(button);
        if !actions.menu.is_empty() {
            let weak = cx.weak_entity();
            let menu = actions.menu;
            split = split.dropdown_menu(move |mut popup, _, _| {
                for action in menu.iter().copied() {
                    let weak = weak.clone();
                    popup = popup.item(PopupMenuItem::new(action.label()).on_click(move |_, _, cx| {
                        let _ = weak.update(cx, |this: &mut TaskPanel, cx| this.perform(action, cx));
                    }));
                }
                popup
            });
        }
        Some(split.into_any_element())
    }

    /// T2: the runner line — lifecycle state, then what the runner is doing
    /// (`doc/ui/task-panel.md`, "Runner"), then the split button.
    fn render_runner_line(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (_, status) = self.runner_status(cx);
        self.runner.ticking = status.since().is_some();
        let elapsed = status.since().map(|since| format_elapsed(now_ms() - since));
        let sep = || style::text_muted(div().flex_none()).child("·");

        let mut line = div().flex().items_center().gap_2().min_w_0();
        if status != RunnerStatus::Done {
            line = line.child(self.render_phase_chip(cx));
        }
        if let Some(cloud) = &self.runner.cloud {
            let mut text = format!("in the cloud (sandbox {})", cloud.sandbox);
            if cloud.lost_at.is_some() {
                text.push_str(", its sandbox is gone; replaced on the next sync");
            }
            line = line.child(sep()).child(style::text_muted(div().flex_1().min_w_0()).child(
                selectable_text("unified-task-runner-cloud", text, window, cx),
            ));
        } else {
            match &status {
                RunnerStatus::Running { activity, tokens, .. } => {
                    if let Some(activity) = activity {
                        // Shrinks, never grows: the elapsed time and tokens
                        // stay beside it.
                        line = line.child(sep()).child(style::text_muted(div().flex_shrink_1().min_w_0()).child(
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
                RunnerStatus::Paused => {
                    line = line.child(sep()).child(style::text_muted(div().flex_none()).child("paused"));
                }
                RunnerStatus::Stopped { reason } => {
                    line = line.child(sep()).child(style::text_muted(div().flex_1().min_w_0()).child(
                        selectable_text("unified-task-runner-stopped", format!("stopped: {reason}"), window, cx),
                    ));
                }
                RunnerStatus::Idle => {}
                RunnerStatus::Done => {
                    line = line.child(style::text_muted(div().flex_none()).child("done"));
                }
            }
        }
        // Why the runner could not start, or the latest word from the cloud.
        let note = match (&self.runner.cloud_note, self.runners.read(cx).error(self.node_id)) {
            (Some(note), _) => Some((note.clone(), self.runner.cloud_failed)),
            (None, Some(error)) => Some((error.to_string(), true)),
            (None, None) => None,
        };
        if let Some((note, failed)) = note {
            let el = div().flex_1().min_w_0();
            let el = if failed { style::text_error(el) } else { style::text_muted(el) };
            line = line.child(sep()).child(el.child(selectable_text("unified-task-runner-note", note, window, cx)));
        }
        let actions = self.runner_actions(&status, cx);
        let node = self.node_id;
        let watch = self.watch_target(cx).map(|_| {
            Button::new("unified-task-runner-watch")
                .label("Watch")
                .small()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open(PanelKind::Watch(node), false, cx)
                }))
        });
        let button = self.render_runner_button(actions, cx);
        if watch.is_some() || button.is_some() {
            line = line.child(div().flex_1());
        }
        if let Some(watch) = watch {
            line = line.child(watch);
        }
        if let Some(button) = button {
            line = line.child(button);
        }
        Some(line.id("unified-task-runner-line").into_any_element())
    }

    /// T4: the requests waiting on the user, oldest first — the only part
    /// that scrolls. Rendering, answering, and keys are the shared
    /// [`Requests`]'s.
    fn render_requests(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        Some(div().px_3().pb_3().child(self.requests.clone()).into_any_element())
    }

    /// T5: the Answered drawer, anchored to the bottom of the column:
    /// collapsed to "Answered (n)", opened it lists the node's decision
    /// answers newest first, each with **Change** and the asking
    /// conversation — the shared [`Requests`] log. Click-only: the header
    /// is not one of the requests' keyboard stops.
    fn render_answered_drawer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        use gpui_component::ActiveTheme as _;
        let count = self.requests.read(cx).log().len();
        let open = self.answered_open;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let entries = open.then(|| {
            self.requests
                .update(cx, |requests, cx| requests.render_log_entries(window, cx))
        });
        Some(
            div()
                .id("unified-task-answered")
                .flex_none()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(border)
                .child(
                    div()
                        .id("unified-task-answered-header")
                        .px_3()
                        .py_2()
                        .text_sm()
                        .cursor_pointer()
                        .child(format!("{} Answered ({count})", if open { "▾" } else { "▸" }))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_answered(cx))),
                )
                .when_some(entries, |el, entries| {
                    el.child(
                        div()
                            .id("unified-task-answered-list")
                            .max_h(gpui::px(320.))
                            .overflow_y_scroll()
                            .px_3()
                            .child(entries)
                            .when(count == 0, |el| {
                                el.child(div().text_xs().text_color(muted).pb_2().child("No answers yet."))
                            }),
                    )
                })
                .into_any_element(),
        )
    }

    /// Open or close the Answered drawer.
    pub(crate) fn toggle_answered(&mut self, cx: &mut Context<Self>) {
        self.answered_open = !self.answered_open;
        cx.notify();
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
        let drawer = self.render_answered_drawer(window, cx);

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
fn test_runners(
    fleet: &Arc<FleetStore>,
    agent_runs: &Entity<AgentRuns>,
    cx: &mut gpui::TestAppContext,
) -> Entity<NodeRunners> {
    let agent: crate::interview::agent::SharedAgent =
        Arc::new(std::sync::Mutex::new(Box::new(tod_agent::MockAgentProvider::new())));
    let (fleet, agent_runs) = (fleet.clone(), agent_runs.clone());
    cx.new(|cx| NodeRunners::new(fleet, agent, agent_runs, cx))
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
        let runners = test_runners(&fleet, &agent_runs, cx);
        let slot = Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| TaskPanel::new(node, fleet, runs_in, lifecycle, runners, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        cx.run_until_parked();
        let status = view.read_with(cx, |view, cx| view.runner_status(cx).1);
        assert!(matches!(status, RunnerStatus::Idle | RunnerStatus::Waiting { .. }), "{status:?}");
        // No runner yet: Start, with the cloud in the menu.
        let actions = view.read_with(cx, |view, cx| view.runner_actions(&status, cx));
        assert_eq!(
            actions,
            RunnerActions {
                primary: Some((RunnerAction::Start, "Start", false)),
                menu: vec![RunnerAction::RunInCloud],
            }
        );

        let focus = Focus::Node(node);
        let slot_id = agent_runs.update(cx, |registry, cx| {
            let config = tod_core::conversation::ConversationConfig {
                data_root: fixture.store.paths().root().to_path_buf(),
                media: tod_core::media::MediaPaths::discover().expect("media paths"),
                launch: tod_agent::AgentLaunchOptions::for_platform(tod_agent::AgentPlatform::Claude),
                settings_path: None,
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
        // A conversation run by hand: the button stops it, no runner to start.
        let actions = view.read_with(cx, |view, cx| {
            let (_, status) = view.runner_status(cx);
            view.runner_actions(&status, cx)
        });
        assert_eq!(actions.primary, Some((RunnerAction::StopTurn, "Stop", false)));

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
        let runners = test_runners(&fleet, &agent_runs, cx);
        let slot = Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| TaskPanel::new(node, fleet, agent_runs, lifecycle, runners, window, cx));
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

        // Answer with the panel open.
        let decision = requests.read_with(cx, |requests, _| requests.loaded.pending[0].clone());
        requests.update(cx, |requests, cx| requests.click_option(decision, 2, cx));
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let answers = fixture
            .store
            .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))
            .unwrap()
            .unwrap()
            .answers;
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].option, Some(2));
        requests.read_with(cx, |requests, _| assert!(requests.items().is_empty()));

        // T5: the drawer starts closed and counts the answer; opened, Change
        // on its entry records a second answer beside the first.
        view.read_with(cx, |view, _| assert!(!view.answered_open));
        view.update(cx, |view, cx| view.toggle_answered(cx));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        requests.update(cx, |requests, cx| {
            assert_eq!(requests.log().len(), 1);
            let decision = requests.log()[0].decision.clone();
            requests.start_change(decision_id, cx);
            requests.click_option(decision, 1, cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let answers = fixture
            .store
            .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))
            .unwrap()
            .unwrap()
            .answers;
        assert_eq!(answers.len(), 2, "the first answer is never overwritten");
        assert_eq!(answers[0].option, Some(2));
        assert_eq!(answers[1].option, Some(1));
        requests.read_with(cx, |requests, _| assert_eq!(requests.log().len(), 2));
        view.read_with(cx, |view, _| assert!(view.answered_open));
    }
}

/// Request tests, hosted by the task panel.
#[cfg(test)]
mod request_tests {
    use super::*;
    use crate::ui::journey::Source;
    use crate::unified::requests::DecisionOptionKey;
    use tod_core::attention::AttentionKind;
    use tod_core::conversation::implement::HandoffAnswer;
    use tod_store::decisions::DecisionRepo;
    use tod_store::outline::repos::PlanStepRepo;
    use tod_store::outline::repos::plan_steps::HandoffReason;
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
                        ..Default::default()
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

    /// A request offers its asking conversation to a terminal once that
    /// conversation has an agent session to resume, and not before.
    #[test]
    fn a_request_links_the_session_that_asked_it_once_it_has_one() {
        let fixture = Fixture::new();
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
        let decision = fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AskDecision {
                    node_id: fixture.node_id,
                    conversation_id: Some(conversation_id),
                    protocol: Some("implementation".into()),
                    decision: tod_store::decisions::NewDecision {
                        question: "Which store?".into(),
                        options: vec!["SQLite".into(), "Files".into()],
                        ..Default::default()
                    },
                },
            )
            .unwrap()
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap();

        let loaded = crate::unified::requests::load(&fixture.store, fixture.node_id);
        assert!(loaded.items.iter().any(|i| i.id == decision));
        assert_eq!(loaded.sessions.get(&decision), None, "no agent session yet");

        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::SetConversationSession {
                    conversation_id,
                    agent_session_id: Some("session-1".into()),
                    session_name: None,
                },
            )
            .unwrap();
        let loaded = crate::unified::requests::load(&fixture.store, fixture.node_id);
        assert_eq!(loaded.sessions.get(&decision), Some(&conversation_id));
    }

    fn open_panel<'a>(
        node_id: Uuid,
        fixture: &Fixture,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Requests>, Entity<AgentRuns>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let fleet = fixture.store.clone();
        let agent_runs = cx.new(|_| AgentRuns::new(fleet.clone(), mock_agent()));
        let agent_runs_for_view = agent_runs.clone();
        let lifecycle = cx.new(|_| LifecycleController::new(fleet.clone()));
        let runners = super::test_runners(&fleet, &agent_runs, cx);
        let slot = Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                TaskPanel::new(node_id, fleet, agent_runs_for_view, lifecycle, runners, window, cx)
            });
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view: Entity<TaskPanel> = slot.borrow_mut().take().unwrap();
        draw(cx);
        cx.run_until_parked();
        let requests = view.read_with(cx, |view, _| view.requests.clone());
        (requests, agent_runs, cx)
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
        let (view, _agent_runs, cx) = open_panel(fixture.node_id, &fixture, cx);

        view.read_with(cx, |view, _| {
            let ids: Vec<_> = view.loaded.pending.iter().map(|d| d.id).collect();
            assert_eq!(ids, [first, second]);
        });
    }

    #[gpui::test]
    fn digit_key_answers_the_top_pending_decision(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let decision_id = ask(&fixture, "Round per line or per invoice?", &["per line", "per invoice"]);
        let (view, _agent_runs, cx) = open_panel(fixture.node_id, &fixture, cx);

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
        let (view, agent_runs, cx) = open_panel(fixture.node_id, &fixture, cx);

        agent_runs
            .update(cx, |runs, cx| runs.answer_decision(decision_id, Some(1), None, cx))
            .unwrap();
        cx.run_until_parked();
        view.update(cx, |view, cx| view.reload(cx));
        cx.run_until_parked();
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
                        ..Default::default()
                    },
                },
            )
            .ok();
        let (view, _agent_runs, cx) = open_panel(fixture.node_id, &fixture, cx);
        view.read_with(cx, |view, _| assert_eq!(view.loaded.pending.len(), 1));

        view.update(cx, |view, cx| view.set_node(None, cx));
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

        let (view, _agent_runs, cx) = open_panel(fixture.node_id, &fixture, cx);
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

    /// The implementation conversation a request came from, for the
    /// "answered elsewhere" tests.
    fn implementation_conversation(fixture: &Fixture) -> Uuid {
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
        conversation_id
    }

    fn turn_count(fixture: &Fixture, conversation_id: Uuid) -> usize {
        fixture
            .store
            .read(|conn| tod_store::conversation::ConversationRepo::new(conn).turns(conversation_id))
            .unwrap()
            .len()
    }

    /// A decision settled in the agent's own session is dismissed with an
    /// answer saying so, and nothing is sent to the agent.
    #[gpui::test]
    fn a_decision_answered_elsewhere_is_dismissed_without_a_turn(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        tod_store::paths::set_data_root(fixture.store.paths().root().to_path_buf());
        let conversation_id = implementation_conversation(&fixture);
        let decision = fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AskDecision {
                    node_id: fixture.node_id,
                    conversation_id: Some(conversation_id),
                    protocol: Some("implementation".into()),
                    decision: tod_store::decisions::NewDecision {
                        question: "Which store?".into(),
                        options: vec!["SQLite".into(), "Files".into()],
                        ..Default::default()
                    },
                },
            )
            .unwrap()
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap();

        let (view, _agent_runs, cx) = open_panel(fixture.node_id, &fixture, cx);
        view.update(cx, |view, cx| {
            let item = view.loaded.items[0].clone();
            assert_eq!(item.id, decision);
            view.resolve_elsewhere(&item, Source::Click, cx);
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(view.loaded.items.is_empty(), "the request is dismissed");
            let answer = &view.loaded.log.last().expect("an answer is logged").answer;
            assert_eq!(answer.text.as_deref(), Some(crate::ui::agent_runs::RESOLVED_ELSEWHERE));
        });
        assert_eq!(turn_count(&fixture, conversation_id), 0, "no turn goes to the agent");
    }

    /// A handed-back plan step settled elsewhere goes back to `in_progress`
    /// with no answer sent.
    #[gpui::test]
    fn a_plan_step_answered_elsewhere_goes_back_to_in_progress(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        tod_store::paths::set_data_root(fixture.store.paths().root().to_path_buf());
        let step_id = fixture.steps[0];
        fixture
            .store
            .enqueue_outline(tod_store::outline::OutlineMutation::UpdatePlanStepStatus {
                step_id,
                status: tod_store::outline::repos::plan_steps::STATUS_BLOCKED.to_string(),
                note: Some("Which rounding?".to_string()),
                reason: None,
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();
        let conversation_id = implementation_conversation(&fixture);

        let (view, _agent_runs, cx) = open_panel(fixture.node_id, &fixture, cx);
        view.update(cx, |view, cx| {
            let item = view.loaded.items[0].clone();
            assert_eq!(item.id, step_id);
            view.resolve_elsewhere(&item, Source::Click, cx);
        });
        cx.run_until_parked();

        let status = fixture
            .store
            .read(|conn| PlanStepRepo::new(conn).get(step_id))
            .unwrap()
            .unwrap()
            .status;
        assert_eq!(status, tod_store::outline::repos::plan_steps::STATUS_IN_PROGRESS);
        view.read_with(cx, |view, _| assert!(view.loaded.items.is_empty()));
        assert_eq!(turn_count(&fixture, conversation_id), 0, "no turn goes to the agent");
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

        let (view, _agent_runs, cx) = open_panel(fixture.node_id, &fixture, cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.items.len(), 2);
            assert!(view.loaded.items[0].since <= view.loaded.items[1].since);
            assert_eq!(view.loaded.items[0].kind, AttentionKind::PlanStep);
            assert_eq!(view.loaded.items[1].kind, AttentionKind::Decision);
        });
    }
}
