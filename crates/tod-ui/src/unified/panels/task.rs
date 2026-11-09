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
    AnyElement, App, ScrollHandle, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
};
use tod_store::fleet::FleetStore;
use tod_store::outline::repos::plan_steps::STATUS_VERIFIED;
use uuid::Uuid;

use crate::ui::selectable_text::{selectable_text, selectable_text_colored};
use crate::ui::style;
use crate::ui::panel_export::{RecordText as _, recorded};

// T2: the runner line.
use gpui::{ElementId, Entity, Subscription};
use gpui_component::Sizable as _;
use gpui_component::Disableable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::Scrollbar;
use tod_core::conversation::ConversationStatus;
use tod_core::runner_status::{LiveTurn, format_tokens};
use tod_core::next_actor::{NextActor, NodeStanding, YouReason};
use crate::unified::panels::runner_menu::{
    self, EndedHow, Place, RunState, RunnerAction, RunnerActions, RunnerInputs,
};
use crate::views::task_list::standing::{self, PartTone};
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
    /// The ticket is done or canceled: its id is struck through.
    pub ticket_terminal: bool,
    /// The pull requests linked to the node (its Ticket capability's links,
    /// where `tod-cli pr open` adds the one it opens).
    pub pull_requests: Vec<tod_store::github::NodePr>,
    pub obligation_count: usize,
    /// Obligations whose latest verdict is `failed`.
    pub obligations_failed: usize,
    pub plan_done: usize,
    pub plan_total: usize,
    /// Review findings recorded on the node, and how many are still open.
    pub findings_total: usize,
    pub findings_open: usize,
}

impl TaskHeader {
    pub(crate) fn findings_label(&self) -> String {
        if self.findings_open > 0 {
            format!("Findings {} · {} open", self.findings_total, self.findings_open)
        } else {
            format!("Findings {}", self.findings_total)
        }
    }

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
        header.ticket_terminal = metadata
            .as_ref()
            .is_some_and(tod_store::outline::types::ticket_is_terminal);
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
    if let Ok(findings) =
        fleet.read(|conn| tod_store::review::ReviewRepo::new(conn).list_for_node(node_id))
    {
        header.findings_total = findings.len();
        header.findings_open = findings.iter().filter(|f| f.is_open()).count();
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
/// (`lifecycle`, where the node's files are, the locations) is read off the
/// UI thread on store change; `run_since` is when this panel first saw the
/// node's conversation running.
#[derive(Debug, Default, Clone)]
struct RunnerLine {
    lifecycle: String,
    /// Where the node's files are, which decides the runner menu; `None`
    /// until the first read, when the menu is empty rather than wrong.
    place: Option<Place>,
    /// The autopilot runs under the supervisor in the node's sandbox
    /// (`tod_core::runner_place::RunnerPlace::Supervisor`).
    supervised: bool,
    /// The node has a checkout of its own (a made Files location).
    has_checkout: bool,
    /// The node has children: it cannot be migrated.
    has_children: bool,
    /// Every global location, for Migrate to…, and the one the node uses.
    locations: Vec<tod_store::fleet::FilesPlace>,
    current_location: Option<String>,
    /// Where the runner runs, for the line: "this machine", or this machine
    /// and the dev container its work is in, or its sandbox.
    place_text: String,
    /// Reading the store for the above failed: shown on the line.
    load_error: Option<String>,
    /// The node's recorded background problems (`tod_store::node_problems`):
    /// failures that happened off any turn of the user's, such as the
    /// daemon's delete of this node's sandbox at `done`.
    problems: Vec<tod_store::node_problems::NodeProblem>,
    /// `(slot id, ms)`: the running slot and when it was first seen running.
    run_since: Option<(u64, i64)>,
    /// The status has an elapsed time, so the ticker re-renders each second.
    ticking: bool,
    /// Set when the node has a cloud row: its supervisor is its runner.
    cloud: Option<CloudNode>,
    /// The sandbox its Files location records (the one record of it).
    cloud_sandbox: Option<String>,
    /// Its old records name a sandbox its location does not: the menu offers
    /// to adopt it.
    adoptable: bool,
    /// A job of the panel's own (Start, Sync now, Delete sandbox…) is in
    /// progress, and what it is doing.
    cloud_busy: Option<&'static str>,
    /// The latest word from a job; an error when `cloud_failed`.
    cloud_note: Option<String>,
    cloud_failed: bool,
    /// Pause was asked of the supervisor and it has not stopped yet.
    pause_asked: bool,
    /// A question the runner line asks before going on (see [`Prompt`]).
    prompt: Option<Prompt>,
    /// Migrate to… the location, once the runner has ended.
    migration_wait: Option<tod_store::fleet::FilesPlace>,
}

/// A question the runner line asks, with its buttons beside it.
#[derive(Debug, Clone, PartialEq)]
enum Prompt {
    /// Migrate while the runner is active: Pause (finish this step, then
    /// migrate), Stop now (interrupt, then migrate) or Cancel.
    MigrateRunning { place: tod_store::fleet::FilesPlace, can_pause: bool },
    /// Delete sandbox while the runner is active: Stop and delete, or Cancel.
    DeleteRunning { checkpoint: bool },
}

impl Prompt {
    fn text(&self) -> String {
        match self {
            Prompt::MigrateRunning { place, .. } => format!(
                "This node is running. Migrate to {} once it has paused (it finishes this step) or been stopped now?",
                place.name
            ),
            Prompt::DeleteRunning { .. } => "This node is running. Stop it and delete its sandbox?".to_string(),
        }
    }

    /// The buttons, in order, by label.
    fn buttons(&self) -> Vec<&'static str> {
        match self {
            Prompt::MigrateRunning { can_pause: true, .. } => vec!["Pause", "Stop now", "Cancel"],
            Prompt::MigrateRunning { can_pause: false, .. } => vec!["Stop now", "Cancel"],
            Prompt::DeleteRunning { .. } => vec!["Stop and delete", "Cancel"],
        }
    }
}

/// What `load_runner` reads: the store half of [`RunnerLine`].
#[derive(Debug, Default, Clone)]
struct RunnerFacts {
    lifecycle: String,
    cloud: Option<CloudNode>,
    cloud_sandbox: Option<String>,
    adoptable: bool,
    place: Option<Place>,
    supervised: bool,
    has_checkout: bool,
    has_children: bool,
    locations: Vec<tod_store::fleet::FilesPlace>,
    current_location: Option<String>,
    place_text: String,
    problems: Vec<tod_store::node_problems::NodeProblem>,
    error: Option<String>,
}

/// Everything the runner menu needs from the store: the node's lifecycle
/// state, its cloud row, where its files are (and whether a sandbox exists),
/// where its runner runs, whether it has a checkout of its own and children,
/// and the global locations. Reads only, but not for the UI thread. A read
/// that fails is said, never taken for "no".
fn load_runner(fleet: &FleetStore, node_id: Uuid) -> RunnerFacts {
    let mut facts = RunnerFacts::default();
    let id = node_id.to_string();
    facts.lifecycle = fleet
        .get_node(&id)
        .ok()
        .flatten()
        .map(|n| n.lifecycle)
        .unwrap_or_default();
    facts.cloud = tod_core::cloud_sync::cloud_node(fleet, &id);
    match tod_core::node_problems::for_node(fleet, node_id) {
        Ok(problems) => facts.problems = problems,
        Err(err) => {
            tracing::error!("could not read the problems of node {node_id}: {err:#}");
            facts.error = Some(format!("Could not read this node's problems: {err:#}"));
        }
    }
    match load_place(fleet, node_id, &mut facts) {
        Ok(()) => {}
        Err(err) => {
            tracing::error!("could not read where node {node_id} runs: {err:#}");
            facts.error = Some(format!("Could not read where this node runs: {err:#}"));
        }
    }
    facts
}

fn load_place(fleet: &FleetStore, node_id: Uuid, facts: &mut RunnerFacts) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let id = node_id.to_string();
    let files = fleet.resolve_files_for_node(&id).context("read the node's Files")?;
    let location = tod_store::fleet::provision::node_location(fleet, &id).context("read the node's location")?;
    facts.supervised = tod_core::runner_place::RunnerPlace::of(fleet, node_id)? == tod_core::runner_place::RunnerPlace::Supervisor;
    facts.has_checkout = location.is_some();
    facts.has_children = fleet.node_has_children(node_id).context("look for the node's children")?;
    facts.locations = tod_store::fleet::places::list(fleet).context("list the locations")?;
    facts.current_location = files.as_ref().and_then(|f| f.place_id.clone());
    let sandbox_made = location.as_ref().and_then(|l| l.sandbox().map(str::to_string));
    facts.cloud_sandbox = sandbox_made.clone();
    facts.adoptable = tod_core::cloud_sync::adopt::adoptable(fleet, fleet.paths().root(), &id)
        .context("look for an old sandbox record to adopt")?;
    let (place, text) = match files.as_ref().and_then(|files| files.dev_container.as_ref()) {
        Some(dev) if dev.sandbox => {
            // The sandbox is made when something first needs the node's
            // files, so it can exist before the runner is started.
            // Only the node's Files location says which sandbox it has.
            let exists = sandbox_made.is_some();
            let text = match &sandbox_made {
                Some(name) => format!("sandbox {name}"),
                None => "cloud sandbox, not made yet".to_string(),
            };
            (Place::Sandbox { exists }, text)
        }
        Some(dev) => match dev.container.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(container) => (Place::DevContainer, format!("this machine, dev container {container}")),
            None => (Place::DevContainer, "this machine, a dev container".to_string()),
        },
        None => (Place::Host, "this machine".to_string()),
    };
    facts.place = Some(place);
    facts.place_text = text;
    Ok(())
}

/// What `run_state` reads.
struct RunFacts<'a> {
    done: bool,
    supervised: bool,
    /// The daemon (or this process) is running the node's autopilot.
    runner_running: bool,
    pausing: bool,
    /// A conversation the user started by hand is running.
    hand_turn: bool,
    /// How the last local run ended.
    outcome: Option<&'a Outcome>,
    actor: Option<&'a NextActor>,
    /// Pause was asked of a supervisor that has not stopped yet.
    pause_asked: bool,
}

/// Where the node's run stands (`doc/runner-menu/design.md`, "The runner's
/// state, wherever it runs"): locally from the runner's own state, else, and
/// for a supervised node, from who is next (`tod_core::next_actor`).
fn derive_run_state(f: &RunFacts) -> RunState {
    if f.done {
        return RunState::Done;
    }
    if f.runner_running {
        return RunState::Active { pausing: f.pausing };
    }
    if f.hand_turn {
        return RunState::HandTurn;
    }
    if f.supervised && matches!(f.actor, Some(NextActor::Working { .. })) {
        return RunState::Active { pausing: f.pause_asked };
    }
    // The last local run's own word, when it has one.
    match f.outcome {
        Some(Outcome::BudgetExhausted { .. }) => return RunState::Ended(EndedHow::Budget),
        Some(Outcome::Stopped { .. }) => return RunState::Ended(EndedHow::Stopped),
        Some(Outcome::NeedsHuman { .. }) => return RunState::Ended(EndedHow::Request),
        Some(Outcome::Waiting { .. }) => return RunState::Ended(EndedHow::Waiting),
        Some(Outcome::Done) | None => {}
    }
    match f.actor {
        Some(NextActor::You { why, .. }) => match why {
            YouReason::Paused => RunState::Ended(EndedHow::Paused),
            YouReason::Stopped { reason } if reason.to_lowercase().contains("budget") => {
                RunState::Ended(EndedHow::Budget)
            }
            YouReason::Stopped { .. } => RunState::Ended(EndedHow::Stopped),
            YouReason::Failed { .. } => RunState::Ended(EndedHow::Failed),
            YouReason::Request { .. } | YouReason::Permission { .. } => RunState::Ended(EndedHow::Request),
            YouReason::NotStarted => RunState::NeverRun,
        },
        // Waiting on something else, or worked by a process this one has not
        // heard from: there is nothing to start or resume.
        Some(NextActor::Others { .. }) | Some(NextActor::Working { .. }) => RunState::Ended(EndedHow::Waiting),
        Some(NextActor::Done) => RunState::Done,
        None => RunState::NeverRun,
    }
}

/// What a check found, in words for the runner line, and whether it is a
/// failure (red): the gate's verdict and where it went, then what reading
/// GitHub found, an unreadable GitHub included.
fn describe_check(found: &tod_core::lifecycle::CheckNow) -> (String, bool) {
    use tod_core::autopilot::GithubCheck;
    let mut failed = false;
    let mut text = if let Some(state) = found.advanced {
        format!("Checked: the gate is clear; moved to {state}")
    } else if found.gate.to.is_none() {
        format!("Checked: {} has no gate", found.gate.from)
    } else if found.gate.clear() {
        match &found.held {
            Some(why) => format!("Checked: the gate is clear, but the node was not moved on: {why}"),
            None => "Checked: the gate is clear".to_string(),
        }
    } else {
        let failing: Vec<String> = found.gate.failing().map(|c| format!("{} ({})", c.criterion.label, c.detail)).collect();
        format!("Checked: the gate is not clear: {}", failing.join("; "))
    };
    match &found.github {
        Some(GithubCheck::Waiting { what, .. }) => text.push_str(&format!(". GitHub: waiting for {what}")),
        Some(GithubCheck::Feedback { what }) => text.push_str(&format!(". GitHub: {what}")),
        Some(GithubCheck::Clear) => text.push_str(". GitHub: nothing left to wait for"),
        Some(GithubCheck::Unreadable(why)) => {
            failed = true;
            text.push_str(&format!(". GitHub could not be read: {why}"));
        }
        Some(GithubCheck::NothingToWaitOn) | None => {}
    }
    (text, failed)
}


pub struct TaskPanel {
    node_id: Uuid,
    fleet: Arc<FleetStore>,
    /// Asks a supervisor to pause or stop now (the request kind), off the UI
    /// thread. A field so a test can stand in for the network.
    stop_job: StopJob,
    focus_handle: FocusHandle,
    header: TaskHeader,
    pending_refresh: bool,
    /// T8: the files-changed count behind the Changes link.
    changes: ChangesWatch,
    _poll: gpui::Task<()>,
    agent_runs: Entity<AgentRuns>,
    runner: RunnerLine,
    /// Who is next, pushed by the workbench from the one pass every view
    /// reads (`tod_core::next_actor`).
    standing: Option<NodeStanding>,
    runners: Entity<NodeRunners>,
    _runner_tick: gpui::Task<()>,
    _agent_runs_sub: Subscription,
    _runners_sub: Subscription,
    /// T4: what the task is waiting on the user for.
    pub(crate) requests: Entity<Requests>,
    _requests_subs: Vec<Subscription>,
    /// T5: whether the Answered drawer is open; kept for the session only.
    pub(crate) answered_open: bool,
    /// Scroll state of the header (its runner line can carry a long error), of
    /// the requests, and of the Answered list; each shows a scrollbar only
    /// when its content overflows.
    header_scroll: ScrollHandle,
    requests_scroll: ScrollHandle,
    answered_scroll: ScrollHandle,
}

/// Starts the job that asks a node's supervisor to pause
/// (`cloud_sync::REQUEST_PAUSE`) or stop now (`REQUEST_STOP_NOW`); its
/// updates reach [`TaskPanel::on_cloud_update`].
type StopJob = fn(&mut TaskPanel, &'static str, &mut Context<TaskPanel>);

/// The real [`StopJob`]: `cloud_node::pause` / `cloud_node::stop_now`.
fn ask_supervisor_to_stop(panel: &mut TaskPanel, kind: &'static str, cx: &mut Context<TaskPanel>) {
    let (node, fleet) = (panel.node_id, panel.fleet.clone());
    let on_update = move |this: &mut TaskPanel, update, cx: &mut Context<TaskPanel>| this.on_cloud_update(node, update, cx);
    if kind == tod_core::cloud_sync::REQUEST_PAUSE {
        crate::views::cloud_node::pause(fleet, node.to_string(), cx, on_update);
    } else {
        crate::views::cloud_node::stop_now(fleet, node.to_string(), cx, on_update);
    }
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
            stop_job: ask_supervisor_to_stop,
            focus_handle,
            requests,
            _requests_subs,
            answered_open: false,
            header_scroll: ScrollHandle::new(),
            requests_scroll: ScrollHandle::new(),
            answered_scroll: ScrollHandle::new(),
            header,
            pending_refresh: false,
            changes,
            _poll,
            agent_runs,
            runner: RunnerLine::default(),
            standing: None,
            runners,
            _runner_tick,
            _agent_runs_sub,
            _runners_sub,
        };
        panel.track_run_since(cx);
        panel.refresh_pr_states(cx);
        panel.refresh_runner(cx);
        ChangesWatch::recompute(&mut panel, node_id, cx);
        panel
    }

    #[cfg(test)]
    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    /// The node this panel shows now (it can be retargeted in place).
    pub fn node_id_now(&self) -> Uuid {
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
        self.refresh_pr_states(cx);
        cx.notify();
    }

    /// Read the state of the linked pull requests from GitHub, off the UI
    /// thread, and redraw when one changed (a merged one is struck through).
    fn refresh_pr_states(&mut self, cx: &mut Context<Self>) {
        let prs: Vec<_> = self
            .header
            .pull_requests
            .iter()
            .filter(|pr| tod_core::pull_requests::pull_state_stale(pr))
            .cloned()
            .collect();
        if prs.is_empty() {
            return;
        }
        let root = self.fleet.paths().root().to_path_buf();
        cx.spawn(async move |this, cx| {
            let changed = cx
                .background_executor()
                .spawn(async move { tod_core::pull_requests::refresh_pull_states(&root, &prs) })
                .await;
            if changed {
                this.update(cx, |_, cx| cx.notify()).ok();
            }
        })
        .detach();
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
                            Some(url) => self.external_link(
                                "unified-task-ticket",
                                ticket,
                                url,
                                self.header.ticket_terminal,
                                cx,
                            ),
                            None => style::text_muted(div())
                                .when(self.header.ticket_terminal, |el| el.line_through())
                                .child(ticket.clone())
                                .record_text(ticket)
                                .into_any_element(),
                        })
                    })
                    .children(self.header.pull_requests.iter().enumerate().map(|(ix, pr)| {
                        let label = format!("{}#{}", pr.repo, pr.pr_number);
                        // A merged or closed pull request is struck through.
                        let finished = tod_core::pull_requests::cached_pull_state(pr)
                            .is_some_and(|state| state.is_terminal());
                        self.external_link(("unified-task-pr", ix), label, pr.url.clone(), finished, cx)
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
        struck: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        style::text_link_external(div().id(id))
            .when(struck, |el| el.line_through())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_this, _event: &MouseDownEvent, _, cx| {
                    cx.open_url(&url);
                }),
            )
            .child(label.clone())
            .record_text(label)
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
            .child(label.clone())
            .record_text(label)
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
        // Only when the review has recorded something, as the details panel
        // shows it.
        let findings = (self.header.findings_total > 0).then(|| {
            self.artifact_link(
                "unified-task-findings",
                self.header.findings_label(),
                PanelKind::Findings(node_id),
                cx,
            )
        });
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .child(obligations)
            .child(plan)
            .children(findings)
            // Only a current count: none while unknown or recomputing.
            .children(self.changes.state.label().map(|label| {
                self.artifact_link("unified-task-changes", label, PanelKind::Changes(node_id), cx)
            }))
            .child(
                div()
                    .ml_auto()
                    .child(self.artifact_link("unified-task-journey", "Journey".into(), PanelKind::Journey(node_id), cx)),
            )
            .into_any_element()
    }

    /// Re-read the lifecycle state and attention off the UI thread.
    fn refresh_runner(&mut self, cx: &mut Context<Self>) {
        let fleet = self.fleet.clone();
        let node_id = self.node_id;
        cx.spawn(async move |this, cx| {
            let facts = cx
                .background_executor()
                .spawn(async move { load_runner(&fleet, node_id) })
                .await;
            let _ = this.update(cx, |this: &mut TaskPanel, cx| {
                if this.node_id == node_id {
                    this.runner.lifecycle = facts.lifecycle;
                    this.runner.cloud = facts.cloud;
                    this.runner.cloud_sandbox = facts.cloud_sandbox;
                    this.runner.adoptable = facts.adoptable;
                    this.runner.place = facts.place;
                    this.runner.supervised = facts.supervised;
                    this.runner.has_checkout = facts.has_checkout;
                    this.runner.has_children = facts.has_children;
                    this.runner.locations = facts.locations;
                    this.runner.current_location = facts.current_location;
                    this.runner.place_text = facts.place_text;
                    this.runner.load_error = facts.error;
                    this.runner.problems = facts.problems;
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

    /// The turn that is live on the node right now, if any: the runner's, or
    /// a conversation run by hand. Who is next is the standing's, never this.
    fn runner_status(&self, cx: &App) -> (Option<u64>, Option<LiveTurn>) {
        let slot = self.node_slot(cx);
        let live = LiveTurn::of(
            self.runners.read(cx).runner(self.node_id),
            slot.as_ref().map(|(_, s)| s),
            self.runner.run_since.map(|(_, at)| at),
        );
        (slot.map(|(id, _)| id), live)
    }

    /// The node is done (or has been shown to be).
    fn is_done(&self) -> bool {
        match &self.standing {
            Some(standing) => matches!(standing.actor, NextActor::Done),
            None => self.runner.lifecycle == tod_core::runner_status::DONE_STATE,
        }
    }

    /// Stop the node's running turn: the conversation view's own stop path.
    fn stop_runner(&mut self, slot: u64, cx: &mut Context<Self>) {
        self.agent_runs.update(cx, |runs, cx| runs.stop(slot, cx));
    }

    /// Where the node's run stands now (see [`derive_run_state`]).
    fn run_state(&self, live: Option<&LiveTurn>, cx: &App) -> RunState {
        let runners = self.runners.read(cx);
        let node = self.node_id;
        let running = runners.is_running(node);
        derive_run_state(&RunFacts {
            done: self.is_done(),
            supervised: self.runner.supervised,
            runner_running: running,
            pausing: runners.is_pausing(node),
            hand_turn: live.is_some() && !running,
            // A supervised node's runs are the supervisor's: this machine's
            // last outcome (from before a migration) says nothing of them.
            outcome: if self.runner.supervised { None } else { runners.outcome(node) },
            actor: self.standing.as_ref().map(|s| &s.actor),
            pause_asked: self.runner.pause_asked,
        })
    }

    /// What the split button offers: `runner_menu::menu` over what the panel
    /// knows. Empty until the store has been read once.
    fn runner_actions(&self, live: Option<&LiveTurn>, cx: &App) -> RunnerActions {
        let Some(place) = self.runner.place else {
            return RunnerActions { default: None, menu: Vec::new(), busy: self.runner.cloud_busy };
        };
        runner_menu::menu(&RunnerInputs {
            place,
            supervised: self.runner.supervised,
            run: self.run_state(live, cx),
            has_checkout: self.runner.has_checkout,
            has_children: self.runner.has_children,
            adoptable: self.runner.adoptable,
            busy: self.runner.cloud_busy,
        })
    }

    /// The journey's snapshot of the split button and the line beside it:
    /// the default action (flagged), the menu's entries, and a disabled
    /// default while a job of the panel's own runs.
    fn presented(&self, actions: &RunnerActions) -> Presented {
        let mut presented: Vec<PresentedAction> = Vec::new();
        if let Some(action) = actions.default {
            presented.push(PresentedAction {
                id: format!("{action:?}"),
                label: action.label().to_string(),
                primary: true,
                disabled: false,
            });
        }
        if let Some(what) = actions.busy {
            presented.push(PresentedAction { id: "Busy".to_string(), label: what.to_string(), primary: true, disabled: true });
        }
        presented.extend(actions.menu.iter().map(|action| PresentedAction {
            id: format!("{action:?}"),
            label: action.label().to_string(),
            primary: false,
            disabled: false,
        }));
        Presented {
            actions: presented,
            focused: None,
            notices: vec![format!(
                "{} · {}",
                self.runner.lifecycle,
                self.standing
                    .as_ref()
                    .map_or("no standing".to_string(), |s| format!("{:?}", s.actor))
            )],
        }
    }

    /// Records a `UserAction` for `id` with the split button as presented now.
    fn record(&self, id: String, cx: &mut Context<Self>) {
        let (_, live) = self.runner_status(cx);
        let presented = self.presented(&self.runner_actions(live.as_ref(), cx));
        crate::ui::journey::record_action(
            cx,
            Focus::Node(self.node_id),
            id,
            crate::ui::journey::Source::Click,
            "task_panel",
            presented,
        );
    }

    /// The split button's click handler, its default action's and its
    /// menu's: records the `UserAction`, then does it where the node's
    /// runner runs (`tod_core::runner_place`): the daemon's local runner
    /// through [`NodeRunners`], or the supervisor in the node's sandbox
    /// through `views::cloud_node`.
    fn perform(&mut self, action: RunnerAction, cx: &mut Context<Self>) {
        use RunnerAction as A;
        let (slot, live) = self.runner_status(cx);
        let run = self.run_state(live.as_ref(), cx);
        self.record(format!("{action:?}"), cx);
        let node = self.node_id;
        let supervised = self.runner.supervised;
        match action {
            A::Start if supervised => self.supervised_go(None, cx),
            A::Resume if supervised => self.supervised_go(Some(tod_core::cloud_sync::REQUEST_KEEP_GOING), cx),
            A::Start | A::Resume => {
                let _ = self.runners.update(cx, |r, cx| r.start(node, false, cx));
            }
            A::ResumeFreshBudget if supervised => {
                self.supervised_go(Some(tod_core::cloud_sync::REQUEST_FRESH_BUDGET), cx)
            }
            A::ResumeFreshBudget => {
                let _ = self.runners.update(cx, |r, cx| r.start(node, true, cx));
            }
            A::RunFresh if supervised => self.supervised_go(Some(tod_core::cloud_sync::REQUEST_FRESH_SESSION), cx),
            A::RunFresh => {
                let root = self.fleet.paths().root().to_path_buf();
                match tod_core::autopilot::AutopilotState::request_fresh_session(&root, node) {
                    Ok(()) => {
                        let _ = self.runners.update(cx, |r, cx| r.start(node, false, cx));
                    }
                    Err(err) => self.runners.update(cx, |r, cx| r.fail(node, format!("{err:#}"), cx)),
                }
            }
            A::Pause => self.pause_run(cx),
            A::StopNow => self.stop_run(run, slot, cx),
            A::CheckNow => self.check_now(cx),
            // A submenu of the locations: choosing one is `choose_migration`.
            A::MigrateTo => self.runners.update(cx, |r, cx| {
                r.fail(node, "Migrate to… needs a location: pick one from its submenu".into(), cx)
            }),
            A::ResetEnvironment => {
                self.cloud_started("Setting the environment up again…", cx);
                crate::views::cloud_node::reset_environment(self.fleet.clone(), node, cx, move |this, update, cx| {
                    this.on_cloud_update(node, update, cx)
                });
            }
            A::AdoptSandbox => {
                self.cloud_started("Adopting the existing sandbox…", cx);
                crate::views::cloud_node::adopt_sandbox(self.fleet.clone(), node.to_string(), cx, move |this, update, cx| {
                    this.on_cloud_update(node, update, cx)
                });
            }
            A::UpdateWorkspace => self.update_workspace(cx),
            A::DeleteSandbox | A::DeleteSandboxAnyway => {
                let checkpoint = action == A::DeleteSandbox;
                if matches!(run, RunState::Active { .. }) {
                    self.runner.prompt = Some(Prompt::DeleteRunning { checkpoint });
                } else {
                    self.cloud_started("Deleting the sandbox…", cx);
                    if checkpoint {
                        crate::views::cloud_node::delete_sandbox(
                            self.fleet.clone(),
                            node.to_string(),
                            cx,
                            move |this, update, cx| this.on_cloud_update(node, update, cx),
                        );
                    } else {
                        crate::views::cloud_node::delete_sandbox_anyway(
                            self.fleet.clone(),
                            node.to_string(),
                            cx,
                            move |this, update, cx| this.on_cloud_update(node, update, cx),
                        );
                    }
                }
            }
            A::SyncNow => {
                self.cloud_started("Syncing…", cx);
                crate::views::cloud_node::sync_now(self.fleet.clone(), Some(node), cx, move |this, update, cx| {
                    this.on_cloud_update(node, update, cx)
                });
            }
        }
        cx.notify();
    }

    /// Start, or go on with, a supervised node. `None` starts it (making its
    /// sandbox when it has none); a request goes to its supervisor, except
    /// that a node with no sandbox now (never made, deleted, or lost) can
    /// only be started: its new sandbox starts the step in a new session.
    fn supervised_go(&mut self, kind: Option<&'static str>, cx: &mut Context<Self>) {
        let node = self.node_id;
        let has_sandbox = matches!(self.runner.place, Some(Place::Sandbox { exists: true }))
            && self.runner.cloud.as_ref().is_some_and(|cloud| cloud.lost_at.is_none());
        match kind {
            Some(kind) if has_sandbox => self.cloud_request(kind, cx),
            _ => {
                self.cloud_started("Starting…", cx);
                crate::views::cloud_node::run_in_cloud(self.fleet.clone(), node.to_string(), cx, move |this, update, cx| {
                    this.on_cloud_update(node, update, cx)
                });
            }
        }
    }

    /// Pause: the local runner stops once its turn ends; a supervised one is
    /// asked to, at its next boundary.
    fn pause_run(&mut self, cx: &mut Context<Self>) {
        let node = self.node_id;
        if self.runner.supervised {
            self.runner.pause_asked = true;
            self.cloud_started("Asking the supervisor to pause…", cx);
            (self.stop_job)(self, tod_core::cloud_sync::REQUEST_PAUSE, cx);
        } else {
            self.runners.update(cx, |r, cx| r.pause(node, cx));
        }
    }

    /// Stop now: a conversation the user started by hand through
    /// `AgentRuns`, the local runner through [`NodeRunners`], a supervised
    /// one by request.
    fn stop_run(&mut self, run: RunState, slot: Option<u64>, cx: &mut Context<Self>) {
        let node = self.node_id;
        match run {
            RunState::HandTurn => {
                if let Some(slot) = slot {
                    self.stop_runner(slot, cx);
                }
            }
            _ if self.runner.supervised => {
                self.cloud_started("Asking the supervisor to stop now…", cx);
                (self.stop_job)(self, tod_core::cloud_sync::REQUEST_STOP_NOW, cx);
            }
            _ => self.runners.update(cx, |r, cx| r.stop_now(node, cx)),
        }
    }

    /// Migrate to `place`: asks first while the runner is active, and calls
    /// the migration (`ui::files_impact::confirm_migrate`) only once it has
    /// ended (see `drain_migration`).
    fn choose_migration(&mut self, place: tod_store::fleet::FilesPlace, cx: &mut Context<Self>) {
        self.record(format!("MigrateTo({})", place.name), cx);
        let (_, live) = self.runner_status(cx);
        match self.run_state(live.as_ref(), cx) {
            RunState::Active { .. } => self.runner.prompt = Some(Prompt::MigrateRunning { place, can_pause: true }),
            RunState::HandTurn => self.runner.prompt = Some(Prompt::MigrateRunning { place, can_pause: false }),
            _ => self.runner.migration_wait = Some(place),
        }
        cx.notify();
    }

    /// An answer to the runner line's question: recorded with the buttons
    /// shown (the first is the default), then done.
    fn answer_prompt(&mut self, label: &'static str, cx: &mut Context<Self>) {
        let Some(prompt) = self.runner.prompt.take() else {
            return;
        };
        let presented = Presented {
            actions: prompt
                .buttons()
                .into_iter()
                .enumerate()
                .map(|(i, l)| PresentedAction { id: l.to_string(), label: l.to_string(), primary: i == 0, disabled: false })
                .collect(),
            focused: None,
            notices: vec![prompt.text()],
        };
        crate::ui::journey::record_action(
            cx,
            Focus::Node(self.node_id),
            label.to_string(),
            crate::ui::journey::Source::Click,
            "task_panel",
            presented,
        );
        let node = self.node_id;
        let (slot, live) = self.runner_status(cx);
        let run = self.run_state(live.as_ref(), cx);
        match (prompt, label) {
            (Prompt::MigrateRunning { place, .. }, "Pause") => {
                self.pause_run(cx);
                self.runner.migration_wait = Some(place);
            }
            (Prompt::MigrateRunning { place, .. }, "Stop now") => {
                self.stop_run(run, slot, cx);
                self.runner.migration_wait = Some(place);
            }
            (Prompt::DeleteRunning { checkpoint }, "Stop and delete") => {
                // A run hosted in this process is stopped here; the daemon's
                // and a supervisor's are stopped by the job itself.
                if self.runners.read(cx).is_running(node) {
                    self.runners.update(cx, |r, cx| r.stop_now(node, cx));
                }
                self.cloud_started("Stopping, then deleting the sandbox…", cx);
                crate::views::cloud_node::stop_and_delete_sandbox(
                    self.fleet.clone(),
                    node.to_string(),
                    checkpoint,
                    cx,
                    move |this, update, cx| this.on_cloud_update(node, update, cx),
                );
            }
            // Cancel: nothing changes.
            _ => {}
        }
        cx.notify();
    }

    /// Once the runner has ended, the pending migration begins: the sessions
    /// are kept, the impact is shown, and the node's location is changed.
    /// Called from render, which has the window the dialog needs.
    fn drain_migration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(place) = self.runner.migration_wait.clone() else {
            return;
        };
        if self.runner.cloud_busy.is_some() {
            return;
        }
        let (_, live) = self.runner_status(cx);
        if matches!(self.run_state(live.as_ref(), cx), RunState::Active { .. } | RunState::HandTurn) {
            return;
        }
        self.runner.migration_wait = None;
        self.runner.cloud_note = None;
        self.runner.cloud_failed = false;
        let weak = cx.weak_entity();
        let name = place.name.clone();
        crate::ui::files_impact::confirm_migrate(
            window,
            cx,
            self.fleet.clone(),
            tod_store::TodPaths::at(self.fleet.paths().root()),
            self.node_id.to_string(),
            place,
            Box::new(move |result, _window, cx| {
                let _ = weak.update(cx, |this: &mut TaskPanel, cx| {
                    let (note, failed) = match result {
                        Ok(()) => (format!("Migrated to {name}. Nothing was started."), false),
                        // The children refusal shows here too.
                        Err(message) => (format!("Could not migrate to {name}: {message}"), true),
                    };
                    this.runner.cloud_note = Some(note);
                    this.runner.cloud_failed = failed;
                    this.refresh_runner(cx);
                    cx.notify();
                });
            }),
        );
    }

    /// "Update workspace to latest main": git runs in the node's workspace
    /// (maybe in its sandbox), off the UI thread, and the line says what it did
    /// or why it left the checkout alone.
    fn update_workspace(&mut self, cx: &mut Context<Self>) {
        let node = self.node_id.to_string();
        let fleet = self.fleet.clone();
        self.runner.cloud_note = Some("Updating the workspace to the latest main…".to_string());
        self.runner.cloud_failed = false;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { tod_store::fleet::workspace_update::update_workspace_to_main(&fleet, &node) })
                .await;
            let _ = this.update(cx, |this: &mut TaskPanel, cx| {
                let (note, failed) = match result {
                    Ok(said) => (said, false),
                    Err(err) => (format!("Could not update the workspace: {err:#}"), true),
                };
                this.runner.cloud_note = Some(note);
                this.runner.cloud_failed = failed;
                this.refresh_runner(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// "Check now": the gate check the autopilot makes, with no agent
    /// (`tod_core::lifecycle::check_now`), off the UI thread; the line says
    /// what it found, an unreadable GitHub and a reason the node was not
    /// moved on included. It never starts or wakes the runner.
    fn check_now(&mut self, cx: &mut Context<Self>) {
        let node = self.node_id;
        let fleet = self.fleet.clone();
        let root = fleet.paths().root().to_path_buf();
        self.runner.cloud_note = Some("Checking the gate…".to_string());
        self.runner.cloud_failed = false;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { tod_core::lifecycle::check_now(&fleet, node, &root) })
                .await;
            let _ = this.update(cx, |this: &mut TaskPanel, cx| {
                let (note, failed) = match result {
                    Ok(found) => describe_check(&found),
                    Err(err) => (format!("Could not check: {err:#}"), true),
                };
                if this.node_id == node {
                    this.runner.cloud_note = Some(note);
                    this.runner.cloud_failed = failed;
                    this.refresh_runner(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }


    /// Defer (a menu of how long) for a node that is on the user, Bring back
    /// for one deferred, nothing otherwise.
    fn render_defer_control(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::views::task_list::actor::{self as defer_ui, DeferOffer};
        match DeferOffer::for_standing(self.standing.as_ref()) {
            DeferOffer::None => None,
            DeferOffer::BringBack => Some(
                Button::new("unified-task-bring-back")
                    .label(defer_ui::BRING_BACK_LABEL)
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.bring_back(cx)))
                    .into_any_element(),
            ),
            DeferOffer::Defer => {
                let weak = cx.weak_entity();
                Some(
                    Button::new("unified-task-defer")
                        .label("Defer…")
                        .small()
                        .dropdown_caret(true)
                        .dropdown_menu(move |mut popup, _, _| {
                            for how in tod_core::defer::DeferFor::PRESETS {
                                let weak = weak.clone();
                                popup = popup.item(PopupMenuItem::new(how.label()).on_click(move |_, _, cx| {
                                    let _ = weak.update(cx, |this: &mut TaskPanel, cx| this.defer(how, cx));
                                }));
                            }
                            popup
                        })
                        .into_any_element(),
                )
            }
        }
    }

    /// Take the node off the user's list for `how`, as a journey `UserAction`
    /// listing every choice shown. A failure is said on the line.
    fn defer(&mut self, how: tod_core::defer::DeferFor, cx: &mut Context<Self>) {
        use crate::views::task_list::actor::{defer_labels, preset_label, presented_of};
        let node = self.node_id;
        crate::ui::journey::record_action(
            cx,
            Focus::Node(node),
            preset_label(how),
            crate::ui::journey::Source::Click,
            "task_panel",
            presented_of(&defer_labels(), &preset_label(how)),
        );
        if let Err(err) = tod_core::defer::defer(&self.fleet, node, how) {
            self.runner.cloud_note = Some(format!("Could not defer: {err:#}"));
            self.runner.cloud_failed = true;
        }
        cx.notify();
    }

    /// Put the node back on the user's list.
    fn bring_back(&mut self, cx: &mut Context<Self>) {
        use crate::views::task_list::actor::{BRING_BACK_LABEL, presented_of};
        let node = self.node_id;
        crate::ui::journey::record_action(
            cx,
            Focus::Node(node),
            BRING_BACK_LABEL,
            crate::ui::journey::Source::Click,
            "task_panel",
            presented_of(&[BRING_BACK_LABEL.to_string()], BRING_BACK_LABEL),
        );
        if let Err(err) = tod_core::defer::undefer(&self.fleet, node) {
            self.runner.cloud_note = Some(format!("Could not bring it back: {err:#}"));
            self.runner.cloud_failed = true;
        }
        cx.notify();
    }

    /// Who is next for this node, as the workbench last computed it.
    pub fn set_standing(&mut self, standing: Option<NodeStanding>, cx: &mut Context<Self>) {
        if standing != self.standing {
            self.standing = standing;
            // A supervised node that is no longer working has paused.
            if !matches!(self.standing.as_ref().map(|s| &s.actor), Some(NextActor::Working { .. })) {
                self.runner.pause_asked = false;
            }
            cx.notify();
        }
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
        let recorded_current = current.clone();
        let chip = Button::new("unified-task-phase")
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
            });
        recorded(chip, recorded_current).into_any_element()
    }

    /// Records the user's request of `kind` (a new session, or going on) for
    /// the node's supervisor, answering a "leave it stopped" stop question to
    /// continue first, and wakes it, off the UI thread.
    fn cloud_request(&mut self, kind: &'static str, cx: &mut Context<Self>) {
        let node = self.node_id;
        self.cloud_started("Asking the supervisor…", cx);
        crate::views::cloud_node::request(self.fleet.clone(), node.to_string(), kind, cx, move |this, update, cx| {
            this.on_cloud_update(node, update, cx)
        });
    }

    /// A job of the panel's own begins: the button shows `what` until it ends.
    fn cloud_started(&mut self, what: &'static str, cx: &mut Context<Self>) {
        self.runner.cloud_busy = Some(what);
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
            CloudUpdate::Failed(msg) => (msg, true, true),
        };
        self.runner.cloud_note = (!note.is_empty()).then_some(note);
        self.runner.cloud_failed = failed;
        if done {
            self.runner.cloud_busy = None;
            // A pause or stop that could not be asked is not pending, and
            // neither is the migration that waited for it.
            if failed {
                self.runner.pause_asked = false;
                self.runner.migration_wait = None;
            }
            self.refresh_runner(cx);
        }
        cx.notify();
    }

    /// The split button: the default action, with the rest in its menu.
    /// While a job of the panel's own runs the button shows what it is doing,
    /// disabled, with an empty menu; after Pause (or Stop now) it says
    /// "Pausing…" until the runner has ended.
    fn render_runner_button(&self, actions: RunnerActions, pausing: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (primary, label, disabled) = runner_button_face(&actions, pausing)?;
        let button = Button::new("unified-task-runner-action")
            .label(label)
            .disabled(disabled)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(primary) = primary {
                    this.perform(primary, cx)
                }
            }));
        let mut split = DropdownButton::new("unified-task-runner").small().button(button);
        if !actions.menu.is_empty() {
            let weak = cx.weak_entity();
            let menu = actions.menu;
            let locations = self.runner.locations.clone();
            let current = self.runner.current_location.clone();
            split = split.dropdown_menu(move |mut popup, window, cx| {
                for action in menu.iter().copied() {
                    if action == RunnerAction::MigrateTo {
                        // A submenu of every location; the one the node
                        // uses is marked and cannot be picked.
                        let (weak, locations, current) = (weak.clone(), locations.clone(), current.clone());
                        popup = popup.submenu(action.label(), window, cx, move |mut sub, _, _| {
                            if locations.is_empty() {
                                sub = sub.item(PopupMenuItem::new("No locations yet").disabled(true));
                            }
                            for place in locations.iter().cloned() {
                                let is_current = current.as_deref() == Some(place.id.as_str());
                                let label = if is_current { format!("{} (current)", place.name) } else { place.name.clone() };
                                let weak = weak.clone();
                                sub = sub.item(PopupMenuItem::new(label).disabled(is_current).on_click(move |_, _, cx| {
                                    let place = place.clone();
                                    let _ = weak.update(cx, |this: &mut TaskPanel, cx| this.choose_migration(place, cx));
                                }));
                            }
                            sub
                        });
                        continue;
                    }
                    let weak = weak.clone();
                    popup = popup.item(PopupMenuItem::new(action.label()).on_click(move |_, _, cx| {
                        let _ = weak.update(cx, |this: &mut TaskPanel, cx| this.perform(action, cx));
                    }));
                }
                popup
            });
        }
        Some(recorded(split, label).into_any_element())
    }

    /// The runner line's question and its buttons ([`Prompt`]).
    fn render_prompt(&self, prompt: &Prompt, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let buttons = prompt.buttons().into_iter().enumerate().map(|(ix, label)| {
            let button = Button::new(SharedString::from(format!("unified-task-prompt-{ix}")))
                .label(label)
                .small()
                .on_click(cx.listener(move |this, _, _, cx| this.answer_prompt(label, cx)));
            // The first is what the question expects; Cancel is plain.
            if ix == 0 { button.primary() } else { button }
        }).collect::<Vec<_>>();
        div()
            .w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(div().flex_auto().min_w(gpui::px(160.)).child(selectable_text(
                "unified-task-prompt-text",
                prompt.text(),
                window,
                cx,
            )))
            .child(div().flex().flex_none().gap_2().children(buttons))
            .into_any_element()
    }

    /// T2: the runner line — lifecycle state, then what the runner is doing
    /// (`doc/ui/task-panel.md`, "Runner"), then the split button.
    fn render_runner_line(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (_, live) = self.runner_status(cx);
        // Who is next is the standing's (`tod_core::next_actor`), never
        // derived here; the runner's own status below only supplies the
        // controls and a live turn's activity.
        let standing = self.standing.clone();
        let done = standing.as_ref().is_some_and(|s| matches!(s.actor, NextActor::Done));
        // A pending migration is looked at again each second, since the
        // runner ending is what releases it.
        self.runner.ticking = (standing.is_some() && !done) || self.runner.migration_wait.is_some();
        let sep = || style::text_muted(div().flex_none()).child("·");

        // Status pieces wrap onto further lines when the panel is narrow;
        // long text keeps a readable minimum width instead of collapsing.
        let mut line = div().flex().flex_wrap().items_center().gap_x_2().gap_y_1().flex_auto().min_w(gpui::px(160.));
        if !done {
            line = line.child(self.render_phase_chip(cx));
        }
        if let Some(cloud) = &self.runner.cloud {
            let mut text = match &self.runner.cloud_sandbox {
                Some(sandbox) => format!("in the cloud (sandbox {sandbox})"),
                None => "in the cloud (its Files location records no sandbox)".to_string(),
            };
            // While the app is making the replacement the standing says
            // working; the stale "deleted, run it" note would contradict it.
            let replacing = standing.as_ref().is_some_and(|s| matches!(s.actor, NextActor::Working { .. }));
            if cloud.lost_at.is_some() && !replacing {
                // What woke it (a release, a merge) found nothing there: the
                // orchestrator marked it, and the app makes a new one at its
                // next sync, so a closed app leaves the node waiting for it.
                if tod_core::cloud_sync::sandbox_deleted_by_user(self.fleet.paths().root(), &self.node_id.to_string()) {
                    text.push_str(", its sandbox was deleted: Start makes a new one");
                } else {
                    text.push_str(", its sandbox is gone: the app makes a new one when it next syncs");
                }
            }
            line = line.child(sep()).child(style::text_muted(div().flex_auto().min_w(gpui::px(160.))).child(
                selectable_text("unified-task-runner-cloud", text, window, cx),
            ));
        } else if !self.runner.place_text.is_empty() {
            line = line.child(sep()).child(style::text_muted(div().flex_none()).child(self.runner.place_text.clone()).record_text(self.runner.place_text.clone()));
        }
        if let Some(standing) = &standing {
            for (ix, part) in standing::runner_parts(standing, now_ms()).into_iter().enumerate() {
                let el = div().flex_auto().min_w(gpui::px(60.));
                let (el, color) = match part.tone {
                    PartTone::Muted => (style::text_muted(el), style::color::text_muted()),
                    PartTone::Error => (style::text_error(el), style::color::danger()),
                };
                line = line.child(sep()).child(el.child(selectable_text_colored(
                    SharedString::from(format!("unified-task-runner-part-{ix}")),
                    part.text,
                    color,
                    window,
                    cx,
                )));
            }
        }
        // What a live turn is doing right now, beside "working".
        if let Some(LiveTurn { activity, tokens, .. }) = &live {
            if let Some(activity) = activity {
                line = line.child(sep()).child(style::text_muted(div().flex_auto().min_w(gpui::px(160.))).child(
                    selectable_text("unified-task-runner-activity", activity.clone(), window, cx),
                ));
            }
            if let Some(tokens) = tokens {
                line = line
                    .child(sep())
                    .child({
                        let t = format_tokens(*tokens);
                        style::text_muted(div().flex_none()).child(t.clone()).record_text(t)
                    });
            }
        }
        // Why the runner could not start, or the latest word from the cloud.
        let mut extra_note: Option<AnyElement> = None;
        // A start that failed off the UI thread outranks the note that said
        // it was starting; a failed note keeps its own error.
        let note = match (&self.runner.cloud_note, self.runners.read(cx).error(self.node_id)) {
            (Some(note), None) => Some((note.clone(), self.runner.cloud_failed)),
            (Some(note), Some(_)) if self.runner.cloud_failed => Some((note.clone(), true)),
            (Some(_), Some(error)) => Some((error.to_string(), true)),
            (None, Some(error)) => Some((error.to_string(), true)),
            (None, None) => None,
        };
        // Not being able to read where the node runs outranks nothing, but is
        // never left unsaid.
        let note = note.or_else(|| self.runner.load_error.clone().map(|error| (error, true)));
        if let Some((note, failed)) = note {
            // A multi-line note (a failure: its title, then each cause) is a
            // block of its own, a line per row, causes set back in muted
            // text; a one-line note stays on the runner line.
            if note.contains('\n') {
                let mut block = div().w_full().flex().flex_col().gap_1();
                for (i, text) in note.lines().filter(|l| !l.trim().is_empty()).enumerate() {
                    let id = SharedString::from(format!("unified-task-runner-note-{i}"));
                    let row = div().w_full().pl(gpui::px(if i == 0 { 0. } else { 12. }));
                    let row = match (failed, i) {
                        (true, 0) => style::text_error(row),
                        _ => style::text_muted(row),
                    };
                    block = block.child(row.child(selectable_text(id, text.to_string(), window, cx)));
                }
                extra_note = Some(block.into_any_element());
            } else {
                let el = div().flex_auto().min_w(gpui::px(160.));
                let el = if failed { style::text_error(el) } else { style::text_muted(el) };
                line = line.child(sep()).child(el.child(selectable_text("unified-task-runner-note", note, window, cx)));
            }
        }
        let problems = self.render_problems(window, cx);
        let actions = self.runner_actions(live.as_ref(), cx);
        let node = self.node_id;
        let watch = self.watch_target(cx).map(|_| {
            Button::new("unified-task-runner-watch")
                .label("Watch")
                .small()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open(PanelKind::Watch(node), false, cx)
                }))
        });
        let pausing = matches!(self.run_state(live.as_ref(), cx), RunState::Active { pausing: true });
        let button = self.render_runner_button(actions, pausing, cx);
        let prompt = self.runner.prompt.clone().map(|prompt| self.render_prompt(&prompt, window, cx));
        let mut buttons = div().flex().flex_none().items_center().gap_2().ml_auto();
        if let Some(defer) = self.render_defer_control(cx) {
            buttons = buttons.child(defer);
        }
        if let Some(watch) = watch {
            buttons = buttons.child(watch);
        }
        if let Some(button) = button {
            buttons = buttons.child(button);
        }
        let line = div()
            .id("unified-task-runner-line")
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_3()
            .gap_y_2()
            .child(line)
            .children(extra_note)
            .children(problems)
            .children(prompt)
            .child(buttons);
        Some(line.into_any_element())
    }

    /// The node's recorded background problems, one block each: what failed
    /// and why (selectable: it is data), then, for a job the daemon retries,
    /// that it will retry automatically, or that it needs the user.
    fn render_problems(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.runner.problems.is_empty() {
            return None;
        }
        let mut block = div().id("unified-task-problems").w_full().flex().flex_col().gap_1();
        for (ix, problem) in self.runner.problems.iter().enumerate() {
            let status = problem_status(problem);
            let message = div().w_full();
            let message = if problem.transient { style::text_muted(message) } else { style::text_error(message) };
            block = block.child(message.child(selectable_text(
                SharedString::from(format!("unified-task-problem-{ix}")),
                problem.message.clone(),
                window,
                cx,
            )));
            if let Some(status) = status {
                block = block.child(style::text_muted(div().w_full().pl(gpui::px(12.))).child(selectable_text(
                    SharedString::from(format!("unified-task-problem-status-{ix}")),
                    status,
                    window,
                    cx,
                )));
            }
        }
        Some(block.into_any_element())
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
                        .record_text(format!("Answered ({count})"))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_answered(cx))),
                )
                .when_some(entries, |el, entries| {
                    el.child(
                        scroll_with_bar(
                            "unified-task-answered-list",
                            &self.answered_scroll,
                            div().px_3().max_h(gpui::px(320.)),
                            div().px_3().child(entries).when(count == 0, |el| {
                                el.child(div().text_xs().text_color(muted).pb_2().child("No answers yet.").record_text("No answers yet."))
                            }),
                        )
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

}

impl EventEmitter<PanelOpenRequest> for TaskPanel {}

impl Focusable for TaskPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// `content` in a vertical scroll area with a scrollbar that appears only when
/// it overflows. `frame` carries the sizing (its max height or flex).
/// The state of a recorded problem, in words: that the daemon will retry it
/// automatically, or that it needs the user and what to do. `None` for a kind
/// of problem that has neither (it is only reported).
fn problem_status(problem: &tod_store::node_problems::NodeProblem) -> Option<String> {
    if problem.kind != tod_store::node_problems::KIND_SANDBOX_RETIRE {
        return None;
    }
    Some(if problem.transient {
        "Will retry automatically.".to_string()
    } else {
        "Needs you: Delete sandbox in this panel's menu tries again, or Delete sandbox without a checkpoint \
         deletes it and loses its uncommitted work."
            .to_string()
    })
}

fn scroll_with_bar(
    id: &'static str,
    handle: &ScrollHandle,
    frame: gpui::Div,
    content: impl IntoElement,
) -> AnyElement {
    frame
        .id(id)
        .relative()
        .w_full()
        .child(
            div()
                .id(SharedString::from(format!("{id}-scroll")))
                .size_full()
                .max_h_full()
                .overflow_y_scroll()
                .track_scroll(handle)
                .child(content),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .left_0()
                .child(Scrollbar::vertical(handle)),
        )
        .into_any_element()
}

impl Render for TaskPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_refresh {
            self.pending_refresh = false;
            self.header = load(&self.fleet, self.node_id);
            self.refresh_pr_states(cx);
        }
        self.drain_migration(window, cx);
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
            // Header: identity, artifacts, runner. It never takes more than
            // half the panel (a failed run's error can be pages long) and
            // scrolls past that.
            .child(scroll_with_bar(
                "unified-task-header",
                &self.header_scroll,
                div().flex_none().max_h_1_2(),
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .child(identity)
                    .child(artifacts)
                    .children(runner),
            ))
            // Requests scroll; the drawer stays at the bottom.
            .child(scroll_with_bar(
                "unified-task-requests",
                &self.requests_scroll,
                div().flex_1().min_h_0(),
                div().children(requests),
            ))
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

/// The split button's own face: the action it performs (none for a label
/// only), its words, and whether it is disabled. `None` when there is
/// nothing to show. A node with no default action (waiting on something
/// outside) still has a menu (Check now, Migrate, Reset environment), so its
/// button reads "Actions", disabled, and the caret opens the menu.
fn runner_button_face(actions: &RunnerActions, pausing: bool) -> Option<(Option<RunnerAction>, &'static str, bool)> {
    match (actions.busy, actions.default) {
        (Some(what), _) => Some((None, what, true)),
        (None, Some(action)) if action == RunnerAction::Pause && pausing => Some((Some(action), "Pausing…", true)),
        (None, Some(action)) => Some((Some(action), action.label(), false)),
        (None, None) if actions.menu.is_empty() => None,
        (None, None) => Some((None, "Actions", true)),
    }
}

#[cfg(test)]
mod face_tests {
    use super::*;

    #[test]
    fn a_node_with_a_menu_but_no_default_still_gets_a_button() {
        let waiting = RunnerActions {
            default: None,
            menu: vec![RunnerAction::CheckNow, RunnerAction::MigrateTo],
            busy: None,
        };
        assert_eq!(runner_button_face(&waiting, false), Some((None, "Actions", true)));
        let nothing = RunnerActions { default: None, menu: Vec::new(), busy: None };
        assert_eq!(runner_button_face(&nothing, false), None);
        let running = RunnerActions { default: Some(RunnerAction::Pause), menu: Vec::new(), busy: None };
        assert_eq!(runner_button_face(&running, true), Some((Some(RunnerAction::Pause), "Pausing…", true)));
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

    #[test]
    fn findings_label_shows_open_only_when_any() {
        let mut header = TaskHeader {
            findings_total: 3,
            ..Default::default()
        };
        assert_eq!(header.findings_label(), "Findings 3");
        header.findings_open = 2;
        assert_eq!(header.findings_label(), "Findings 3 · 2 open");
    }

    fn facts<'a>(supervised: bool, actor: Option<&'a NextActor>) -> RunFacts<'a> {
        RunFacts {
            done: false,
            supervised,
            runner_running: false,
            pausing: false,
            hand_turn: false,
            outcome: None,
            actor,
            pause_asked: false,
        }
    }

    fn you(why: YouReason) -> NextActor {
        NextActor::You { why, since: 1 }
    }

    #[test]
    fn the_run_state_follows_the_runner_here_and_the_standing_in_the_cloud() {
        use tod_core::next_actor::Work;
        let working = NextActor::Working { what: Work::Cloud, since: 1 };
        // Locally the runner's own state wins; a hand-started turn is its own state.
        let mut f = facts(false, Some(&working));
        f.runner_running = true;
        f.pausing = true;
        assert_eq!(derive_run_state(&f), RunState::Active { pausing: true });
        let mut f = facts(false, None);
        f.hand_turn = true;
        assert_eq!(derive_run_state(&f), RunState::HandTurn);
        // Supervised, a live heartbeat is an active run, and Pause stays asked.
        let mut f = facts(true, Some(&working));
        f.pause_asked = true;
        assert_eq!(derive_run_state(&f), RunState::Active { pausing: true });
        // A node worked by something this process has not heard of is not Start-able.
        assert_eq!(derive_run_state(&facts(false, Some(&working))), RunState::Ended(EndedHow::Waiting));
        // Ended: by how.
        let cases = [
            (you(YouReason::Paused), RunState::Ended(EndedHow::Paused)),
            (you(YouReason::Stopped { reason: "budget spent: 6 sessions".into() }), RunState::Ended(EndedHow::Budget)),
            (you(YouReason::Stopped { reason: "an agent is stuck".into() }), RunState::Ended(EndedHow::Stopped)),
            (you(YouReason::Failed { error: "x".into() }), RunState::Ended(EndedHow::Failed)),
            (you(YouReason::Request { count: 1 }), RunState::Ended(EndedHow::Request)),
            (you(YouReason::NotStarted), RunState::NeverRun),
            (NextActor::Done, RunState::Done),
        ];
        for (actor, expected) in cases {
            assert_eq!(derive_run_state(&facts(true, Some(&actor))), expected, "{actor:?}");
        }
        assert_eq!(derive_run_state(&facts(false, None)), RunState::NeverRun);
        // A local outcome beats the standing; a finished one says nothing.
        let spent = Outcome::BudgetExhausted { limit: tod_core::autopilot::BudgetLimit::Sessions { used: 6, elapsed_secs: 0 } };
        let mut f = facts(false, None);
        f.outcome = Some(&spent);
        assert_eq!(derive_run_state(&f), RunState::Ended(EndedHow::Budget));
        let mut f = facts(false, None);
        f.outcome = Some(&Outcome::Done);
        assert_eq!(derive_run_state(&f), RunState::NeverRun);
        let mut f = facts(false, None);
        f.done = true;
        assert_eq!(derive_run_state(&f), RunState::Done);
    }

    #[test]
    fn a_check_now_result_says_the_gate_where_it_went_and_an_unreadable_github() {
        use tod_core::autopilot::GithubCheck;
        use tod_core::lifecycle::CheckNow;
        use tod_core::phase::GateCheck;
        let gate = |to: Option<&str>| GateCheck { from: "pr_draft".into(), to: to.map(str::to_string), criteria: Vec::new() };
        let moved = CheckNow { gate: gate(Some("pr_review")), advanced: Some("pr_review"), github: None, held: None };
        assert_eq!(describe_check(&moved), ("Checked: the gate is clear; moved to pr_review".to_string(), false));
        let held = CheckNow {
            gate: gate(Some("pr_review")),
            advanced: None,
            github: Some(GithubCheck::Waiting { what: "a review".into(), due_at_ms: 0 }),
            held: Some("a review".into()),
        };
        let (text, failed) = describe_check(&held);
        assert!(!failed);
        assert!(text.contains("gate is clear, but the node was not moved on: a review"), "{text}");
        assert!(text.contains("GitHub: waiting for a review"), "{text}");
        let unreadable = CheckNow {
            gate: gate(Some("pr_review")),
            advanced: None,
            github: Some(GithubCheck::Unreadable("no network".into())),
            held: None,
        };
        let (text, failed) = describe_check(&unreadable);
        assert!(failed && text.contains("GitHub could not be read: no network"), "{text}");
    }

    /// A panel over a fixture node, with its runner line pointed at a node
    /// whose files are in a cloud sandbox, so its runs are the supervisor's.
    fn supervised_panel(
        cx: &mut gpui::TestAppContext,
    ) -> (crate::views::rows::fixture::Fixture, Entity<TaskPanel>, &mut gpui::VisualTestContext) {
        use crate::views::rows::fixture::Fixture;
        use gpui::AppContext as _;
        use std::cell::RefCell;
        use std::rc::Rc;

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
        view.update(cx, |view, _| {
            view.runner.place = Some(Place::Sandbox { exists: true });
            view.runner.supervised = true;
            view.runner.cloud = Some(CloudNode {
                user: "alice".into(),
                accepted_at_ms: 1,
                lost_at: None,
            });
        });
        (fixture, view, cx)
    }

    fn set_actor(view: &Entity<TaskPanel>, cx: &mut gpui::VisualTestContext, actor: NextActor) {
        let node = view.read_with(cx, |view, _| view.node_id);
        view.update(cx, |view, cx| {
            view.set_standing(Some(NodeStanding { node_id: node, actor, deferred: None }), cx)
        });
    }

    fn working() -> NextActor {
        NextActor::Working { what: tod_core::next_actor::Work::Cloud, since: 1 }
    }

    /// A background problem recorded on the node (the daemon's failed delete
    /// of its sandbox at `done`) shows in the task panel, with its error and
    /// "will retry automatically" while transient, or "needs you" with the
    /// menu entries when permanent; clearing it removes the block.
    #[gpui::test]
    fn a_recorded_node_problem_shows_with_its_state_and_clears(cx: &mut gpui::TestAppContext) {
        use tod_store::node_problems::{KIND_SANDBOX_RETIRE, NodeProblem};

        let (fixture, view, cx) = supervised_panel(cx);
        let node = fixture.node_id;
        let now = now_ms();
        let mut problem = NodeProblem::new(node, KIND_SANDBOX_RETIRE, "Deleting the sandbox s failed: blaxel said no".into(), now);
        problem.transient = true;
        tod_core::node_problems::record(&fixture.store, problem.clone()).unwrap();
        fixture.store.reload_if_stale().unwrap();
        view.update(cx, |view, cx| view.refresh_runner(cx));
        cx.run_until_parked();
        let shown = view.read_with(cx, |view, _| view.runner.problems.clone());
        assert_eq!(shown.len(), 1);
        assert!(shown[0].message.contains("blaxel said no"));
        let status = problem_status(&shown[0]).unwrap();
        assert_eq!(status, "Will retry automatically.");
        let rendered = cx.update(|window, cx| view.update(cx, |view, cx| view.render_problems(window, cx).is_some()));
        assert!(rendered, "the block is drawn");

        problem.transient = false;
        let status = problem_status(&problem).unwrap();
        assert!(status.starts_with("Needs you") && status.contains("Delete sandbox"), "{status}");
        // Another kind of problem has no status line of its own.
        assert_eq!(problem_status(&NodeProblem::new(node, tod_store::node_problems::KIND_ADOPT, "x".into(), now)), None);

        tod_core::node_problems::clear(&fixture.store, &node.to_string(), KIND_SANDBOX_RETIRE).unwrap();
        fixture.store.reload_if_stale().unwrap();
        view.update(cx, |view, cx| view.refresh_runner(cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.runner.problems.is_empty()));
        let rendered = cx.update(|window, cx| view.update(cx, |view, cx| view.render_problems(window, cx).is_some()));
        assert!(!rendered, "nothing recorded, nothing drawn");
    }

    /// A supervised node's menu follows the run's state: Resume after a
    /// stop, a fresh budget (and no plain Resume) after a spent budget, Pause
    /// while it works, and no default while it only waits.
    #[gpui::test]
    fn a_supervised_node_offers_resume_after_a_stop_and_a_fresh_budget_after_a_spent_one(cx: &mut gpui::TestAppContext) {
        let (_fixture, view, cx) = supervised_panel(cx);
        let menu = |cx: &mut gpui::VisualTestContext| view.read_with(cx, |view, cx| view.runner_actions(None, cx));

        set_actor(&view, cx, you(YouReason::Stopped { reason: "an agent is stuck".into() }));
        let actions = menu(cx);
        assert_eq!(actions.default, Some(RunnerAction::Resume));
        assert!(actions.menu.contains(&RunnerAction::ResumeFreshBudget) && actions.menu.contains(&RunnerAction::RunFresh));
        assert!(actions.menu.contains(&RunnerAction::SyncNow) && actions.menu.contains(&RunnerAction::DeleteSandbox));

        set_actor(&view, cx, you(YouReason::Stopped { reason: "you answered the question about its budget".into() }));
        let actions = menu(cx);
        assert_eq!(actions.default, Some(RunnerAction::ResumeFreshBudget));
        assert!(!actions.menu.contains(&RunnerAction::Resume));

        set_actor(&view, cx, working());
        let actions = menu(cx);
        assert_eq!(actions.default, Some(RunnerAction::Pause));
        assert!(actions.menu.contains(&RunnerAction::StopNow));
        assert!(!actions.menu.contains(&RunnerAction::CheckNow), "Check now is hidden while it runs");

        // Done after waiting on someone: no default (the owner's decision for
        // a run that ended only to wait), but Check now and Migrate remain.
        set_actor(&view, cx, NextActor::Others {
            wait: tod_core::next_actor::ExternalWait {
                kind: tod_core::next_actor::WaitKind::HumanReview,
                who: None,
                detail: None,
                since: 1,
                next_check_ms: None,
                overdue: false,
                retry: None,
            },
        });
        let actions = menu(cx);
        assert_eq!(actions.default, None);
        assert!(actions.menu.contains(&RunnerAction::CheckNow) && actions.menu.contains(&RunnerAction::MigrateTo));
    }

    fn elsewhere() -> tod_store::fleet::FilesPlace {
        tod_store::fleet::FilesPlace { id: "elsewhere-id".into(), name: "elsewhere".into(), ..Default::default() }
    }

    /// Migrate while the runner is active asks first, and migrates (calls
    /// `confirm_migrate`) only after the run has ended.
    #[gpui::test]
    fn migrate_while_running_asks_first_and_migrates_only_after_the_run_ends(cx: &mut gpui::TestAppContext) {
        let (_fixture, view, cx) = supervised_panel(cx);
        set_actor(&view, cx, working());
        view.update(cx, |view, cx| view.choose_migration(elsewhere(), cx));
        view.read_with(cx, |view, _| {
            assert_eq!(view.runner.prompt, Some(Prompt::MigrateRunning { place: elsewhere(), can_pause: true }));
            assert!(view.runner.migration_wait.is_none(), "nothing is pending until it is answered");
        });
        assert_eq!(
            Prompt::MigrateRunning { place: elsewhere(), can_pause: true }.buttons(),
            ["Pause", "Stop now", "Cancel"]
        );
        // Cancel: the question goes and nothing is pending.
        view.update(cx, |view, cx| view.answer_prompt("Cancel", cx));
        view.read_with(cx, |view, _| {
            assert!(view.runner.prompt.is_none() && view.runner.migration_wait.is_none());
            assert!(view.runner.cloud_busy.is_none() && view.runner.cloud_note.is_none());
        });

        // Answered (Pause or Stop now) while the run is still going: it waits.
        view.update(cx, |view, _| view.runner.migration_wait = Some(elsewhere()));
        view.update_in(cx, |view, window, cx| view.drain_migration(window, cx));
        view.read_with(cx, |view, _| {
            assert_eq!(view.runner.migration_wait, Some(elsewhere()), "the run has not ended");
            assert!(view.runner.cloud_note.is_none(), "nothing was migrated");
        });
        // The run ended: the migration begins (the location is not in this
        // store, so the store's own refusal comes back on the line).
        set_actor(&view, cx, you(YouReason::Paused));
        view.update_in(cx, |view, window, cx| view.drain_migration(window, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.runner.migration_wait.is_none());
            let note = view.runner.cloud_note.clone().unwrap_or_default();
            assert!(note.contains("Could not migrate to elsewhere"), "{note}");
            assert!(view.runner.cloud_failed);
        });
    }

    /// A stand-in for the supervisor round trip: delivers its answer on a
    /// later turn of the event loop, as the real job's updates arrive.
    fn answer_later(panel: &mut TaskPanel, update: CloudUpdate, cx: &mut Context<TaskPanel>) {
        let node = panel.node_id;
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |this: &mut TaskPanel, cx| this.on_cloud_update(node, update, cx));
        })
        .detach();
    }

    fn supervisor_accepts(panel: &mut TaskPanel, kind: &'static str, cx: &mut Context<TaskPanel>) {
        answer_later(panel, CloudUpdate::Synced(format!("asked {kind}")), cx);
    }

    fn supervisor_still_waking(panel: &mut TaskPanel, kind: &'static str, cx: &mut Context<TaskPanel>) {
        answer_later(
            panel,
            CloudUpdate::Failed(format!("{kind} failed\nsandbox sb is still waking after 90s")),
            cx,
        );
    }

    /// Makes the fixture node's Files run in a cloud sandbox in the store
    /// (not only on the panel), so a refresh of the runner line keeps it
    /// supervised.
    fn put_files_in_a_sandbox(
        fixture: &crate::views::rows::fixture::Fixture,
        view: &Entity<TaskPanel>,
        cx: &mut gpui::VisualTestContext,
    ) {
        use tod_store::fleet::{DevContainerSetting, FilesPlace, FleetMutation};
        use tod_store::outline::{Capability, OutlineMutation};
        fixture
            .store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: fixture.node_id,
                capabilities: vec![Capability::Files],
            })
            .unwrap();
        let place = FilesPlace {
            id: Uuid::new_v4().to_string(),
            name: "cloud".into(),
            repo: Some("/src/app".into()),
            dev_container: Some(DevContainerSetting { sandbox: true, ..Default::default() }),
            ..Default::default()
        };
        let id = place.id.clone();
        fixture.store.enqueue(FleetMutation::SaveFilesPlace { place }).unwrap();
        fixture
            .store
            .enqueue(FleetMutation::SetNodeFilesPlace { node_id: fixture.node_id.to_string(), place_id: Some(id) })
            .unwrap();
        fixture.store.writer().flush().unwrap();
        fixture.store.reload_if_stale().unwrap();
        view.update(cx, |view, cx| view.refresh_runner(cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(view.runner.supervised, "the store says it runs in the cloud"));
    }

    /// Each answer of the Migrate-while-running question leads to its job:
    /// Pause asks the supervisor to pause and Stop now to stop now, each
    /// leaving the migration pending until the run has ended, when it begins;
    /// Cancel asks nothing and changes nothing.
    #[gpui::test]
    fn each_answer_of_migrate_while_running_leads_to_its_own_job(cx: &mut gpui::TestAppContext) {
        use tod_core::cloud_sync::{REQUEST_PAUSE, REQUEST_STOP_NOW};
        for (label, kind) in [("Pause", REQUEST_PAUSE), ("Stop now", REQUEST_STOP_NOW)] {
            let (fixture, view, cx) = supervised_panel(cx);
            put_files_in_a_sandbox(&fixture, &view, cx);
            view.update(cx, |view, _| view.stop_job = supervisor_accepts);
            set_actor(&view, cx, working());
            view.update(cx, |view, cx| view.choose_migration(elsewhere(), cx));
            view.update(cx, |view, cx| view.answer_prompt(label, cx));
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert_eq!(view.runner.cloud_note.as_deref(), Some(format!("asked {kind}").as_str()), "{label}");
                assert!(!view.runner.cloud_failed);
                assert_eq!(view.runner.migration_wait, Some(elsewhere()), "{label}: the migration waits for the run");
                assert_eq!(view.runner.pause_asked, kind == REQUEST_PAUSE, "{label}");
            });
            // Still running: nothing migrates yet.
            view.update_in(cx, |view, window, cx| view.drain_migration(window, cx));
            view.read_with(cx, |view, _| assert_eq!(view.runner.migration_wait, Some(elsewhere())));
            // The supervisor stopped: the migration begins (this store has no
            // such location, so its own refusal comes back on the line).
            set_actor(&view, cx, you(YouReason::Paused));
            view.update_in(cx, |view, window, cx| view.drain_migration(window, cx));
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert!(view.runner.migration_wait.is_none() && !view.runner.pause_asked, "{label}");
                let note = view.runner.cloud_note.clone().unwrap_or_default();
                assert!(note.contains("Could not migrate to elsewhere"), "{label}: {note}");
            });
        }

        // Cancel asks the supervisor nothing.
        let (_fixture, view, cx) = supervised_panel(cx);
        view.update(cx, |view, _| view.stop_job = supervisor_accepts);
        set_actor(&view, cx, working());
        view.update(cx, |view, cx| view.choose_migration(elsewhere(), cx));
        view.update(cx, |view, cx| view.answer_prompt("Cancel", cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.runner.cloud_note.is_none(), "no job ran: {:?}", view.runner.cloud_note);
            assert!(!view.runner.pause_asked && view.runner.migration_wait.is_none() && view.runner.prompt.is_none());
        });
    }

    /// A Pause or Stop now that fails (a sandbox still waking after 90
    /// seconds) is not pending, and neither is the migration queued behind it:
    /// the failure is on the line, and nothing waits forever.
    #[gpui::test]
    fn a_failed_pause_or_stop_now_cancels_the_migration_loudly(cx: &mut gpui::TestAppContext) {
        for label in ["Pause", "Stop now"] {
            let (_fixture, view, cx) = supervised_panel(cx);
            view.update(cx, |view, _| view.stop_job = supervisor_still_waking);
            set_actor(&view, cx, working());
            view.update(cx, |view, cx| view.choose_migration(elsewhere(), cx));
            view.update(cx, |view, cx| view.answer_prompt(label, cx));
            // Until the answer comes the migration is queued behind the job.
            view.read_with(cx, |view, _| {
                assert_eq!(view.runner.migration_wait, Some(elsewhere()), "{label}");
                assert!(view.runner.cloud_busy.is_some(), "{label}");
            });
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert!(view.runner.cloud_failed, "{label}");
                let note = view.runner.cloud_note.clone().unwrap_or_default();
                assert!(note.contains("still waking after 90s"), "{label}: {note}");
                assert!(view.runner.cloud_busy.is_none(), "{label}");
                assert!(!view.runner.pause_asked, "{label}: Pause is not pending");
                assert!(view.runner.migration_wait.is_none(), "{label}: the migration is cancelled");
            });
            // Even once the run has ended, nothing migrates by itself.
            set_actor(&view, cx, you(YouReason::Paused));
            view.update_in(cx, |view, window, cx| view.drain_migration(window, cx));
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                let note = view.runner.cloud_note.clone().unwrap_or_default();
                assert!(note.contains("still waking"), "{label}: the failure stays: {note}");
            });
        }
    }

    /// A node with children is refused with exactly the store's sentence,
    /// shown on the runner line when the migration begins (the menu does not
    /// offer Migrate for such a node: `runner_menu`'s own test).
    #[gpui::test]
    fn migrating_a_node_with_children_is_refused_with_the_stores_sentence(cx: &mut gpui::TestAppContext) {
        use tod_store::outline::{CreatePosition, OutlineMutation};
        let (fixture, view, cx) = supervised_panel(cx);
        let list_id = fixture.store.list_outline_lists().unwrap()[0].id;
        fixture
            .store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(Uuid::new_v4()),
                list_id,
                parent_id: Some(fixture.node_id),
                anchor_id: None,
                position: CreatePosition::Below,
                title: "child".into(),
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();
        let place = tod_store::fleet::places::save(
            &fixture.store,
            &tod_store::fleet::FilesPlace {
                name: "elsewhere".into(),
                repo: Some("/src/elsewhere".into()),
                use_worktree: true,
                ..Default::default()
            },
        )
        .unwrap();
        view.update(cx, |view, _| view.runner.migration_wait = Some(place));
        set_actor(&view, cx, you(YouReason::Paused));
        view.update_in(cx, |view, window, cx| view.drain_migration(window, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.runner.cloud_note.as_deref(),
                Some("Could not migrate to elsewhere: You cannot migrate a node that has children.")
            );
            assert!(view.runner.cloud_failed && view.runner.migration_wait.is_none());
        });
    }

    /// Delete sandbox while the runner is active asks "Stop it and delete its
    /// sandbox?" first; Cancel changes nothing.
    #[gpui::test]
    fn delete_sandbox_while_running_shows_the_confirmation_and_cancel_changes_nothing(cx: &mut gpui::TestAppContext) {
        let (_fixture, view, cx) = supervised_panel(cx);
        set_actor(&view, cx, working());
        for (action, checkpoint) in [(RunnerAction::DeleteSandbox, true), (RunnerAction::DeleteSandboxAnyway, false)] {
            view.update(cx, |view, cx| view.perform(action, cx));
            view.read_with(cx, |view, _| {
                assert_eq!(view.runner.prompt, Some(Prompt::DeleteRunning { checkpoint }));
                assert!(view.runner.cloud_busy.is_none(), "nothing runs until it is confirmed");
                assert!(view.runner.cloud_note.is_none());
            });
            view.update(cx, |view, cx| view.answer_prompt("Cancel", cx));
            view.read_with(cx, |view, _| {
                assert!(view.runner.prompt.is_none());
                assert!(view.runner.cloud_busy.is_none() && view.runner.cloud_note.is_none());
                assert!(view.runner.cloud.is_some() && view.runner.migration_wait.is_none());
            });
        }
        let prompt = Prompt::DeleteRunning { checkpoint: true };
        assert_eq!(prompt.text(), "This node is running. Stop it and delete its sandbox?");
        assert_eq!(prompt.buttons(), ["Stop and delete", "Cancel"]);
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
        assert_eq!(status, None);
        // No runner yet: Start, with the cloud in the menu.
        let actions = view.read_with(cx, |view, cx| view.runner_actions(status.as_ref(), cx));
        assert_eq!(actions.default, Some(RunnerAction::Start));
        assert_eq!(actions.busy, None);
        assert!(actions.menu.contains(&RunnerAction::RunFresh), "{:?}", actions.menu);
        assert!(actions.menu.contains(&RunnerAction::CheckNow), "{:?}", actions.menu);
        assert!(!actions.menu.contains(&RunnerAction::ResetEnvironment), "a host directory has no environment to reset");

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
            Some(LiveTurn { activity, since, .. }) => {
                assert!(activity.is_some());
                assert!(since.is_some());
            }
            None => panic!("expected a live turn"),
        }
        // A conversation run by hand: the button stops it, no runner to start.
        let actions = view.read_with(cx, |view, cx| {
            let (_, status) = view.runner_status(cx);
            view.runner_actions(status.as_ref(), cx)
        });
        assert_eq!(actions.default, Some(RunnerAction::StopNow));
        assert!(!actions.menu.contains(&RunnerAction::Pause) && !actions.menu.contains(&RunnerAction::CheckNow));

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
        // Times are in milliseconds: without a gap the two can tie, and the
        // order of a tie is not the one this test is about.
        std::thread::sleep(std::time::Duration::from_millis(5));

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
