//! Every node's autopilot run on this machine, hosted by the daemon so that a
//! run outlives the app (`doc/agentd.md`, "Desired state and the lease").
//!
//! This is what the app's `NodeRunners` used to do in its own process: each
//! run is a [`LocalRun`] on a thread of its own, its state is kept per node,
//! and a run that ended to wait (for a review) or for a request the user has
//! since answered is started again by the daemon. The app only shows the state
//! it is pushed ([`Event::Runner`]) and asks for a start, a pause or a stop.
//!
//! One daemon runs at most one run per node, which is the lease: a second
//! start for a node that is running is a no-op.

use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tod_agent::SharedAgent;
use tod_agentd_client::Event;
use tod_core::attention;
use tod_core::autopilot::local::{LocalEvent, LocalRun, Live, Request, RunnerState};
use tod_core::autopilot::{AutopilotState, Budget, Outcome};
use tod_core::conversation::ConversationConfig;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

/// How often a run that ended to wait is checked for being due, and a run
/// that stopped for a request for having been answered. The clock is read
/// each time, so a machine that slept wakes its runs when it wakes.
const TICK: Duration = Duration::from_secs(5);

/// How long a drain lets a turn in flight finish before cancelling it.
const DRAIN_PATIENCE: Duration = Duration::from_secs(20);
const DRAIN_CANCELLED: Duration = Duration::from_secs(8);

#[derive(Default)]
struct Slot {
    run: Option<LocalRun>,
    live: Live,
    /// When the run in progress started (ms since the epoch).
    since: i64,
    /// How the last run ended, from this session or its saved state.
    outcome: Option<Outcome>,
    /// The node was seen waiting on the user since the run stopped for a
    /// request: once it no longer is, the run continues.
    seen_waiting: bool,
}

struct Inner {
    store: Arc<FleetStore>,
    agent: SharedAgent,
    slots: Mutex<HashMap<Uuid, Slot>>,
    subscribers: Mutex<Vec<mpsc::Sender<Event>>>,
}

#[derive(Clone)]
pub struct Runners {
    inner: Arc<Inner>,
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

impl Runners {
    pub fn new(store: Arc<FleetStore>, agent: SharedAgent) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                agent,
                slots: Mutex::new(HashMap::new()),
                subscribers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Pick up where the last daemon left off: a run that has no outcome in
    /// its saved state was cut off (the daemon or the machine stopped), so it
    /// starts again and reopens the conversation it was in; the others are
    /// only read, for what the runner line says and for their wake. Then keep
    /// the clock.
    pub fn resume_saved(&self) {
        let root = self.inner.store.paths().root().to_path_buf();
        for (node, state) in load_saved(&root) {
            match state.outcome {
                None => {
                    tracing::info!(%node, "resuming the run the daemon was stopped on");
                    if let Err(err) = self.start(node, false) {
                        tracing::warn!(%node, "could not resume: {err:#}");
                    }
                }
                Some(outcome) => {
                    self.inner.slots.lock().expect("runner slots").entry(node).or_default().outcome =
                        Some(outcome);
                }
            }
        }
        let this = self.clone();
        let _ = std::thread::Builder::new().name("tod-agentd-runners".into()).spawn(move || {
            loop {
                std::thread::sleep(TICK);
                this.tick();
            }
        });
    }

    /// Start a run that is due: one that ended to wait and whose time has
    /// come, or one that stopped for a request that has been answered.
    fn tick(&self) {
        let (due, request_nodes) = {
            let slots = self.inner.slots.lock().expect("runner slots");
            let now = now_ms();
            let mut due = Vec::new();
            let mut requests = Vec::new();
            for (node, slot) in slots.iter() {
                if slot.run.is_some() {
                    continue;
                }
                match &slot.outcome {
                    Some(Outcome::Waiting { due_at_ms, .. }) if *due_at_ms <= now => due.push(*node),
                    Some(Outcome::NeedsHuman { reason }) if reason.is_request() => requests.push(*node),
                    _ => {}
                }
            }
            (due, requests)
        };
        for node in due {
            tracing::info!(%node, "the wait is due; checking again");
            let _ = self.start(node, false);
        }
        if request_nodes.is_empty() {
            return;
        }
        let waiting = self
            .inner
            .store
            .read(|conn| attention::for_nodes(conn, &request_nodes))
            .unwrap_or_default();
        let mut resume = Vec::new();
        {
            let mut slots = self.inner.slots.lock().expect("runner slots");
            for node in request_nodes {
                let Some(slot) = slots.get_mut(&node) else { continue };
                let is_waiting = waiting.get(&node).is_some_and(|a| a.waiting_since.is_some());
                if is_waiting {
                    slot.seen_waiting = true;
                } else if slot.seen_waiting {
                    resume.push(node);
                }
            }
        }
        for node in resume {
            tracing::info!(%node, "the run's request was answered; continuing");
            let _ = self.start(node, false);
        }
    }

    /// Start `node`'s run, or continue its last one. A node that is running
    /// is left alone.
    pub fn start(&self, node: Uuid, renew_budget: bool) -> Result<()> {
        let root = self.inner.store.paths().root().to_path_buf();
        {
            let slots = self.inner.slots.lock().expect("runner slots");
            if slots.get(&node).is_some_and(|s| s.run.is_some()) {
                return Ok(());
            }
        }
        let config = config(&root).map_err(|e| anyhow!("{e}"))?;
        let inner = self.inner.clone();
        let run = LocalRun::start(
            self.inner.store.clone(),
            self.inner.agent.clone(),
            config,
            node,
            Budget::default(),
            renew_budget,
            move |event| Inner::on_event(&inner, node, event),
        )
        .map_err(|e| anyhow!("Starting the runner failed: {e:#}"))?;
        {
            let mut slots = self.inner.slots.lock().expect("runner slots");
            let slot = slots.entry(node).or_default();
            slot.run = Some(run);
            slot.live = Live::default();
            slot.since = now_ms();
            slot.outcome = None;
            slot.seen_waiting = false;
        }
        self.inner.publish(node);
        Ok(())
    }

    /// Stop once the agent's turn ends.
    pub fn pause(&self, node: Uuid) {
        if let Some(run) = self.inner.slots.lock().expect("runner slots").get(&node).and_then(|s| s.run.as_ref()) {
            run.pause();
        }
        self.inner.publish(node);
    }

    /// Cancel the turn in flight and stop.
    pub fn stop_now(&self, node: Uuid) {
        if let Some(run) = self.inner.slots.lock().expect("runner slots").get(&node).and_then(|s| s.run.as_ref()) {
            run.stop_now();
        }
        self.inner.publish(node);
    }

    /// Every node's runner state.
    pub fn snapshot(&self) -> Vec<RunnerState> {
        let slots = self.inner.slots.lock().expect("runner slots");
        slots.iter().map(|(node, slot)| Inner::state(*node, slot)).collect()
    }

    /// A feed of runner state changes, starting with every node's state now.
    pub fn subscribe(&self) -> mpsc::Receiver<Event> {
        let (tx, rx) = mpsc::channel();
        for state in self.snapshot() {
            let _ = tx.send(Inner::event(&state));
        }
        self.inner.subscribers.lock().expect("runner subscribers").push(tx);
        rx
    }

    /// Stop every run at its next boundary, and wait for them: a turn in
    /// flight gets [`DRAIN_PATIENCE`] to end before it is cancelled. Runs
    /// that have not ended keep no outcome, so the next daemon starts them
    /// again.
    pub fn drain(&self) {
        let nodes: Vec<Uuid> = {
            let slots = self.inner.slots.lock().expect("runner slots");
            slots.iter().filter(|(_, s)| s.run.is_some()).map(|(n, _)| *n).collect()
        };
        if nodes.is_empty() {
            return;
        }
        for node in &nodes {
            self.pause(*node);
        }
        if self.wait_idle(DRAIN_PATIENCE) {
            return;
        }
        for node in &nodes {
            self.stop_now(*node);
        }
        self.wait_idle(DRAIN_CANCELLED);
    }

    fn wait_idle(&self, patience: Duration) -> bool {
        let deadline = std::time::Instant::now() + patience;
        while std::time::Instant::now() < deadline {
            let busy = self
                .inner
                .slots
                .lock()
                .expect("runner slots")
                .values()
                .any(|s| s.run.as_ref().is_some_and(|r| !r.is_finished()));
            if !busy {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

impl Inner {
    fn state(node: Uuid, slot: &Slot) -> RunnerState {
        RunnerState {
            node,
            running: slot.run.is_some(),
            pausing: slot.run.as_ref().is_some_and(|r| r.requested() != Request::Run),
            since: slot.since,
            live: slot.live.clone(),
            outcome: slot.outcome.clone(),
        }
    }

    fn event(state: &RunnerState) -> Event {
        Event::Runner { state: serde_json::to_value(state).unwrap_or_default() }
    }

    fn publish(&self, node: Uuid) {
        let state = {
            let slots = self.slots.lock().expect("runner slots");
            let Some(slot) = slots.get(&node) else { return };
            Self::state(node, slot)
        };
        let event = Self::event(&state);
        self.subscribers
            .lock()
            .expect("runner subscribers")
            .retain(|tx| tx.send(event.clone()).is_ok());
    }

    fn on_event(this: &Arc<Self>, node: Uuid, event: LocalEvent) {
        {
            let mut slots = this.slots.lock().expect("runner slots");
            let Some(slot) = slots.get_mut(&node) else { return };
            match event {
                LocalEvent::Live(live) => slot.live = live,
                LocalEvent::Finished(result) => {
                    slot.run = None;
                    slot.live = Live::default();
                    slot.outcome = Some(match result {
                        Ok(outcome) => outcome,
                        Err(error) => Outcome::Stopped {
                            reason: format!("{}{error}", tod_core::autopilot::local::FAILED_PREFIX),
                        },
                    });
                }
            }
        }
        this.publish(node);
    }
}

/// What a run's conversations are configured with: the data root's settings
/// and the install's media bundle.
fn config(data_root: &std::path::Path) -> Result<ConversationConfig, String> {
    let paths = tod_core::interview::TodPaths::at(data_root);
    let settings = tod_core::interview::TodSettings::load(&paths).unwrap_or_default();
    let media = tod_core::media::MediaPaths::discover().map_err(|e| format!("Media bundle: {e}"))?;
    Ok(ConversationConfig {
        data_root: data_root.to_path_buf(),
        media,
        launch: settings.launch_options_for(tod_store::AgentRole::Default),
        settings_path: Some(paths.settings_path()),
        context: settings.interview_context.clone(),
    })
}
