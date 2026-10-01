//! Every task's runner on this machine, shown and driven from the task
//! panel's runner line (`doc/ui/task-panel.md`, "Runner").
//!
//! The runs themselves are the daemon's (`tod_agentd::runners`), so they go
//! on when the app is closed: this keeps the state the daemon pushes for each
//! node ([`RunnerState`]) and sends it a start, a pause or a stop. Without a
//! daemon (a store opened in this process) the same [`Runners`] run in the
//! app. One [`NodeRunners`] is made by the workbench and shared with its task
//! panels. The conversation a run is in is shown in [`AgentRuns`] as hosted
//! elsewhere, so the tree's status labels and the close-window warning see it
//! and nothing else sends to it.
//!
//! - **After a restart.** The daemon starts again a run it was stopped on and
//!   wakes a run that ended to wait, with no app open; the app learns every
//!   node's state when it subscribes.
//! - **Requests.** A run that stopped for a request (a decision, a handed
//!   back step, a gate criterion) continues by itself once the user has
//!   answered: the daemon sees the node waiting, then no longer waiting.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Context, Entity, Task};
use tod_agentd::runners::Runners;
use tod_agentd_client::remote::DaemonWriter;
use tod_core::autopilot::Outcome;
use tod_core::autopilot::local::{Live, RunnerState};
use tod_core::conversation::ConversationStatus;
use tod_core::runner_status::Runner;
use tod_store::conversation::Focus;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::interview::agent::SharedAgent;
use crate::ui::agent_runs::AgentRuns;

/// One node's runner.
#[derive(Default)]
struct NodeRunner {
    /// A run is in progress.
    running: bool,
    /// A pause or a stop was asked for and the run has not ended yet.
    pausing: bool,
    /// What it is doing (while `running`).
    live: Live,
    /// When the run in progress started (ms since the epoch).
    since: i64,
    /// How the last run ended.
    outcome: Option<Outcome>,
    /// The [`AgentRuns`] slot showing the run's conversation.
    slot: Option<u64>,
}

/// Where the runs are: the daemon, or (a store opened in this process) here.
enum Host {
    Daemon(Arc<DaemonWriter>),
    Local(Runners),
}

impl Host {
    fn start(&self, node: Uuid, renew_budget: bool) -> Result<(), String> {
        match self {
            Host::Daemon(daemon) => daemon.runner_start(node, renew_budget).map_err(|e| format!("{e:#}")),
            Host::Local(runners) => runners.start(node, renew_budget).map_err(|e| format!("{e:#}")),
        }
    }

    fn pause(&self, node: Uuid) {
        match self {
            Host::Daemon(daemon) => {
                let _ = daemon.runner_pause(node);
            }
            Host::Local(runners) => runners.pause(node),
        }
    }

    fn stop_now(&self, node: Uuid) {
        match self {
            Host::Daemon(daemon) => {
                let _ = daemon.runner_stop(node);
            }
            Host::Local(runners) => runners.stop_now(node),
        }
    }

    /// Every node's state now, then each change.
    fn states(&self) -> std::sync::mpsc::Receiver<RunnerState> {
        let (tx, rx) = std::sync::mpsc::channel();
        let events = match self {
            Host::Daemon(_) => tod_agentd_client::remote::events(),
            Host::Local(runners) => runners.subscribe(),
        };
        std::thread::spawn(move || {
            while let Ok(event) = events.recv() {
                if let tod_agentd_client::Event::Runner { state } = event {
                    if let Ok(state) = serde_json::from_value(state) {
                        if tx.send(state).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        rx
    }
}

pub struct NodeRunners {
    host: Arc<Host>,
    agent_runs: Entity<AgentRuns>,
    runners: HashMap<Uuid, NodeRunner>,
    /// The last error starting a run, per node, until the next start.
    errors: HashMap<Uuid, String>,
    _states: Task<()>,
}

impl NodeRunners {
    pub fn new(fleet: Arc<FleetStore>, agent: SharedAgent, agent_runs: Entity<AgentRuns>, cx: &mut Context<Self>) -> Self {
        let host = Arc::new(if fleet.is_client() {
            Host::Daemon(Arc::new(DaemonWriter::new(fleet.paths().root(), None)))
        } else {
            let runners = Runners::new(fleet, agent);
            runners.resume_saved();
            Host::Local(runners)
        });
        let (tx, rx) = async_channel::unbounded();
        let states = host.states();
        std::thread::spawn(move || {
            while let Ok(state) = states.recv() {
                if tx.send_blocking(state).is_err() {
                    break;
                }
            }
        });
        let _states = cx.spawn(async move |this, cx| {
            while let Ok(state) = rx.recv().await {
                if this.update(cx, |this, cx| this.on_state(state, cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            host,
            agent_runs,
            runners: HashMap::new(),
            errors: HashMap::new(),
            _states,
        }
    }

    /// The node's runner, for [`tod_core::runner_status::RunnerStatus::with_runner`].
    pub fn runner(&self, node: Uuid) -> Option<Runner<'_>> {
        let runner = self.runners.get(&node)?;
        if runner.running {
            return Some(Runner::Running {
                live: &runner.live,
                since: runner.since,
            });
        }
        runner.outcome.as_ref().map(Runner::Ended)
    }

    /// Every node with a run in progress.
    pub fn running_nodes(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.runners.iter().filter(|(_, r)| r.running).map(|(n, _)| *n)
    }

    pub fn is_running(&self, node: Uuid) -> bool {
        self.runners.get(&node).is_some_and(|r| r.running)
    }

    /// A pause was asked for and the run has not stopped yet.
    pub fn is_pausing(&self, node: Uuid) -> bool {
        self.runners.get(&node).is_some_and(|r| r.running && r.pausing)
    }

    /// How the node's last run ended, when it has ended.
    pub fn outcome(&self, node: Uuid) -> Option<&Outcome> {
        self.runners
            .get(&node)
            .filter(|r| !r.running)
            .and_then(|r| r.outcome.as_ref())
    }

    /// Why the node's runner could not be started, if it could not.
    pub fn error(&self, node: Uuid) -> Option<&str> {
        self.errors.get(&node).map(String::as_str)
    }

    /// Start `node`'s runner, or continue its last run; `renew_budget` gives
    /// the run a fresh budget first. Refused while one of the app's own
    /// conversations is working on the node. The host starts it off this
    /// thread; a failure comes back as [`Self::error`].
    pub fn start(&mut self, node: Uuid, renew_budget: bool, cx: &mut Context<Self>) -> Result<(), String> {
        if self.is_running(node) {
            return Ok(());
        }
        if self.agent_runs.read(cx).driving_on_node(node) {
            let err = "An agent is already working on this task; start the runner once it is done.".to_string();
            self.errors.insert(node, err.clone());
            cx.notify();
            return Err(err);
        }
        self.errors.remove(&node);
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { host.start(node, renew_budget) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(err) = result {
                    this.errors.insert(node, err);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
        Ok(())
    }

    /// Stop once the agent's turn ends.
    pub fn pause(&mut self, node: Uuid, cx: &mut Context<Self>) {
        if let Some(runner) = self.runners.get_mut(&node).filter(|r| r.running) {
            runner.pausing = true;
            let host = self.host.clone();
            cx.background_executor().spawn(async move { host.pause(node) }).detach();
            cx.notify();
        }
    }

    /// Cancel the turn in flight and stop.
    pub fn stop_now(&mut self, node: Uuid, cx: &mut Context<Self>) {
        if let Some(runner) = self.runners.get_mut(&node).filter(|r| r.running) {
            runner.pausing = true;
            let host = self.host.clone();
            cx.background_executor().spawn(async move { host.stop_now(node) }).detach();
            cx.notify();
        }
    }

    /// What the host pushed about a node's run.
    fn on_state(&mut self, state: RunnerState, cx: &mut Context<Self>) {
        let node = state.node;
        let runner = self.runners.entry(node).or_default();
        runner.running = state.running;
        runner.pausing = state.pausing;
        runner.live = state.live;
        runner.since = if state.running { state.since } else { runner.since };
        runner.outcome = state.outcome;
        self.show_conversation(node, cx);
        cx.notify();
    }

    /// Keep the run's conversation, and only it, shown in [`AgentRuns`].
    fn show_conversation(&mut self, node: Uuid, cx: &mut Context<Self>) {
        let Some(runner) = self.runners.get_mut(&node) else {
            return;
        };
        let wanted = match (runner.running, runner.live.protocol, runner.live.conversation_id) {
            (true, Some(protocol), Some(conversation_id)) => Some((protocol, conversation_id)),
            _ => None,
        };
        // Shown as working for as long as the run is in it, between turns
        // too, so nothing else sends to it meanwhile.
        let status = ConversationStatus {
            running: true,
            ..runner.live.status.clone()
        };
        let slot = runner.slot;
        let slot = self.agent_runs.update(cx, |runs, cx| {
            cx.notify();
            let current = slot.and_then(|id| runs.slot_by_id(id)).and_then(|s| s.conversation_id);
            match wanted {
                Some((protocol, conversation_id)) => {
                    if let Some(id) = slot
                        && current == Some(conversation_id)
                        && runs.update_elsewhere(id, status.clone())
                    {
                        return Some(id);
                    }
                    if let Some(id) = slot {
                        runs.release_elsewhere(id);
                    }
                    runs.host_elsewhere(Focus::Node(node), protocol, conversation_id, status)
                }
                None => {
                    if let Some(id) = slot {
                        runs.release_elsewhere(id);
                    }
                    None
                }
            }
        });
        if let Some(runner) = self.runners.get_mut(&node) {
            runner.slot = slot;
        }
    }
}
