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
use tod_core::runner_place::RunnerPlace;
use tod_store::activity::{ActivityRepo, Lease};
use tod_store::deferrals::DeferralRepo;
use tod_store::fleet::{FleetMutation, FleetStore};
use uuid::Uuid;

/// Why this daemon does not run a node whose Files are in a cloud sandbox.
const SUPERVISED: &str = "this node's Files run in a cloud sandbox, so its autopilot runs under the \
supervisor in that sandbox, not in the daemon on this machine";

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
    /// When the stale waits were last swept (ms; 0 = never), so the
    /// reconciler writes at most once per [`WAIT_SWEEP_EVERY_MS`].
    last_wait_sweep: std::sync::atomic::AtomicI64,
    /// The nodes of the last logged stop mismatch, and when it was logged.
    last_stop_mismatch_log: Mutex<(Vec<Uuid>, i64)>,
    /// When this build was made (ms; 0 = not known): requests filed before it
    /// whose cause a deploy can remove are re-examined once
    /// (`tod_core::decision_recheck`).
    build_time_ms: std::sync::atomic::AtomicI64,
    /// Whether that re-examination has been done by this process.
    rechecked: std::sync::atomic::AtomicBool,
}

/// How often the reconciler sweeps waits nothing serves.
const WAIT_SWEEP_EVERY_MS: i64 = 5 * 60 * 1000;

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
    /// Where the node's run is hosted now (from its resolved Files).
    place: RunnerPlace,
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
    /// The node should run, but it now runs in a cloud sandbox under the
    /// supervisor, which this daemon does not host: say so, as the node's
    /// failure, instead of starting it.
    MovedToSandbox,
}

/// The one rule: desired state against evidence.
///
/// - Desired `run`, no run alive, no outcome (the run did not end), and
///   nothing else explaining it: the node's actor is `You::NotStarted` (no
///   live lease, no open request, no pending wait, no saved outcome), or
///   `You::Failed` for a lifecycle lease that stopped beating. Start it.
///   Every other actor has a reason (processing, a request, a wait, an
///   outcome) and is left to it.
/// - Desired `run` on a node that now runs in a cloud sandbox: never start it
///   here; report it ([`Action::MovedToSandbox`]).
/// - Desired `pause` or `stop` and a live lifecycle lease: report it.
fn decide(view: &SlotView, standing: Option<&NodeStanding>, leases: &[Lease], now: i64) -> Option<Action> {
    match view.desired {
        Desired::Unset => None,
        Desired::Run => {
            if view.starting || view.active_run || !view.outcome_none {
                return None;
            }
            if view.place == RunnerPlace::Supervisor {
                return Some(Action::MovedToSandbox);
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
                last_wait_sweep: std::sync::atomic::AtomicI64::new(0),
                last_stop_mismatch_log: Mutex::new((Vec::new(), 0)),
                build_time_ms: std::sync::atomic::AtomicI64::new(0),
                rechecked: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    /// Say when this build was made, so the first tick re-examines the
    /// requests filed before it (`tod_core::decision_recheck`).
    pub fn set_build_time_ms(&self, ms: i64) {
        self.inner.build_time_ms.store(ms, std::sync::atomic::Ordering::Relaxed);
    }

    /// Withdraw, once per process, the requests whose cause this build may
    /// have fixed; the nodes' runs continue on the next tick, as for an
    /// answered request. Not marked done on a failure, so the next tick tries
    /// again.
    fn recheck_requests(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let built_at = self.inner.build_time_ms.load(Relaxed);
        if built_at <= 0 || self.inner.rechecked.load(Relaxed) {
            return;
        }
        match tod_core::decision_recheck::sweep(&self.inner.store, built_at) {
            Ok(withdrawn) => {
                self.inner.rechecked.store(true, Relaxed);
                for d in &withdrawn {
                    tracing::info!(node = %d.node_id, request = %d.id, "withdrew a request filed before this build; its cause may be fixed");
                }
            }
            Err(err) => tracing::error!("re-examining the requests filed before this build failed: {err:#}"),
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
            // A node deleted from the store holds no slot: its saved run file
            // is left on disk (not ours to delete), noted once here.
            match self.node_exists(node) {
                Ok(true) => {}
                Ok(false) => {
                    tracing::info!(%node, "the node of this saved run no longer exists; the run is not resumed and holds no slot");
                    continue;
                }
                Err(err) => {
                    tracing::error!(%node, "could not tell whether the node of a saved run exists, so it is not resumed: {err:#}");
                    continue;
                }
            }
            match RunnerPlace::of(&self.inner.store, node) {
                Ok(RunnerPlace::Local) => {}
                Ok(RunnerPlace::Supervisor) => {
                    // Its autopilot runs under the supervisor now: this run is
                    // not resumed, and not kept as a slot, so nothing here
                    // starts it later. A cut-off one is shown on the node.
                    let err = anyhow!(SUPERVISED);
                    tracing::error!(%node, "not resuming the saved local run: {err:#}");
                    if state.outcome.is_none() {
                        self.record_start_failure(node, &err);
                    }
                    continue;
                }
                Err(err) => {
                    tracing::error!(%node, "could not tell where the saved run belongs, so it is not resumed: {err:#}");
                    continue;
                }
            }
            match state.outcome {
                None => {
                    tracing::info!(%node, "resuming the run the daemon was stopped on");
                    // A failure is recorded on the node by `start`.
                    if let Err(err) = self.start_auto(node) {
                        tracing::error!(%node, "could not resume: {err:#}");
                    }
                }
                Some(outcome) => {
                    // A run that ended `Done` for a node that is no longer
                    // `done` (moved back or reopened while the daemon was
                    // down) is desired to run: `desired` is only in memory, so
                    // left `Unset` the reconciler would clear the stale
                    // outcome and never start the node.
                    let reopened = matches!(outcome, Outcome::Done)
                        && match tod_core::lifecycle::current_state(&self.inner.store, node) {
                            Ok(state) => state != "done",
                            Err(err) => {
                                tracing::error!(%node, "could not read the lifecycle state of a node whose saved run is done, so it is not started: {err:#}");
                                false
                            }
                        };
                    let mut slots = self.inner.slots.lock().expect("runner slots");
                    let slot = slots.entry(node).or_default();
                    // The user's pause or stop is theirs until they undo it:
                    // `desired` is only in memory, so it is read back here.
                    if let Outcome::Stopped { reason } = &outcome {
                        if reason == tod_core::autopilot::local::PAUSED {
                            slot.desired = Desired::Pause;
                        } else if reason == tod_core::autopilot::local::STOPPED {
                            slot.desired = Desired::Stop;
                        }
                    }
                    slot.outcome = Some(outcome);
                    if reopened {
                        tracing::info!(%node, "the saved run is done but the node is not; it is desired to run");
                        slot.desired = Desired::Run;
                    }
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
    /// Logs the stop-mismatch statistic (`tod_core::stop_mismatch`): how many
    /// nodes, and how long the longest has lasted.
    fn log_stop_mismatch(&self, found: &[tod_core::stop_mismatch::Mismatch], now: i64) {
        const REPEAT_MS: i64 = 5 * 60_000;
        let mut nodes: Vec<Uuid> = found.iter().map(|m| m.node_id).collect();
        nodes.sort();
        let mut last = self.inner.last_stop_mismatch_log.lock().expect("stop mismatch log");
        if last.0 == nodes && (nodes.is_empty() || now - last.1 < REPEAT_MS) {
            return;
        }
        if found.is_empty() {
            tracing::info!("every stopped or failed run is shown as such: no mismatch");
        } else {
            let longest = found.iter().map(|m| m.lasted_ms).max().unwrap_or(0);
            let list = found.iter().map(|m| format!("{} (shows {}, {}s)", m.node_id, m.shown, m.lasted_ms / 1000)).collect::<Vec<_>>().join("; ");
            tracing::warn!(count = found.len(), longest_ms = longest, "runs stopped or failed while the node shows something else: {list}");
        }
        *last = (nodes, now);
    }

    fn tick(&self) {
        self.start_due();
        self.resume_stale_request_waits();
        self.adopt_cut_off_runs();
        if let Err(err) = self.reconcile() {
            tracing::error!("reconciling the runs failed: {err:#}");
        }
        // After `start_due` has seen each node waiting, so the next tick sees
        // its request gone and continues the run.
        self.recheck_requests();
    }

    /// A local run row that says `working` while no live lease backs it was
    /// cut off: a daemon replaced by a newer one, killed, or crashed gave it
    /// up without recording an end, and no slot here holds it (a node the
    /// saved-run scan did not resume, or one whose saved state ended). Resume
    /// it, as `resume_saved` does for a run the daemon was stopped on: the
    /// autopilot continues from its saved state. The stale lease is cleared
    /// and the restart is journaled, naming the dead owner. Where it cannot be
    /// resumed (the node runs in a sandbox now, or the user paused it) the
    /// run is recorded as failed with that reason, never left `working`.
    fn adopt_cut_off_runs(&self) {
        let now = now_ms();
        let found = self.inner.store.read(|conn| {
            let rows = tod_store::run_state::list_working(conn)?;
            let ids: Vec<Uuid> = rows.iter().map(|r| r.node_id).collect();
            let mut leases: HashMap<Uuid, Vec<Lease>> = HashMap::new();
            for lease in ActivityRepo::new(conn).list_for_nodes(&ids)? {
                leases.entry(lease.node_id).or_default().push(lease);
            }
            Ok((rows, leases))
        });
        let (rows, leases) = match found {
            Ok(found) => found,
            Err(err) => {
                tracing::error!("could not look for runs cut off while working: {err:#}");
                return;
            }
        };
        for row in rows {
            let node = row.node_id;
            let Some(ghost) = tod_core::run_state::ghost_working(&row, leases.get(&node).map(Vec::as_slice).unwrap_or(&[]), now)
            else {
                continue;
            };
            {
                let slots = self.inner.slots.lock().expect("runner slots");
                if let Some(slot) = slots.get(&node)
                    && (slot.starting
                        || slot.run.as_ref().is_some_and(|r| !r.is_finished())
                        || matches!(slot.desired, Desired::Pause | Desired::Stop)
                        || matches!(&slot.outcome, Some(Outcome::Stopped { reason }) if reason.starts_with(FAILED_PREFIX)))
                {
                    continue;
                }
            }
            let why = ghost.describe(&row, now);
            match RunnerPlace::of(&self.inner.store, node) {
                Ok(RunnerPlace::Local) => {}
                Ok(RunnerPlace::Supervisor) => {
                    let err = anyhow!("{why}; {SUPERVISED}");
                    tracing::error!(%node, "{err:#}");
                    self.record_start_failure(node, &err);
                    continue;
                }
                Err(err) => {
                    tracing::error!(%node, "could not tell where a cut-off run belongs, so it is not resumed: {err:#}");
                    continue;
                }
            }
            tracing::warn!(%node, "resuming a run cut off while working: {why}");
            let lost = Some(format!(
                "{} held by {} stopped beating {}ms ago",
                tod_store::activity::KIND_RUN,
                ghost.owner.as_deref().unwrap_or("nothing"),
                now - ghost.silent_since,
            ));
            // A failure to start is recorded on the node by `start`.
            if let Err(err) = self.act(node, Action::Start { lost }) {
                tracing::error!(%node, "could not resume the cut-off run: {err:#}");
                continue;
            }
            // Started in name only (the user's pause is saved and holds):
            // the row must not go on saying `working`.
            let running = self
                .inner
                .slots
                .lock()
                .expect("runner slots")
                .get(&node)
                .is_some_and(|s| s.run.is_some() || s.starting);
            if !running {
                let err = anyhow!("{why}; it was not resumed, because the user paused or stopped it");
                self.record_start_failure(node, &err);
            }
        }
    }

    /// A run row that says "waiting on a request" while none is pending
    /// (gap 67) is stale whatever this daemon remembers of the run: it may
    /// have been answered while the slot held no outcome, withdrawn by the
    /// agent, or synced from another machine. Continue the run, which
    /// rewrites the row. Reported with its age, so a wait that outlives a
    /// tick is never silent. A run the user paused or stopped is theirs.
    fn resume_stale_request_waits(&self) {
        let stale = match tod_core::run_state::stale_request_waits(&self.inner.store) {
            Ok(stale) => stale,
            Err(err) => {
                tracing::error!("could not look for request waits with no request: {err:#}");
                return;
            }
        };
        let now = now_ms();
        for wait in stale {
            let node = wait.node;
            {
                let slots = self.inner.slots.lock().expect("runner slots");
                if let Some(slot) = slots.get(&node)
                    && (slot.run.is_some()
                        || slot.starting
                        || matches!(slot.desired, Desired::Pause | Desired::Stop)
                        // A start that failed is recorded on the node: not
                        // retried every tick behind a row that could not be
                        // rewritten.
                        || matches!(&slot.outcome, Some(Outcome::Stopped { reason }) if reason.starts_with(FAILED_PREFIX)))
                {
                    continue;
                }
            }
            match RunnerPlace::of(&self.inner.store, node) {
                Ok(RunnerPlace::Local) => {}
                // Not this daemon's run: its supervisor owns it.
                Ok(RunnerPlace::Supervisor) => {
                    tracing::error!(%node, age_ms = now - wait.since, "a run in a sandbox waits on a request that is gone; only its supervisor can continue it");
                    continue;
                }
                Err(err) => {
                    tracing::error!(%node, "could not tell where a stale request wait belongs: {err:#}");
                    continue;
                }
            }
            tracing::warn!(%node, age_ms = now - wait.since, "the run waits on a request that is gone; continuing");
            if let Err(err) = self.start_auto(node) {
                tracing::error!(%node, "could not continue a run whose request is gone: {err:#}");
            }
        }
    }

    fn start_due(&self) {
        let (mut due, request_nodes, parked) = {
            let slots = self.inner.slots.lock().expect("runner slots");
            let now = now_ms();
            let mut due = Vec::new();
            let mut requests = Vec::new();
            let mut parked = Vec::new();
            for (node, slot) in slots.iter() {
                if slot.run.is_some() {
                    continue;
                }
                match &slot.outcome {
                    Some(Outcome::Waiting { due_at_ms, .. }) if *due_at_ms <= now => due.push(*node),
                    Some(Outcome::Waiting { .. }) => parked.push((*node, slot.since)),
                    // A run the user paused or stopped by hand is theirs to resume.
                    Some(Outcome::NeedsHuman { reason })
                        if reason.is_request() && !matches!(slot.desired, Desired::Pause | Desired::Stop) =>
                    {
                        requests.push((*node, matches!(reason, tod_core::autopilot::NeedsHuman::Decision { .. })))
                    }
                    _ => {}
                }
            }
            (due, requests, parked)
        };
        // A run parked on a silent review bot is woken by the user's answer
        // to the bot-silence question, not at the wait's time.
        for (node, since) in parked {
            match self.inner.store.read(|conn| tod_core::bot_silence::answered_since(conn, node, since)) {
                Ok(true) => due.push(node),
                Ok(false) => {}
                Err(err) => tracing::error!(%node, "could not read the answers to the bot-silence question: {err:#}"),
            }
        }
        for node in due {
            tracing::info!(%node, "the wait is due; checking again");
            // A failure is recorded on the node by `start`.
            if let Err(err) = self.start_auto(node) {
                tracing::error!(%node, "could not check the wait again: {err:#}");
            }
        }
        if request_nodes.is_empty() {
            return;
        }
        let ids: Vec<Uuid> = request_nodes.iter().map(|(n, _)| *n).collect();
        let waiting = match self.inner.store.read(|conn| attention::for_nodes(conn, &ids)) {
            Ok(waiting) => waiting,
            Err(err) => {
                tracing::error!("could not read what the nodes wait on: {err:#}");
                return;
            }
        };
        let mut resume = Vec::new();
        {
            let mut slots = self.inner.slots.lock().expect("runner slots");
            for (node, is_decision) in request_nodes {
                let Some(slot) = slots.get_mut(&node) else { continue };
                let is_waiting = waiting.get(&node).is_some_and(|a| a.waiting_since.is_some());
                if is_waiting {
                    slot.seen_waiting = true;
                } else if slot.seen_waiting || is_decision {
                    // A run that stopped on a question was stopped because a
                    // decision was pending, so one that is no longer pending
                    // is an answer even if this daemon never saw it pending
                    // (answered while it was down, or between two ticks, or
                    // the run was read from its saved state). Without that,
                    // the stale "waiting on N question(s)" kept the node
                    // from being started, and `decide` leaves it alone for
                    // having an outcome.
                    resume.push(node);
                }
            }
        }
        for node in resume {
            tracing::info!(%node, "the run's request was answered; continuing");
            if let Err(err) = self.start_auto(node) {
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
        // Waits nothing serves any more would read as "waiting on others"
        // forever and stop every run at its next boundary.
        let last = self.inner.last_wait_sweep.load(std::sync::atomic::Ordering::Relaxed);
        if now - last >= WAIT_SWEEP_EVERY_MS {
            store
                .interview(
                    tod_store::waits::LIFECYCLE_ACTOR,
                    tod_store::interview::InterviewCommand::SweepWaits {
                        expire_grace_ms: Some(tod_store::waits::EXPIRE_GRACE_MS),
                    },
                )
                .map_err(|e| anyhow::anyhow!("sweep the stale waits: {e}"))?;
            self.inner.last_wait_sweep.store(now, std::sync::atomic::Ordering::Relaxed);
        }
        let standings = next_actor::compute(store, &root).context("work out who is next")?;
        let mut errors: Vec<String> = Vec::new();
        // The statistic of gap 52: runs stopped or failed whose node shows
        // something else. Logged when the set changes, and every few minutes
        // while any stands; a failed check is an error of the tick.
        match store.read(|conn| tod_core::stop_mismatch::find_with(conn, &standings, now)) {
            Ok(found) => self.log_stop_mismatch(&found, now),
            Err(err) => errors.push(format!("check for stopped runs shown as something else: {err:#}")),
        }

        let rows = store.read(|conn| DeferralRepo::new(conn).list_all()).context("read the deferrals")?;
        for node in next_actor::spent_deferrals(&rows, &standings, now) {
            if let Err(err) = tod_core::defer::undefer(store, node) {
                errors.push(format!("clear the spent deferral of {node}: {err:#}"));
            }
        }

        self.drop_deleted_slots(&mut errors);
        let nodes: Vec<Uuid> = self.inner.slots.lock().expect("runner slots").keys().copied().collect();
        let mut places: HashMap<Uuid, RunnerPlace> = HashMap::new();
        for node in &nodes {
            match RunnerPlace::of(store, *node) {
                Ok(place) => {
                    places.insert(*node, place);
                }
                // Without its place the node is left alone this tick.
                Err(err) => errors.push(format!("{node}: {err:#}")),
            }
        }
        // A run that ended `Done` says so only while the node is `done`. A
        // node that is in any other state again (moved back, or reopened) has
        // no run that ended: keeping the old word would leave it reading
        // "not started" with nothing to start it, in whatever phase it is in.
        for node in self.stale_done_nodes() {
            match tod_core::lifecycle::current_state(store, node) {
                Ok(state) if state != "done" => {
                    tracing::info!(%node, %state, "the node left done; its finished run no longer counts");
                    if let Some(slot) = self.inner.slots.lock().expect("runner slots").get_mut(&node)
                        && matches!(slot.outcome, Some(Outcome::Done))
                    {
                        slot.outcome = None;
                    }
                }
                Ok(_) => {}
                Err(err) => errors.push(format!("{node}: read the lifecycle state: {err:#}")),
            }
        }
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
                    let view = Self::view(slot, *places.get(node)?);
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

    /// Nodes whose idle slot holds a `Done` outcome.
    /// Whether the store still has `node`; a read failure is an error, never
    /// "absent".
    fn node_exists(&self, node: Uuid) -> Result<bool> {
        Ok(self.inner.store.get_node(&node.to_string())?.is_some())
    }

    /// Drop idle slots of nodes deleted from the store (once each, as the
    /// slot is gone afterwards). A read failure is collected, the slot kept.
    fn drop_deleted_slots(&self, errors: &mut Vec<String>) {
        let idle: Vec<Uuid> = self
            .inner
            .slots
            .lock()
            .expect("runner slots")
            .iter()
            .filter(|(_, s)| s.run.is_none() && !s.starting)
            .map(|(n, _)| *n)
            .collect();
        for node in idle {
            match self.node_exists(node) {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(%node, "the node no longer exists; its runner slot is dropped");
                    self.inner.slots.lock().expect("runner slots").remove(&node);
                }
                Err(err) => errors.push(format!("{node}: look the node up: {err:#}")),
            }
        }
    }

    fn stale_done_nodes(&self) -> Vec<Uuid> {
        self.inner
            .slots
            .lock()
            .expect("runner slots")
            .iter()
            .filter(|(_, s)| s.run.is_none() && !s.starting && matches!(s.outcome, Some(Outcome::Done)))
            .map(|(n, _)| *n)
            .collect()
    }

    fn view(slot: &Slot, place: RunnerPlace) -> SlotView {
        SlotView {
            place,
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
                let started = self.start_auto(node);
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
            Action::MovedToSandbox => {
                let err = anyhow!(SUPERVISED);
                tracing::error!(%node, "desired to run here, but: {err:#}");
                self.record_start_failure(node, &err);
                Err(err)
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
        let mut reason = format!("{FAILED_PREFIX}{error:#}");
        let root = self.inner.store.paths().root().to_path_buf();
        // The run's row says the same, so an older "waiting on a request"
        // never outlives the failed start.
        let recorded = tod_core::run_state::record(
            &self.inner.store,
            node,
            tod_core::run_state::runner_failed(&reason[FAILED_PREFIX.len()..]),
            "local",
        );
        if let Err(err) = &recorded {
            // Said on the node too (in the outcome's reason), and the slot's
            // outcome stops the reconciler and the stale-wait sweep from
            // starting it again every tick.
            tracing::error!(%node, "could not record the failure of the run's start in its run state: {err:#}");
            reason = format!("{reason} (and the run state could not be recorded: {err:#})");
        }
        let outcome = Outcome::Stopped { reason: reason.clone() };
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
        self.start_as(node, renew_budget, true)
    }

    /// A start nobody asked for (a restart, a due wait, an answered request):
    /// a stop at a round limit stays in force.
    fn start_auto(&self, node: Uuid) -> Result<()> {
        self.start_as(node, false, false)
    }

    fn start_as(&self, node: Uuid, renew_budget: bool, user_go_on: bool) -> Result<()> {
        // Refused before anything is recorded: the run is not this daemon's, so
        // a failure outcome saved here would only mask the supervisor's.
        match RunnerPlace::of(&self.inner.store, node)? {
            RunnerPlace::Local => {}
            RunnerPlace::Supervisor => return Err(anyhow!(SUPERVISED)),
        }
        // Nothing automatic resumes what the user paused or stopped: only
        // their Start or Resume (`user_go_on`) does. Read from the saved
        // state, so it holds across a restart and for a node with no slot.
        if !user_go_on {
            let root = self.inner.store.paths().root().to_path_buf();
            let saved = AutopilotState::load(&root, node)
                .with_context(|| format!("read the saved run of {node} to see whether the user paused it"))?;
            if saved.outcome.as_ref().is_some_and(tod_core::autopilot::local::stopped_by_user) {
                tracing::info!(%node, "not starting the run on its own: the user paused or stopped it");
                return Ok(());
            }
        }
        {
            let mut slots = self.inner.slots.lock().expect("runner slots");
            let slot = slots.entry(node).or_default();
            slot.desired = Desired::Run;
            if slot.run.is_some() || slot.starting {
                return Ok(());
            }
            slot.starting = true;
        }
        match self.spawn(node, renew_budget, user_go_on) {
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

    fn spawn(&self, node: Uuid, renew_budget: bool, user_go_on: bool) -> Result<LocalRun> {
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
            user_go_on,
            move |event| Inner::on_event(&inner, node, event),
        )
        .map_err(|e| anyhow!("Starting the runner failed: {e:#}"))
    }

    /// Stop once the agent's turn ends. With no run alive the pause is
    /// written to the node's saved state and run state before this returns
    /// (a live run writes it itself as it ends): a daemon that restarts, or
    /// is replaced by a newer one, still holds it, and nothing automatic
    /// resumes the node until the user's Start or Resume. A failure to write
    /// it is returned, never only logged.
    pub fn pause(&self, node: Uuid) -> Result<()> {
        self.user_stop(node, Desired::Pause)
    }

    /// The one place a user's pause or stop is set: the app's runner menu
    /// (in process or through the daemon API) and `tod-cli runner pause` /
    /// `stop` all come here.
    fn user_stop(&self, node: Uuid, desired: Desired) -> Result<()> {
        let reason = match desired {
            Desired::Stop => tod_core::autopilot::local::STOPPED,
            _ => tod_core::autopilot::local::PAUSED,
        };
        let idle = {
            let mut slots = self.inner.slots.lock().expect("runner slots");
            let slot = slots.entry(node).or_default();
            slot.desired = desired;
            match &slot.run {
                Some(run) => {
                    match desired {
                        Desired::Stop => run.stop_now(),
                        _ => run.pause(),
                    }
                    false
                }
                None => !slot.starting,
            }
        };
        let saved = if idle { self.save_user_stop(node, reason) } else { Ok(()) };
        self.inner.publish(node);
        saved
    }

    /// Write a user's pause or stop of a node with no live run: its saved
    /// state's outcome (read by `resume_saved` and `start_as`) and its run
    /// state (what the runner line shows and the stale-wait sweep reads).
    fn save_user_stop(&self, node: Uuid, reason: &str) -> Result<()> {
        let root = self.inner.store.paths().root().to_path_buf();
        let outcome = Outcome::Stopped { reason: reason.to_string() };
        let mut state = AutopilotState::load(&root, node).context("read the run's saved state")?;
        state.outcome = Some(outcome.clone());
        state.save(&root, node).context("save the pause or stop in the run's saved state")?;
        tod_core::run_state::record(
            &self.inner.store,
            node,
            tod_core::run_state::for_outcome(&outcome, tod_core::run_state::Words::local()),
            "local",
        )
        .context("record the pause or stop in the run's state")?;
        let mut slots = self.inner.slots.lock().expect("runner slots");
        let slot = slots.entry(node).or_default();
        // Only an idle slot: a run started meanwhile writes its own outcome.
        if slot.run.is_none() && !slot.starting {
            slot.outcome = Some(outcome);
        }
        Ok(())
    }

    /// Read GitHub now, for a run that is waiting on it.
    pub fn poll_now(&self, node: Uuid) {
        if let Some(run) = self.inner.slots.lock().expect("runner slots").get(&node).and_then(|s| s.run.as_ref()) {
            run.poll_now();
        }
    }

    /// Cancel the turn in flight and stop.
    pub fn stop_now(&self, node: Uuid) -> Result<()> {
        self.user_stop(node, Desired::Stop)
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

    /// Hand every run over: each stops at its next boundary, and waits for
    /// them: a turn in flight gets [`DRAIN_PATIENCE`] to end before it is
    /// cancelled. This is never the user's Pause or Stop: nothing is recorded
    /// as desired, a run the user already paused or stopped keeps that, and a
    /// handed-over run keeps no outcome, so the next daemon continues it.
    pub fn drain(&self) {
        let nodes: Vec<Uuid> = {
            let slots = self.inner.slots.lock().expect("runner slots");
            slots.iter().filter(|(_, s)| s.run.is_some()).map(|(n, _)| *n).collect()
        };
        if nodes.is_empty() {
            return;
        }
        self.hand_over_all(&nodes, false);
        if self.wait_idle(DRAIN_PATIENCE) {
            return;
        }
        self.hand_over_all(&nodes, true);
        self.wait_idle(DRAIN_CANCELLED);
    }

    /// Ask each node's run to hand over; `desired` is not touched.
    fn hand_over_all(&self, nodes: &[Uuid], cancel_turn: bool) {
        let slots = self.inner.slots.lock().expect("runner slots");
        for node in nodes {
            if let Some(run) = slots.get(node).and_then(|s| s.run.as_ref()) {
                if cancel_turn {
                    run.hand_over_now();
                } else {
                    run.hand_over();
                }
            }
        }
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
                    // The node's work ended: its idle sandbox is retired now.
                    tod_core::sandbox_retire::poke();
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
        SlotView { desired, starting: false, active_run: false, outcome_none: true, place: RunnerPlace::Local }
    }

    #[test]
    fn a_desired_run_on_a_sandbox_node_is_reported_never_started() {
        let mut v = view(Desired::Run);
        v.place = RunnerPlace::Supervisor;
        let standing = you(YouReason::NotStarted);
        assert_eq!(decide(&v, Some(&standing), &[], 1_000), Some(Action::MovedToSandbox));
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
        assert_eq!(decide(&v, Some(&you(YouReason::Request { count: 1, ended: None })), &[], NOW), None, "an open request");
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
        let root = tod_test_util::scratch_dir(format!("tod-agentd-runners-{}", Uuid::new_v4()));
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
    fn a_finished_run_does_not_hold_a_node_that_is_no_longer_done() {
        let fx = fixture();
        {
            let mut slots = fx.runners.inner.slots.lock().unwrap();
            let slot = slots.entry(fx.node).or_default();
            slot.desired = Desired::Run;
            slot.outcome = Some(Outcome::Done);
        }
        // Still `done`-or-not is the lifecycle's word: the node is in its
        // first state, so the old "done" is stale and the reconciler starts
        // it (which fails here for want of media, and says so).
        let err = fx.runners.reconcile().unwrap_err();
        assert!(format!("{err:#}").contains("Media bundle"), "{err:#}");
        assert!(matches!(standing_of(&fx).actor, NextActor::You { why: YouReason::Failed { .. }, .. }));
    }

    #[test]
    fn a_saved_done_run_for_a_node_absent_from_the_store_holds_no_slot_and_is_no_error() {
        let fx = fixture();
        let ghost = Uuid::new_v4();
        AutopilotState { outcome: Some(Outcome::Done), ..AutopilotState::fresh() }.save(&fx.root, ghost).unwrap();
        fx.runners.resume_saved();
        assert!(!fx.runners.inner.slots.lock().unwrap().contains_key(&ghost));
        // A slot made before the node vanished is dropped, with no error.
        fx.runners.inner.slots.lock().unwrap().entry(ghost).or_default().outcome = Some(Outcome::Done);
        fx.runners.reconcile().unwrap();
        fx.runners.reconcile().unwrap();
        assert!(!fx.runners.inner.slots.lock().unwrap().contains_key(&ghost));
        // The saved file is left alone.
        assert!(fx.root.join("autopilot").join(format!("{ghost}.json")).exists());
    }

    /// Gap 58: a run stopped on a question whose answer this daemon never saw
    /// pending (answered while it was down, or read from its saved state) kept
    /// "waiting on 1 question(s)" until someone pressed Start.
    #[test]
    fn a_run_stopped_on_a_question_that_is_no_longer_pending_continues_though_never_seen_waiting() {
        let fx = fixture();
        {
            let mut slots = fx.runners.inner.slots.lock().unwrap();
            let slot = slots.entry(fx.node).or_default();
            slot.outcome = Some(Outcome::NeedsHuman { reason: tod_core::autopilot::NeedsHuman::Decision { pending: 1 } });
            assert!(!slot.seen_waiting);
        }
        fx.runners.start_due();
        // The run was started again (it fails here for want of media, and says
        // so), so the stale question text is gone.
        let slots = fx.runners.inner.slots.lock().unwrap();
        let slot = slots.get(&fx.node).unwrap();
        assert!(
            matches!(&slot.outcome, Some(Outcome::Stopped { reason }) if reason.starts_with(FAILED_PREFIX)),
            "{:?}",
            slot.outcome
        );
    }

    /// Gap 67: the run row said "waiting on 1 question(s)" for 27 hours after
    /// the request was answered, because this daemon's slot held no outcome
    /// for the node (restart, sync). The row alone is enough to continue it,
    /// whether the request was answered or withdrawn.
    #[test]
    fn a_request_wait_row_with_no_request_pending_is_left_without_a_user_action() {
        use tod_core::run_state::{kind, record};
        use tod_store::interview::InterviewCommand;
        use tod_store::run_state::{Draft, Phase};
        for how in ["answered", "withdrawn"] {
            let fx = fixture();
            let store = &fx.runners.inner.store;
            let stale = || tod_core::run_state::stale_request_waits(store).unwrap().iter().any(|w| w.node == fx.node);
            record(
                store,
                fx.node,
                Draft {
                    phase: Phase::Waiting,
                    reason_kind: kind::REQUEST.into(),
                    reason_text: "the agent is waiting on 1 question(s) only you can answer".into(),
                    resumable_by: tod_store::run_state::RESUMABLE_BY_YOU.into(),
                },
                "local",
            )
            .unwrap();
            let asked = store
                .interview(
                    "agent",
                    InterviewCommand::AskDecision {
                        node_id: fx.node,
                        conversation_id: None,
                        protocol: None,
                        decision: tod_store::decisions::NewDecision {
                            question: "heap?".into(),
                            options: vec!["1g".into(), "2g".into()],
                            ..Default::default()
                        },
                    },
                )
                .unwrap();
            store.reload_if_stale().unwrap();
            assert!(!stale(), "a pending request is a real wait");
            fx.runners.resume_stale_request_waits();
            let row = store.read(|c| tod_store::run_state::get(c, fx.node)).unwrap().unwrap();
            assert_eq!(row.reason_kind, kind::REQUEST, "left alone while pending");
            let decision_id: Uuid = asked["id"].as_str().unwrap().parse().unwrap();
            let command = if how == "answered" {
                InterviewCommand::AnswerDecision { decision_id, option: Some(1), text: None }
            } else {
                InterviewCommand::WithdrawDecision { decision_id }
            };
            store.interview("user", command).unwrap();
            store.reload_if_stale().unwrap();
            assert!(stale(), "{how}: the row still says it waits");
            fx.runners.resume_stale_request_waits();
            // The run was started again (it fails here for want of media, and
            // says so), so the row no longer says it waits on a request.
            let row = store.read(|c| tod_store::run_state::get(c, fx.node)).unwrap().unwrap();
            assert_ne!(row.reason_kind, kind::REQUEST, "{how}: {row:?}");
            assert!(!stale(), "{how}");
        }
    }

    #[test]
    fn a_run_the_user_paused_is_not_continued_by_a_question_being_gone() {
        let fx = fixture();
        {
            let mut slots = fx.runners.inner.slots.lock().unwrap();
            let slot = slots.entry(fx.node).or_default();
            slot.desired = Desired::Pause;
            slot.outcome = Some(Outcome::NeedsHuman { reason: tod_core::autopilot::NeedsHuman::Decision { pending: 1 } });
        }
        fx.runners.start_due();
        let slots = fx.runners.inner.slots.lock().unwrap();
        assert!(matches!(slots.get(&fx.node).unwrap().outcome, Some(Outcome::NeedsHuman { .. })));
    }

    #[test]
    fn a_saved_done_run_on_a_node_that_is_not_done_is_started_after_a_restart() {
        let fx = fixture();
        AutopilotState { outcome: Some(Outcome::Done), ..AutopilotState::fresh() }.save(&fx.root, fx.node).unwrap();
        fx.runners.resume_saved();
        // The reconciler (not a hand-set `desired`) clears the stale word and
        // starts the run, which fails here for want of media, and says so.
        let err = fx.runners.reconcile().unwrap_err();
        assert!(format!("{err:#}").contains("Media bundle"), "{err:#}");
        assert!(matches!(standing_of(&fx).actor, NextActor::You { why: YouReason::Failed { .. }, .. }));
    }

    #[test]
    fn a_saved_done_run_on_a_done_node_is_not_started_after_a_restart() {
        let fx = fixture();
        tod_core::lifecycle::set_lifecycle(&fx.runners.inner.store, fx.node, "done").unwrap();
        AutopilotState { outcome: Some(Outcome::Done), ..AutopilotState::fresh() }.save(&fx.root, fx.node).unwrap();
        fx.runners.resume_saved();
        fx.runners.reconcile().unwrap();
        let slots = fx.runners.inner.slots.lock().unwrap();
        assert!(matches!(slots[&fx.node].outcome, Some(Outcome::Done)));
        assert_eq!(slots[&fx.node].desired, Desired::Unset);
    }

    #[test]
    fn a_finished_run_still_holds_a_node_that_is_done() {
        let fx = fixture();
        tod_core::lifecycle::set_lifecycle(&fx.runners.inner.store, fx.node, "done").unwrap();
        {
            let mut slots = fx.runners.inner.slots.lock().unwrap();
            let slot = slots.entry(fx.node).or_default();
            slot.desired = Desired::Run;
            slot.outcome = Some(Outcome::Done);
        }
        fx.runners.reconcile().unwrap();
        let slots = fx.runners.inner.slots.lock().unwrap();
        assert!(matches!(slots[&fx.node].outcome, Some(Outcome::Done)));
    }

    #[test]
    fn a_working_row_whose_lease_owner_is_dead_is_adopted_on_the_next_tick_with_no_slot() {
        let fx = fixture();
        let store = &fx.runners.inner.store;
        let long_ago = now_ms() - 7 * 3_600_000;
        // What the live data showed: the row says working since hours ago,
        // written by the local runner; its lease belongs to a dead daemon;
        // and this daemon holds no slot for the node.
        store
            .interview(
                tod_core::run_state::ACTOR,
                tod_store::interview::InterviewCommand::SetRunState {
                    node_id: fx.node,
                    draft: tod_core::run_state::working("running on this machine"),
                    writer: "local".into(),
                    at: long_ago,
                },
            )
            .unwrap();
        store
            .enqueue(FleetMutation::ActivityStart {
                id: Uuid::new_v4(),
                node: fx.node,
                kind: tod_store::activity::KIND_RUN.into(),
                owner: "tod-agentd-768aa82677b906a7:19909".into(),
                at: long_ago,
            })
            .unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().unwrap();
        assert!(fx.runners.inner.slots.lock().unwrap().get(&fx.node).is_none());
        let row = store.read(|c| tod_store::run_state::get(c, fx.node)).unwrap().unwrap();
        assert_eq!(row.phase, tod_store::run_state::Phase::Working);

        fx.runners.tick();
        store.reload_if_stale().unwrap();

        // Adopted: a start was made (it fails here for want of media, and the
        // node says so). Never left `working`, and never shown as working.
        let row = store.read(|c| tod_store::run_state::get(c, fx.node)).unwrap().unwrap();
        assert_ne!(row.phase, tod_store::run_state::Phase::Working, "{row:?}");
        let NextActor::You { why: YouReason::Failed { error }, .. } = standing_of(&fx).actor else {
            panic!("the node should be failed, not working")
        };
        assert!(error.contains("Media bundle"), "{error}");
        let leases = store.read(|c| ActivityRepo::new(c).list_for_nodes(&[fx.node])).unwrap();
        assert!(leases.iter().all(|l| l.owner != "tod-agentd-768aa82677b906a7:19909"), "the dead owner's lease is cleared");
    }

    #[test]
    fn a_cut_off_working_row_the_user_paused_is_recorded_not_resumed() {
        let fx = fixture();
        let store = &fx.runners.inner.store;
        store
            .interview(
                tod_core::run_state::ACTOR,
                tod_store::interview::InterviewCommand::SetRunState {
                    node_id: fx.node,
                    draft: tod_core::run_state::working("running on this machine"),
                    writer: "local".into(),
                    at: now_ms() - 3_600_000,
                },
            )
            .unwrap();
        {
            let mut slots = fx.runners.inner.slots.lock().unwrap();
            slots.entry(fx.node).or_default().desired = Desired::Pause;
        }
        fx.runners.adopt_cut_off_runs();
        let slots = fx.runners.inner.slots.lock().unwrap();
        // The user's pause holds: nothing is started.
        assert!(slots[&fx.node].run.is_none() && !slots[&fx.node].starting);
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

    /// Give the fixture's node Files that run in a cloud sandbox.
    fn move_to_sandbox(fx: &Fixture) {
        use tod_store::fleet::{DevContainerSetting, FilesPlace};
        use tod_store::outline::Capability;
        let fleet = &fx.runners.inner.store;
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: fx.node,
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
        fleet.enqueue(FleetMutation::SaveFilesPlace { place }).unwrap();
        fleet
            .enqueue(FleetMutation::SetNodeFilesPlace { node_id: fx.node.to_string(), place_id: Some(id) })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        assert_eq!(RunnerPlace::of(fleet, fx.node).unwrap(), RunnerPlace::Supervisor);
    }

    #[test]
    fn a_sandbox_node_is_refused_by_start_and_nothing_is_recorded() {
        let fx = fixture();
        move_to_sandbox(&fx);
        let err = fx.runners.start(fx.node, false).unwrap_err();
        assert!(format!("{err:#}").contains("supervisor"), "{err:#}");
        assert!(!fx.runners.inner.slots.lock().unwrap().contains_key(&fx.node));
        assert!(AutopilotState::load(&fx.root, fx.node).map_or(true, |s| s.outcome.is_none()));
    }

    #[test]
    fn a_sandbox_node_is_not_resumed_and_a_cut_off_local_run_is_reported() {
        let fx = fixture();
        AutopilotState::fresh().save(&fx.root, fx.node).unwrap();
        move_to_sandbox(&fx);
        fx.runners.resume_saved();
        // Not started, not kept as a slot, and shown on the node as failed.
        let slots = fx.runners.inner.slots.lock().unwrap();
        let slot = slots.get(&fx.node).expect("the failure is recorded");
        assert!(slot.run.is_none() && !slot.starting);
        assert!(matches!(slot.outcome, Some(Outcome::Stopped { ref reason }) if reason.contains("supervisor")));
        drop(slots);
        let saved = AutopilotState::load(&fx.root, fx.node).unwrap();
        assert!(matches!(saved.outcome, Some(Outcome::Stopped { ref reason }) if reason.contains("supervisor")));
    }

    #[test]
    fn the_reconciler_does_not_restart_a_node_that_moved_to_a_sandbox() {
        let fx = fixture();
        {
            let mut slots = fx.runners.inner.slots.lock().unwrap();
            slots.entry(fx.node).or_default().desired = Desired::Run;
        }
        move_to_sandbox(&fx);
        let err = fx.runners.reconcile().unwrap_err();
        assert!(format!("{err:#}").contains("supervisor"), "{err:#}");
        assert!(matches!(standing_of(&fx).actor, NextActor::You { why: YouReason::Failed { .. }, .. }));
        let slots = fx.runners.inner.slots.lock().unwrap();
        assert!(slots[&fx.node].run.is_none());
        drop(slots);
        // Reported once: the outcome now explains the node.
        fx.runners.reconcile().unwrap();
    }

    /// Gap 61: the old daemon's drain on a handover paused, then stopped,
    /// every live run as the user would: the nodes read "you stopped it".
    #[test]
    fn a_drain_for_a_handover_is_not_a_user_pause_or_stop_and_the_next_daemon_continues_the_run() {
        let fx = fixture_with_media(true);
        fx.runners.start(fx.node, false).unwrap();
        fx.runners.drain();
        {
            let slots = fx.runners.inner.slots.lock().unwrap();
            assert_eq!(slots[&fx.node].desired, Desired::Run, "a drain records no pause or stop");
            assert!(slots[&fx.node].run.as_ref().is_none_or(|r| r.is_finished()));
        }
        let saved = AutopilotState::load(&fx.root, fx.node).unwrap();
        assert!(
            !saved.outcome.as_ref().is_some_and(tod_core::autopilot::local::stopped_by_user),
            "never saved as the user's stop: {:?}",
            saved.outcome
        );
        fx.runners.inner.store.writer().flush().unwrap();
        fx.runners.inner.store.reload_if_stale().unwrap();
        let row = fx.runners.inner.store.read(|c| tod_store::run_state::get(c, fx.node)).unwrap().unwrap();
        assert!(!matches!(row.reason_kind.as_str(), "stopped_now" | "paused"), "{row:?}");
        // A handed-over run has no outcome, which the next daemon resumes;
        // one that ended on its own in the meantime keeps its own.
        if saved.outcome.is_none() {
            let agent: SharedAgent = Arc::new(Mutex::new(Box::new(MockAgentProvider::new())));
            let next = Runners::new(fx.runners.inner.store.clone(), agent);
            next.resume_saved();
            let slots = next.inner.slots.lock().unwrap();
            assert!(slots.contains_key(&fx.node), "the next daemon holds the run");
            assert_eq!(slots[&fx.node].desired, Desired::Run);
        }
    }

    /// A real user pause is saved as the run's outcome and holds across a
    /// handover: the next daemon does not start the node.
    #[test]
    fn a_user_pause_holds_across_a_handover() {
        let fx = fixture_with_media(true);
        AutopilotState {
            outcome: Some(Outcome::Stopped { reason: tod_core::autopilot::local::PAUSED.into() }),
            ..AutopilotState::fresh()
        }
        .save(&fx.root, fx.node)
        .unwrap();
        fx.runners.resume_saved();
        fx.runners.reconcile().unwrap();
        let slots = fx.runners.inner.slots.lock().unwrap();
        let slot = &slots[&fx.node];
        assert!(slot.run.is_none() && !slot.starting);
        assert!(matches!(&slot.outcome, Some(Outcome::Stopped { reason }) if reason == tod_core::autopilot::local::PAUSED));
    }

    /// Gap 72: a pause was only the slot's in-memory `desired`, so a node
    /// parked on a request with no live run forgot it at the next deploy, and
    /// the stale-wait sweep resumed it once the request was gone.
    #[test]
    fn a_pause_on_a_node_parked_on_a_request_survives_a_restart_and_only_resume_clears_it() {
        use tod_core::run_state::{kind, record};
        use tod_store::interview::InterviewCommand;
        use tod_store::run_state::{Draft, Phase};
        for stop in [false, true] {
            let fx = fixture();
            let store = fx.runners.inner.store.clone();
            let agent = fx.runners.inner.agent.clone();
            let waiting = |text: &str| Draft {
                phase: Phase::Waiting,
                reason_kind: kind::REQUEST.into(),
                reason_text: text.into(),
                resumable_by: tod_store::run_state::RESUMABLE_BY_YOU.into(),
            };
            record(&store, fx.node, waiting("the agent is waiting on 1 question(s) only you can answer"), "local").unwrap();
            let asked = store
                .interview(
                    "agent",
                    InterviewCommand::AskDecision {
                        node_id: fx.node,
                        conversation_id: None,
                        protocol: None,
                        decision: tod_store::decisions::NewDecision {
                            question: "heap?".into(),
                            options: vec!["1g".into(), "2g".into()],
                            ..Default::default()
                        },
                    },
                )
                .unwrap();
            // The user pauses (or stops) it: nothing is running.
            if stop { fx.runners.stop_now(fx.node) } else { fx.runners.pause(fx.node) }.unwrap();
            let want = if stop { tod_core::autopilot::local::STOPPED } else { tod_core::autopilot::local::PAUSED };
            let held = |fx: &Fixture| {
                matches!(
                    AutopilotState::load(&fx.root, fx.node).unwrap().outcome,
                    Some(Outcome::Stopped { ref reason }) if reason == want
                )
            };
            assert!(held(&fx), "saved");
            store.reload_if_stale().unwrap();
            let row = store.read(|c| tod_store::run_state::get(c, fx.node)).unwrap().unwrap();
            assert_ne!(row.reason_kind, kind::REQUEST, "the runner line shows the pause: {row:?}");
            // A new daemon, with only what is stored.
            let next = Runners::new(store.clone(), agent.clone());
            next.resume_saved();
            {
                let slots = next.inner.slots.lock().unwrap();
                assert_eq!(slots[&fx.node].desired, if stop { Desired::Stop } else { Desired::Pause });
            }
            // The request is answered; every automatic path ticks.
            let decision_id: Uuid = asked["id"].as_str().unwrap().parse().unwrap();
            store.interview("user", InterviewCommand::AnswerDecision { decision_id, option: Some(1), text: None }).unwrap();
            store.reload_if_stale().unwrap();
            next.start_due();
            next.resume_stale_request_waits();
            next.reconcile().unwrap();
            assert!(held(&fx), "not resumed by the answer");
            assert!(next.inner.slots.lock().unwrap()[&fx.node].run.is_none());
            // Even a daemon with no slot and a row that still says "waiting
            // on a request" (synced from elsewhere) leaves it alone.
            record(&store, fx.node, waiting("stale"), "local").unwrap();
            let bare = Runners::new(store.clone(), agent);
            bare.resume_stale_request_waits();
            assert!(held(&fx), "the sweep does not resume it");
            assert!(bare.inner.slots.lock().unwrap().get(&fx.node).is_none_or(|s| s.run.is_none() && !s.starting));
            // Only the user's Resume runs it (here it fails for want of media,
            // which proves it was attempted).
            let err = next.start(fx.node, false).unwrap_err();
            assert!(format!("{err:#}").contains("Media bundle"), "{err:#}");
            assert!(!held(&fx));
            assert_eq!(next.inner.slots.lock().unwrap()[&fx.node].desired, Desired::Run);
        }
    }

    #[test]
    fn pause_and_stop_are_the_desired_state() {
        let fx = fixture();
        let desired = |fx: &Fixture| fx.runners.inner.slots.lock().unwrap().get(&fx.node).map(|s| s.desired);
        fx.runners.pause(fx.node).unwrap();
        assert_eq!(desired(&fx), Some(Desired::Pause));
        fx.runners.stop_now(fx.node).unwrap();
        assert_eq!(desired(&fx), Some(Desired::Stop));
        let _ = fx.runners.start(fx.node, false);
        assert_eq!(desired(&fx), Some(Desired::Run));
    }
}
