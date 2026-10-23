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
//!
//! **Reconciling.** Each tick also compares what the daemon was asked for
//! (the node's [`Desired`] state) with the evidence in the store
//! (`tod_core::next_actor`, the activity leases): a node that should be
//! running but is not, with nothing else explaining it, is started; when it
//! cannot be, the failure is recorded as the run's outcome so the node shows
//! `You::Failed` with the reason. A lease on a node that should not be
//! running is reported and the run asked to stop. See [`decide`].

use crate::actor_journey::{self, ActorTracker};
use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tod_agent::SharedAgent;
use tod_agentd_client::Event;
use tod_core::attention;
use tod_core::autopilot::local::{FAILED_PREFIX, Live, LocalEvent, LocalRun, Request, RunnerState};
use tod_core::autopilot::{AutopilotState, Budget, Outcome};
use tod_core::conversation::ConversationConfig;
use tod_core::next_actor::{self, NextActor, NodeStanding, YouReason};
use tod_store::activity::{ActivityRepo, Lease};
use tod_store::deferrals::DeferralRepo;
use tod_store::fleet::{FleetMutation, FleetStore};
use uuid::Uuid;

/// How often a run that ended to wait is checked for being due, and a run
/// that stopped for a request for having been answered. The clock is read
/// each time, so a machine that slept wakes its runs when it wakes.
const TICK: Duration = Duration::from_secs(5);

/// How long a drain lets a turn in flight finish before cancelling it.
const DRAIN_PATIENCE: Duration = Duration::from_secs(20);
const DRAIN_CANCELLED: Duration = Duration::from_secs(8);

/// What the daemon was last asked to make true of a node's run. In memory: a
/// restarted daemon starts from the saved outcomes (`resume_saved`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Desired {
    /// Nobody has asked for anything yet (a node only read from its saved
    /// state).
    #[default]
    Unset,
    Run,
    Pause,
    Stop,
}

#[derive(Default)]
struct Slot {
    desired: Desired,
    /// A start is in progress (it can take a while), so the reconciler must
    /// not start the node a second time.
    starting: bool,
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
    /// Each node's last actor, for the journey.
    actors: Mutex<ActorTracker>,
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

/// What the reconciler knows of a node's slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SlotView {
    desired: Desired,
    starting: bool,
    /// A run thread is alive.
    active_run: bool,
    /// The slot has no outcome: the run did not end in a way anyone knows.
    outcome_none: bool,
}

/// What the reconciler does about a node.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    /// Start the run. With `lost`, the node's lifecycle lease went stale
    /// first (the text says which): clear it, and say it was restarted.
    Start { lost: Option<String> },
    /// This daemon's own run is still going though it should stop: ask again.
    StopOwn,
    /// Someone else's lease is live on a node that should not be running:
    /// say so. Only its owner can stop it.
    StopForeign { owner: String, kind: String },
}

/// The one rule: desired state against evidence.
///
/// - Desired `run`, no run alive, no outcome (the run did not end), and
///   nothing else explaining it: the node's actor is `You::NotStarted` (no
///   live lease, no open request, no pending wait, no saved outcome), or
///   `You::Failed` for a lifecycle lease that stopped beating. Start it.
///   Every other actor has a reason (processing, a request, a wait, an
///   outcome) and is left to it.
/// - Desired `pause` or `stop` and a live lifecycle lease: report it.
fn decide(view: &SlotView, standing: Option<&NodeStanding>, leases: &[Lease], now: i64) -> Option<Action> {
    match view.desired {
        Desired::Unset => None,
        Desired::Run => {
            if view.starting || view.active_run || !view.outcome_none {
                return None;
            }
            match &standing?.actor {
                NextActor::You { why: YouReason::NotStarted, .. } => Some(Action::Start { lost: None }),
                NextActor::You { why: YouReason::Failed { .. }, .. } => {
                    let stale = leases.iter().find(|l| l.is_lifecycle() && !l.is_live(now))?;
                    Some(Action::Start {
                        lost: Some(format!(
                            "{} held by {} stopped beating {}ms ago",
                            stale.kind,
                            stale.owner,
                            now - stale.heartbeat_at
                        )),
                    })
                }
                _ => None,
            }
        }
        Desired::Pause | Desired::Stop => {
            if view.active_run {
                return Some(Action::StopOwn);
            }
            leases
                .iter()
                .find(|l| l.is_lifecycle() && l.is_live(now))
                .map(|l| Action::StopForeign { owner: l.owner.clone(), kind: l.kind.clone() })
        }
    }
}

impl Runners {
    pub fn new(store: Arc<FleetStore>, agent: SharedAgent) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                agent,
                slots: Mutex::new(HashMap::new()),
                subscribers: Mutex::new(Vec::new()),
                actors: Mutex::new(ActorTracker::default()),
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
                    // A failure is recorded on the node by `start`.
                    if let Err(err) = self.start(node, false) {
                        tracing::error!(%node, "could not resume: {err:#}");
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
    /// come, or one that stopped for a request that has been answered. Then
    /// reconcile what is desired with what is happening.
    fn tick(&self) {
        self.start_due();
        if let Err(err) = self.reconcile() {
            tracing::error!("reconciling the runs failed: {err:#}");
        }
    }

    fn start_due(&self) {
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
            // A failure is recorded on the node by `start`.
            if let Err(err) = self.start(node, false) {
                tracing::error!(%node, "could not check the wait again: {err:#}");
            }
        }
        if request_nodes.is_empty() {
            return;
        }
        let waiting = match self.inner.store.read(|conn| attention::for_nodes(conn, &request_nodes)) {
            Ok(waiting) => waiting,
            Err(err) => {
                tracing::error!("could not read what the nodes wait on: {err:#}");
                return;
            }
        };
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
            if let Err(err) = self.start(node, false) {
                tracing::error!(%node, "could not continue after the answer: {err:#}");
            }
        }
    }

    /// The reconciler: clear spent deferrals, start what should be running
    /// and is not, report what runs and should not, and record who is next
    /// for the journey. Every failure is collected and returned together.
    fn reconcile(&self) -> Result<()> {
        let store = &self.inner.store;
        let root = store.paths().root().to_path_buf();
        let now = now_ms();
        store.reload_if_stale().context("reload the store")?;
        let standings = next_actor::compute(store, &root).context("work out who is next")?;
        let mut errors: Vec<String> = Vec::new();

        let rows = store.read(|conn| DeferralRepo::new(conn).list_all()).context("read the deferrals")?;
        for node in next_actor::spent_deferrals(&rows, &standings, now) {
            if let Err(err) = tod_core::defer::undefer(store, node) {
                errors.push(format!("clear the spent deferral of {node}: {err:#}"));
            }
        }

        let nodes: Vec<Uuid> = self.inner.slots.lock().expect("runner slots").keys().copied().collect();
        let mut leases: HashMap<Uuid, Vec<Lease>> = HashMap::new();
        for lease in store
            .read(|conn| ActivityRepo::new(conn).list_for_nodes(&nodes))
            .context("read the activity leases")?
        {
            leases.entry(lease.node_id).or_default().push(lease);
        }
        let plan: Vec<(Uuid, Action)> = {
            let slots = self.inner.slots.lock().expect("runner slots");
            slots
                .iter()
                .filter_map(|(node, slot)| {
                    let view = Self::view(slot);
                    let mine = leases.get(node).map(Vec::as_slice).unwrap_or(&[]);
                    decide(&view, standings.get(node), mine, now).map(|a| (*node, a))
                })
                .collect()
        };
        for (node, action) in plan {
            if let Err(err) = self.act(node, action) {
                errors.push(format!("{node}: {err:#}"));
            }
        }

        let changes = self.inner.actors.lock().expect("actor tracker").observe(&standings);
        actor_journey::record(&changes);

        if errors.is_empty() { Ok(()) } else { Err(anyhow!("{}", errors.join("; "))) }
    }

    fn view(slot: &Slot) -> SlotView {
        SlotView {
            desired: slot.desired,
            starting: slot.starting,
            active_run: slot.run.as_ref().is_some_and(|r| !r.is_finished()),
            outcome_none: slot.outcome.is_none(),
        }
    }

    fn act(&self, node: Uuid, action: Action) -> Result<()> {
        match action {
            Action::Start { lost } => {
                if let Some(lost) = &lost {
                    tracing::warn!(%node, "restarting a run whose processing was lost: {lost}");
                    let cleared = self
                        .inner
                        .store
                        .enqueue(FleetMutation::ActivityClearLost { node, at: now_ms() })
                        .map_err(|e| anyhow!("{e}"));
                    if let Err(err) = cleared {
                        // The stale lease would keep the node failed: say so.
                        let err = err.context("clear the lost lease");
                        self.record_start_failure(node, &err);
                        return Err(err);
                    }
                }
                // A run whose thread died: take it out of the slot.
                let dead = {
                    let mut slots = self.inner.slots.lock().expect("runner slots");
                    slots.get_mut(&node).and_then(|slot| slot.run.take_if(|r| r.is_finished()))
                };
                if let Some(dead) = dead {
                    dead.join();
                }
                tracing::warn!(%node, "desired to run but nothing is running it; starting it");
                let started = self.start(node, false);
                // The restart is recorded where the journey records who is next.
                actor_journey::record(&[actor_journey::Change {
                    node,
                    from: lost.as_ref().map_or("you:not_started", |_| "you:failed").to_string(),
                    to: if started.is_ok() { "working:runner" } else { "you:failed" }.to_string(),
                    detail: match (&lost, &started) {
                        (_, Err(err)) => format!("the daemon could not restart it: {err:#}"),
                        (Some(lost), Ok(())) => format!("restarted by the daemon after processing was lost: {lost}"),
                        (None, Ok(())) => "started by the daemon: it was desired to run and nothing was".to_string(),
                    },
                }]);
                started
            }
            Action::StopOwn => {
                let slots = self.inner.slots.lock().expect("runner slots");
                let Some(slot) = slots.get(&node) else { return Ok(()) };
                tracing::warn!(
                    %node,
                    owner = %next_actor::this_process(),
                    desired = ?slot.desired,
                    "a run is still going on a node that should not run; asking it to stop"
                );
                if let Some(run) = &slot.run {
                    match slot.desired {
                        Desired::Stop => run.stop_now(),
                        _ => run.pause(),
                    }
                }
                Ok(())
            }
            Action::StopForeign { owner, kind } => {
                tracing::warn!(
                    %node, %owner, %kind,
                    "a lease is live on a node that should not run, and it is not this daemon's run"
                );
                Ok(())
            }
        }
    }

    /// Record that `node`'s run could not be started, as its outcome: in the
    /// slot, and in its saved state, so the node shows `You::Failed` with the
    /// reason and a restarted daemon does not start it again at once. The
    /// same way `LocalRun` saves a run that failed.
    fn record_start_failure(&self, node: Uuid, error: &anyhow::Error) {
        let reason = format!("{FAILED_PREFIX}{error:#}");
        let outcome = Outcome::Stopped { reason: reason.clone() };
        let root = self.inner.store.paths().root().to_path_buf();
        let saved = AutopilotState::load(&root, node).and_then(|mut state| {
            state.outcome = Some(outcome.clone());
            state.save(&root, node)
        });
        if let Err(err) = saved {
            // The slot still says it; only a restart would lose it.
            tracing::error!(%node, "could not save the failure of the run's start ({reason}): {err:#}");
        }
        {
            let mut slots = self.inner.slots.lock().expect("runner slots");
            let slot = slots.entry(node).or_default();
            slot.starting = false;
            slot.run = None;
            slot.live = Live::default();
            slot.outcome = Some(outcome);
        }
        self.inner.publish(node);
    }

    /// Start `node`'s run, or continue its last one. A node that is running
    /// is left alone. When it cannot be started the failure is recorded as
    /// the run's outcome (the node shows it) and returned.
    pub fn start(&self, node: Uuid, renew_budget: bool) -> Result<()> {
        {
            let mut slots = self.inner.slots.lock().expect("runner slots");
            let slot = slots.entry(node).or_default();
            slot.desired = Desired::Run;
            if slot.run.is_some() || slot.starting {
                return Ok(());
            }
            slot.starting = true;
        }
        match self.spawn(node, renew_budget) {
            Ok(run) => {
                {
                    let mut slots = self.inner.slots.lock().expect("runner slots");
                    let slot = slots.entry(node).or_default();
                    slot.starting = false;
                    slot.run = Some(run);
                    slot.live = Live::default();
                    slot.since = now_ms();
                    slot.outcome = None;
                    slot.seen_waiting = false;
                }
                self.inner.publish(node);
                Ok(())
            }
            Err(err) => {
                self.record_start_failure(node, &err);
                Err(err)
            }
        }
    }

    fn spawn(&self, node: Uuid, renew_budget: bool) -> Result<LocalRun> {
        let root = self.inner.store.paths().root().to_path_buf();
        let config = config(&root).map_err(|e| anyhow!("{e}"))?;
        let inner = self.inner.clone();
        LocalRun::start(
            self.inner.store.clone(),
            self.inner.agent.clone(),
            config,
            node,
            Budget::default(),
            renew_budget,
            move |event| Inner::on_event(&inner, node, event),
        )
        .map_err(|e| anyhow!("Starting the runner failed: {e:#}"))
    }

    /// Stop once the agent's turn ends.
    pub fn pause(&self, node: Uuid) {
        {
            let mut slots = self.inner.slots.lock().expect("runner slots");
            let slot = slots.entry(node).or_default();
            slot.desired = Desired::Pause;
            if let Some(run) = &slot.run {
                run.pause();
            }
        }
        self.inner.publish(node);
    }

    /// Read GitHub now, for a run that is waiting on it.
    pub fn poll_now(&self, node: Uuid) {
        if let Some(run) = self.inner.slots.lock().expect("runner slots").get(&node).and_then(|s| s.run.as_ref()) {
            run.poll_now();
        }
    }

    /// Cancel the turn in flight and stop.
    pub fn stop_now(&self, node: Uuid) {
        {
            let mut slots = self.inner.slots.lock().expect("runner slots");
            let slot = slots.entry(node).or_default();
            slot.desired = Desired::Stop;
            if let Some(run) = &slot.run {
                run.stop_now();
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tod_core::next_actor::{ExternalWait, WaitKind};

    const NOW: i64 = 1_000_000;

    fn view(desired: Desired) -> SlotView {
        SlotView { desired, starting: false, active_run: false, outcome_none: true }
    }

    fn standing(actor: NextActor) -> NodeStanding {
        NodeStanding { node_id: Uuid::new_v4(), actor, deferred: None }
    }

    fn you(why: YouReason) -> NodeStanding {
        standing(NextActor::You { why, since: 1 })
    }

    fn lease(owner: &str, heartbeat_at: i64) -> Lease {
        Lease {
            id: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
            kind: tod_store::activity::KIND_RUN.to_string(),
            owner: owner.to_string(),
            started_at: 0,
            heartbeat_at,
        }
    }

    #[test]
    fn a_desired_run_with_nothing_explaining_it_is_started() {
        assert_eq!(
            decide(&view(Desired::Run), Some(&you(YouReason::NotStarted)), &[], NOW),
            Some(Action::Start { lost: None })
        );
    }

    #[test]
    fn a_desired_run_with_a_reason_is_left_alone() {
        let v = view(Desired::Run);
        let waiting = standing(NextActor::Others {
            wait: ExternalWait {
                kind: WaitKind::HumanReview,
                who: None,
                detail: None,
                since: 1,
                next_check_ms: None,
                overdue: false,
                retry: None,
            },
        });
        assert_eq!(decide(&v, Some(&waiting), &[], NOW), None, "a pending wait");
        assert_eq!(decide(&v, Some(&you(YouReason::Request { count: 1 })), &[], NOW), None, "an open request");
        assert_eq!(
            decide(&v, Some(&standing(NextActor::Working { what: next_actor::Work::Runner, since: 1 })), &[], NOW),
            None,
            "live processing"
        );
        assert_eq!(decide(&v, Some(&you(YouReason::Failed { error: "x".into() })), &[], NOW), None, "a recorded failure");
        assert_eq!(decide(&v, None, &[], NOW), None, "no Lifecycle");
        assert_eq!(decide(&SlotView { starting: true, ..v }, Some(&you(YouReason::NotStarted)), &[], NOW), None);
        assert_eq!(decide(&SlotView { active_run: true, ..v }, Some(&you(YouReason::NotStarted)), &[], NOW), None);
        assert_eq!(decide(&SlotView { outcome_none: false, ..v }, Some(&you(YouReason::NotStarted)), &[], NOW), None);
        assert_eq!(decide(&view(Desired::Unset), Some(&you(YouReason::NotStarted)), &[], NOW), None);
    }

    #[test]
    fn a_lost_lease_on_a_desired_run_restarts_it_and_says_why() {
        let stale = lease("tod-agentd:7", NOW - tod_store::activity::STALE_AFTER_MS - 1);
        let got = decide(
            &view(Desired::Run),
            Some(&you(YouReason::Failed { error: "processing was lost".into() })),
            &[stale],
            NOW,
        );
        let Some(Action::Start { lost: Some(lost) }) = got else { panic!("expected a restart, got {got:?}") };
        assert!(lost.contains("tod-agentd:7") && lost.contains("run"), "{lost}");
    }

    #[test]
    fn a_live_lease_on_a_node_that_should_stop_is_reported() {
        let live = lease("tod:9", NOW - 1);
        let standing = standing(NextActor::Working { what: next_actor::Work::Runner, since: 1 });
        for desired in [Desired::Pause, Desired::Stop] {
            assert_eq!(
                decide(&view(desired), Some(&standing), std::slice::from_ref(&live), NOW),
                Some(Action::StopForeign { owner: "tod:9".into(), kind: "run".into() })
            );
            // Our own run still going is asked again.
            assert_eq!(
                decide(&SlotView { active_run: true, ..view(desired) }, Some(&standing), std::slice::from_ref(&live), NOW),
                Some(Action::StopOwn)
            );
            // A stale lease is not processing, and nothing is reported.
            let stale = lease("tod:9", NOW - tod_store::activity::STALE_AFTER_MS - 1);
            assert_eq!(decide(&view(desired), Some(&standing), &[stale], NOW), None);
            assert_eq!(decide(&view(desired), Some(&standing), &[], NOW), None);
        }
    }

    // ---- against a real store ----

    use tod_agent::MockAgentProvider;
    use tod_store::outline::OutlineMutation;

    struct Fixture {
        runners: Runners,
        node: Uuid,
        root: std::path::PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// A store with one node that has Lifecycle. The media bundle is made
    /// unfindable, so starting a run fails.
    fn fixture() -> Fixture {
        fixture_with_media(false)
    }

    /// [`fixture`], with the real media bundle findable when `media`, so a
    /// run can start and go as far as the mock agent takes it.
    fn fixture_with_media(media: bool) -> Fixture {
        if !media {
            // One process per test under nextest; the variable is this test's.
            unsafe { std::env::set_var("TOD_MEDIA_ROOT", "/nonexistent/tod-media") };
        }
        let root = std::env::temp_dir().join(format!("tod-agentd-runners-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = Arc::new(FleetStore::open(&root).unwrap());
        fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
        fleet.writer().flush().unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: None,
                position: tod_store::outline::CreatePosition::Below,
                title: "N".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        let node = fleet.flatten_outline(list_id).unwrap()[0].node.id;
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: node,
                capabilities: vec![tod_store::outline::Capability::Lifecycle],
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        let agent: SharedAgent = Arc::new(Mutex::new(Box::new(MockAgentProvider::new())));
        Fixture { runners: Runners::new(fleet, agent), node, root }
    }

    fn standing_of(fx: &Fixture) -> NodeStanding {
        fx.runners.inner.store.reload_if_stale().unwrap();
        next_actor::compute(&fx.runners.inner.store, &fx.root).unwrap().remove(&fx.node).expect("Lifecycle")
    }

    #[test]
    fn a_start_that_fails_shows_the_node_failed_with_the_reason_and_keeps_it_saved() {
        let fx = fixture();
        let err = fx.runners.start(fx.node, false).unwrap_err();
        assert!(format!("{err:#}").contains("Media bundle"), "{err:#}");
        let NextActor::You { why: YouReason::Failed { error }, .. } = standing_of(&fx).actor else {
            panic!("the node should be failed");
        };
        assert!(error.contains("Media bundle"), "{error}");
        // Saved: a new daemon reads it, and does not start the node again.
        let saved = AutopilotState::load(&fx.root, fx.node).unwrap();
        assert!(matches!(saved.outcome, Some(Outcome::Stopped { ref reason }) if reason.starts_with(FAILED_PREFIX)));
        // The slot is not "running" and not "starting".
        let state = fx.runners.snapshot().into_iter().find(|s| s.node == fx.node).unwrap();
        assert!(!state.running);
    }

    #[test]
    fn the_reconciler_starts_a_desired_run_and_a_failed_start_is_reported_by_it() {
        let fx = fixture();
        {
            let mut slots = fx.runners.inner.slots.lock().unwrap();
            slots.entry(fx.node).or_default().desired = Desired::Run;
        }
        assert!(matches!(standing_of(&fx).actor, NextActor::You { why: YouReason::NotStarted, .. }));
        let err = fx.runners.reconcile().unwrap_err();
        assert!(format!("{err:#}").contains("Media bundle"), "{err:#}");
        assert!(matches!(standing_of(&fx).actor, NextActor::You { why: YouReason::Failed { .. }, .. }));
        // Now it has an outcome: the next tick leaves it be.
        fx.runners.reconcile().unwrap();
    }

    #[test]
    fn a_lost_lease_is_cleared_when_the_run_is_restarted() {
        let fx = fixture();
        let store = &fx.runners.inner.store;
        let stale_at = now_ms() - tod_store::activity::STALE_AFTER_MS - 10_000;
        store
            .enqueue(FleetMutation::ActivityStart {
                id: Uuid::new_v4(),
                node: fx.node,
                kind: tod_store::activity::KIND_RUN.into(),
                owner: "dead:1".into(),
                at: stale_at,
            })
            .unwrap();
        store.writer().flush().unwrap();
        {
            let mut slots = fx.runners.inner.slots.lock().unwrap();
            slots.entry(fx.node).or_default().desired = Desired::Run;
        }
        assert!(
            matches!(standing_of(&fx).actor, NextActor::You { why: YouReason::Failed { ref error }, .. } if error.contains("dead:1"))
        );
        // The restart fails here (no media), but the stale lease is cleared first.
        let _ = fx.runners.reconcile();
        store.writer().flush().unwrap();
        store.reload_if_stale().unwrap();
        let left = store.read(|c| ActivityRepo::new(c).list_for_nodes(&[fx.node])).unwrap();
        assert!(left.is_empty(), "{left:?}");
    }

    /// Wait for `node`'s run thread to end (it is the mock agent's, so short).
    fn wait_until_not_running(fx: &Fixture) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let running = fx.runners.snapshot().into_iter().any(|s| s.node == fx.node && s.running);
            if !running {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "the run did not end");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    #[test]
    fn a_dead_run_is_restarted_by_the_reconciler_and_goes_as_far_as_the_agent_takes_it() {
        let fx = fixture_with_media(true);
        let store = &fx.runners.inner.store;
        let stale_at = now_ms() - tod_store::activity::STALE_AFTER_MS - 10_000;
        store
            .enqueue(FleetMutation::ActivityStart {
                id: Uuid::new_v4(),
                node: fx.node,
                kind: tod_store::activity::KIND_RUN.into(),
                owner: "dead:1".into(),
                at: stale_at,
            })
            .unwrap();
        store.writer().flush().unwrap();
        fx.runners.inner.slots.lock().unwrap().entry(fx.node).or_default().desired = Desired::Run;
        assert!(
            matches!(standing_of(&fx).actor, NextActor::You { why: YouReason::Failed { ref error }, .. } if error.contains("dead:1")),
            "the loss is shown before the restart"
        );

        // The restart succeeds: nothing is reported as failed.
        fx.runners.reconcile().unwrap();
        wait_until_not_running(&fx);
        store.writer().flush().unwrap();
        store.reload_if_stale().unwrap();

        let left = store.read(|c| ActivityRepo::new(c).list_for_nodes(&[fx.node])).unwrap();
        assert!(left.is_empty(), "the stale lease is gone and the run's own lease ended: {left:?}");
        let saved = AutopilotState::load(&fx.root, fx.node).unwrap();
        assert!(
            !matches!(saved.outcome, Some(Outcome::Stopped { ref reason }) if reason.starts_with(FAILED_PREFIX)),
            "{:?}",
            saved.outcome
        );
        assert!(saved.outcome.is_some(), "the run ended and said how");
        assert!(
            !matches!(standing_of(&fx).actor, NextActor::You { why: YouReason::Failed { ref error }, .. } if error.contains("lost")),
            "no longer reported as lost"
        );
    }

    /// With a journey recorder installed, a change of actor seen by the tick
    /// is on the node's journey; the first sight only seeds.
    #[test]
    fn a_change_of_actor_is_recorded_to_the_node_journey_by_the_tick() {
        let fx = fixture();
        let journeys = fx.root.join("journeys");
        tod_core::journey::recorder::install(tod_core::journey::recorder::spawn(journeys.clone(), 1024));

        fx.runners.reconcile().unwrap(); // first sight: seeds, records nothing
        AutopilotState {
            outcome: Some(Outcome::Stopped { reason: format!("{FAILED_PREFIX}boom") }),
            ..AutopilotState::fresh()
        }
        .save(&fx.root, fx.node)
        .unwrap();
        fx.runners.reconcile().unwrap();

        // The recorder's writer thread appends asynchronously.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let changes = loop {
            let changes: Vec<(String, String, String)> =
                match tod_journey::JourneyReader::open(&journeys, tod_journey::JourneyKey::Node(fx.node)) {
                    Ok(reader) => reader
                        .all()
                        .iter()
                        .filter_map(|r| match &r.event {
                            tod_journey::Event::NextActorChanged { from, to, detail } => {
                                Some((from.clone(), to.clone(), detail.clone()))
                            }
                            _ => None,
                        })
                        .collect(),
                    // Nothing written yet.
                    Err(_) => Vec::new(),
                };
            if !changes.is_empty() || std::time::Instant::now() > deadline {
                break changes;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].0, "you:not_started");
        assert_eq!(changes[0].1, "you:failed");
        assert!(changes[0].2.contains("boom"), "{:?}", changes[0]);
    }

    #[test]
    fn spent_deferrals_are_cleared_by_the_tick() {
        let fx = fixture();
        let store = &fx.runners.inner.store;
        store
            .enqueue(FleetMutation::DeferNode { node: fx.node, until: Some(now_ms() - 1), at: now_ms() - 10_000 })
            .unwrap();
        store.writer().flush().unwrap();
        fx.runners.reconcile().unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().unwrap();
        let rows = store.read(|c| DeferralRepo::new(c).list_all()).unwrap();
        assert!(rows.is_empty(), "{rows:?}");
    }

    #[test]
    fn pause_and_stop_are_the_desired_state() {
        let fx = fixture();
        let desired = |fx: &Fixture| fx.runners.inner.slots.lock().unwrap().get(&fx.node).map(|s| s.desired);
        fx.runners.pause(fx.node);
        assert_eq!(desired(&fx), Some(Desired::Pause));
        fx.runners.stop_now(fx.node);
        assert_eq!(desired(&fx), Some(Desired::Stop));
        let _ = fx.runners.start(fx.node, false);
        assert_eq!(desired(&fx), Some(Desired::Run));
    }
}
