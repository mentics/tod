//! Every task's runner on this machine: the node's autopilot
//! (`tod_core::autopilot::local`), started, paused, and stopped from the
//! task panel's runner line (`doc/ui/task-panel.md`, "Runner").
//!
//! One [`NodeRunners`] is made by the workbench and shared with its task
//! panels. Each run is on a thread of its own; its events come back here
//! and are kept per node for the runner line. The conversation a run is in
//! is shown in [`AgentRuns`] as hosted elsewhere, so the tree's status
//! labels and the close-window warning see it and nothing else sends to it.
//!
//! - **After a restart.** A run the app was closed on has no outcome in its
//!   saved state; it is started again when the app opens, and reopens the
//!   conversation it was in. Every other saved run is only read, for what
//!   the runner line says about it.
//! - **Requests.** A run that stopped for a request (a decision, a handed
//!   back step, a gate criterion) continues by itself once the user has
//!   answered: [`NodeRunners::on_attention`] sees the node waiting, then no
//!   longer waiting, with nothing of the app's own running on it.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Context, Entity, Task};
use tod_core::attention::NodeAttention;
use tod_core::autopilot::local::{LocalEvent, LocalRun, Live, Request};
use tod_core::autopilot::{AutopilotState, Budget, Outcome};
use tod_core::conversation::{ConversationConfig, ConversationStatus};
use tod_core::runner_status::Runner;
use tod_store::conversation::Focus;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::interview::agent::SharedAgent;
use crate::interview::{TodPaths, TodSettings};
use crate::ui::agent_runs::AgentRuns;

/// One node's runner.
#[derive(Default)]
struct NodeRunner {
    /// The run in progress.
    run: Option<LocalRun>,
    /// What it is doing (while `run` is set).
    live: Live,
    /// When the run in progress started (ms since the epoch).
    since: i64,
    /// How the last run ended, from this session or its saved state.
    outcome: Option<Outcome>,
    /// The [`AgentRuns`] slot showing the run's conversation.
    slot: Option<u64>,
    /// The node was seen waiting on the user since the run stopped for a
    /// request: once it no longer is, the run continues.
    seen_waiting: bool,
}

pub struct NodeRunners {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    agent_runs: Entity<AgentRuns>,
    runners: HashMap<Uuid, NodeRunner>,
    /// The last error starting a run, per node, until the next start.
    errors: HashMap<Uuid, String>,
    _load: Task<()>,
}


fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Every saved run under the data root.
fn load_saved(data_root: &std::path::Path) -> Vec<(Uuid, AutopilotState)> {
    let Ok(entries) = std::fs::read_dir(data_root.join("autopilot")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                return None;
            }
            let node: Uuid = path.file_stem()?.to_str()?.parse().ok()?;
            let state = AutopilotState::load(data_root, node).ok()?;
            Some((node, state))
        })
        .collect()
}

impl NodeRunners {
    pub fn new(fleet: Arc<FleetStore>, agent: SharedAgent, agent_runs: Entity<AgentRuns>, cx: &mut Context<Self>) -> Self {
        let root = fleet.paths().root().to_path_buf();
        let _load = cx.spawn(async move |this, cx| {
            let saved = cx.background_executor().spawn(async move { load_saved(&root) }).await;
            let _ = this.update(cx, |this, cx| {
                for (node, state) in saved {
                    if this.runners.get(&node).is_some_and(|r| r.run.is_some()) {
                        continue;
                    }
                    match state.outcome {
                        // The app was closed while it ran.
                        None => {
                            tracing::info!(%node, "resuming the runner the app was closed on");
                            let _ = this.start(node, false, cx);
                        }
                        Some(outcome) => this.runners.entry(node).or_default().outcome = Some(outcome),
                    }
                }
                cx.notify();
            });
        });
        Self {
            fleet,
            agent,
            agent_runs,
            runners: HashMap::new(),
            errors: HashMap::new(),
            _load,
        }
    }

    /// The node's runner, for [`tod_core::runner_status::RunnerStatus::with_runner`].
    pub fn runner(&self, node: Uuid) -> Option<Runner<'_>> {
        let runner = self.runners.get(&node)?;
        if runner.run.is_some() {
            return Some(Runner::Running {
                live: &runner.live,
                since: runner.since,
            });
        }
        runner.outcome.as_ref().map(Runner::Ended)
    }

    pub fn is_running(&self, node: Uuid) -> bool {
        self.runners.get(&node).is_some_and(|r| r.run.is_some())
    }

    /// A pause was asked for and the run has not stopped yet.
    pub fn is_pausing(&self, node: Uuid) -> bool {
        self.runners
            .get(&node)
            .and_then(|r| r.run.as_ref())
            .is_some_and(|run| run.requested() != Request::Run)
    }

    /// How the node's last run ended, when it has ended.
    pub fn outcome(&self, node: Uuid) -> Option<&Outcome> {
        self.runners
            .get(&node)
            .filter(|r| r.run.is_none())
            .and_then(|r| r.outcome.as_ref())
    }

    /// Why the node's runner could not be started, if it could not.
    pub fn error(&self, node: Uuid) -> Option<&str> {
        self.errors.get(&node).map(String::as_str)
    }

    fn config(&self) -> Result<ConversationConfig, String> {
        let paths = TodPaths::discover().map_err(|e| format!("{e:#}"))?;
        let settings = TodSettings::load(&paths).unwrap_or_default();
        let media = tod_core::media::MediaPaths::discover().map_err(|e| format!("Media bundle: {e}"))?;
        Ok(ConversationConfig {
            data_root: self.fleet.paths().root().to_path_buf(),
            media,
            launch: settings.launch_options_for(tod_store::AgentRole::Default),
            settings_path: Some(paths.settings_path()),
            context: settings.interview_context.clone(),
        })
    }

    /// Start `node`'s runner, or continue its last run; `renew_budget` gives
    /// the run a fresh budget first. Refused while one of the app's own
    /// conversations is working on the node.
    pub fn start(&mut self, node: Uuid, renew_budget: bool, cx: &mut Context<Self>) -> Result<(), String> {
        let result = self.try_start(node, renew_budget, cx);
        match &result {
            Ok(()) => self.errors.remove(&node),
            Err(err) => self.errors.insert(node, err.clone()),
        };
        cx.notify();
        result
    }

    fn try_start(&mut self, node: Uuid, renew_budget: bool, cx: &mut Context<Self>) -> Result<(), String> {
        if self.is_running(node) {
            return Ok(());
        }
        if self.agent_runs.read(cx).driving_on_node(node) {
            return Err("An agent is already working on this task; start the runner once it is done.".into());
        }
        let config = self.config()?;
        let (tx, rx) = async_channel::unbounded();
        let run = LocalRun::start(
            self.fleet.clone(),
            self.agent.clone(),
            config,
            node,
            Budget::default(),
            renew_budget,
            move |event| {
                let _ = tx.send_blocking(event);
            },
        )
        .map_err(|e| format!("Starting the runner failed: {e:#}"))?;
        let runner = self.runners.entry(node).or_default();
        runner.run = Some(run);
        runner.live = Live::default();
        runner.since = now_ms();
        runner.outcome = None;
        runner.seen_waiting = false;
        cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                if this.update(cx, |this, cx| this.on_event(node, event, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        Ok(())
    }

    /// Stop once the agent's turn ends.
    pub fn pause(&mut self, node: Uuid, cx: &mut Context<Self>) {
        if let Some(run) = self.runners.get(&node).and_then(|r| r.run.as_ref()) {
            run.pause();
            cx.notify();
        }
    }

    /// Cancel the turn in flight and stop.
    pub fn stop_now(&mut self, node: Uuid, cx: &mut Context<Self>) {
        if let Some(run) = self.runners.get(&node).and_then(|r| r.run.as_ref()) {
            run.stop_now();
            cx.notify();
        }
    }

    fn on_event(&mut self, node: Uuid, event: LocalEvent, cx: &mut Context<Self>) {
        let Some(runner) = self.runners.get_mut(&node) else {
            return;
        };
        match event {
            LocalEvent::Live(live) => {
                runner.live = live;
                self.show_conversation(node, cx);
            }
            LocalEvent::Finished(result) => {
                runner.run = None;
                runner.live = Live::default();
                runner.outcome = Some(match result {
                    Ok(outcome) => outcome,
                    Err(error) => Outcome::Stopped {
                        reason: format!("{}{error}", tod_core::autopilot::local::FAILED_PREFIX),
                    },
                });
                self.show_conversation(node, cx);
            }
        }
        cx.notify();
    }

    /// Keep the run's conversation, and only it, shown in [`AgentRuns`].
    fn show_conversation(&mut self, node: Uuid, cx: &mut Context<Self>) {
        let Some(runner) = self.runners.get_mut(&node) else {
            return;
        };
        let wanted = match (runner.run.is_some(), runner.live.protocol, runner.live.conversation_id) {
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

    /// What every node is waiting on the user for, after each change: a run
    /// that stopped for a request continues once it has been answered.
    pub fn on_attention(&mut self, attention: &HashMap<Uuid, NodeAttention>, cx: &mut Context<Self>) {
        let mut resume = Vec::new();
        for (node, runner) in &mut self.runners {
            let Some(Outcome::NeedsHuman { reason }) = &runner.outcome else {
                continue;
            };
            if runner.run.is_some() || !reason.is_request() {
                continue;
            }
            let waiting = attention.get(node).is_some_and(|a| a.waiting_since.is_some());
            if waiting {
                runner.seen_waiting = true;
            } else if runner.seen_waiting && !self.agent_runs.read(cx).driving_on_node(*node) {
                resume.push(*node);
            }
        }
        for node in resume {
            tracing::info!(%node, "the runner's request was answered; continuing");
            let _ = self.start(node, false, cx);
        }
    }
}
