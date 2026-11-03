//! `tod-supervisor`: runs one autonomous node in its cloud sandbox.
//!
//! `tod-supervisor wake` (started by the relay's poke, from a schedule, or
//! when the sandbox is provisioned) asks what the node is waiting on. If it
//! is still waiting, it schedules the next check and exits. Otherwise it
//! runs the lifecycle
//! [`Autopilot`] for the node with the agent started here, and when that
//! stops — the node is done, needs the user, or recorded a wait — pushes the
//! branch, schedules the wake a wait needs, and exits. It holds the sandbox
//! awake through the relay for the whole wake, from before it syncs its
//! local copy ([`hold`]) until it exits.
//!
//! The autopilot runs against a local copy of the user's database that is
//! synced with the orchestrator around every step and turn ([`replica`]).
//! Claude's session files are mirrored as they are written
//! ([`transcripts`]), and restored first on a new sandbox so its sessions
//! resume. A `SIGUSR1` (a poke while it runs) is taken at the next stopping
//! point, and so is a context change the orchestrator marked on the node
//! ([`context`]): the autopilot stops there, the node is moved back if its
//! state no longer holds, the conversation's session is ended so the next
//! turn gets the new context, and the autopilot continues.
//!
//! When it stops for repeated failures or a spent budget it asks the user
//! (`tod_core::stop_questions`); each wake first reads the answer to the
//! latest such question, or the watchdog's ([`answers`]).
//!
//! Design: `doc/cloud-sandboxes/autonomous-nodes.md` ("The supervisor and
//! waiting", "Holding the sandbox awake", "Transcripts").

pub mod agent;
pub mod answers;
pub mod beat;
pub mod context;
pub mod git;
pub mod guard;
pub mod hold;
pub mod lock;
pub mod orchestrator;
pub mod problems;
pub mod progress;
pub mod replica;
pub mod signal;
pub mod transcripts;
pub mod usage_limit;
pub mod waits;

use agent::{AgentKind, Syncing};
use anyhow::{Context, Result};
use hold::{HoldGuard, Holder};
use replica::Replica;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tod_agent::{AgentLaunchOptions, AgentPlatform};
use guard::{Guarded, Guards, OnFailure};
use tod_core::autopilot::{Autopilot, Boundary, Budget, BudgetLimit, NeedsHuman, Outcome, StepHook};
use tod_core::conversation::driver::{AgentAccess, ConversationConfig, ConversationDriver};
use tod_core::media::MediaPaths;
use tod_store::fleet::FleetStore;
use tod_store::settings::InterviewContextSettings;
use transcripts::{Mirror, TranscriptStore};
use uuid::Uuid;

pub struct Config {
    pub orchestrator: orchestrator::Orchestrator,
    pub node: Uuid,
    /// The node's checkout (`/workspace/repo`).
    pub workspace: PathBuf,
    /// The local copy and the autopilot's state (`/var/lib/tod-supervisor/<node>`).
    pub state_dir: PathBuf,
    pub agent: AgentKind,
    pub holder: Arc<dyn Holder>,
    /// Claude's projects directory and where its sessions are mirrored.
    pub transcripts: Option<(PathBuf, Arc<dyn TranscriptStore>)>,
    pub media: MediaPaths,
    pub budget: Budget,
    /// Hung agents and repeated failures (design: "Crash guards").
    pub guards: Guards,
    /// Between polls of a turn in flight.
    pub poll: Duration,
    /// Push the branch after each step and at the end.
    pub push_branch: bool,
    /// Wakes the sandbox when a wait is due (`tod_core::scheduler`).
    pub scheduler: Option<Arc<dyn tod_core::scheduler::Scheduler>>,
    /// This sandbox's name, for the scheduler.
    pub sandbox: String,
}

/// How a wake ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Woke {
    /// It was still waiting; nothing ran.
    StillWaiting(String),
    /// The user answered its latest stop question to leave it stopped;
    /// nothing ran, and no wake is scheduled.
    LeftStopped(String),
    /// The autopilot ran and stopped.
    Ran(Outcome),
    /// Another supervisor holds this node's lock ([`lock`]): this one did
    /// nothing, changed no schedule, and poked the holder to look again.
    AlreadyRunning { holder: Option<u32> },
}

/// The autopilot's hook: sync at every boundary, and stop for a wait.
struct Hook<'a> {
    replica: &'a Arc<Mutex<Replica>>,
    node: Uuid,
    workspace: &'a std::path::Path,
    push_branch: bool,
    /// The supervisor's state directory, where the app leaves a request for
    /// a new session ([`fresh_requested`]).
    state_dir: &'a std::path::Path,
    /// A request for a new session found, to take.
    fresh_pending: bool,
    /// Who consumes requests (`consumed_by`).
    consumer: String,
    /// The last context mark taken.
    context_seen: i64,
    /// A newer mark found at the last boundary, to take.
    context_pending: Option<i64>,
    /// Proof of work moving, for the hold and the heartbeat ([`progress`]).
    progress: &'a progress::Progress,
    /// The reply as it was when it last moved.
    last_parts: Vec<tod_agent::ReplyPart>,
    /// The reply in flight last sent to the app, and when.
    live_sent: Option<(std::time::Instant, Vec<tod_agent::ReplyPart>)>,
}

/// The least time between two sends of the reply in flight: each carries the
/// whole reply, and the app only looks every few seconds.
const LIVE_EVERY: Duration = Duration::from_secs(3);

/// The reason the autopilot is stopped with to take a request for a new session.
pub const FRESH_SESSION_REQUESTED: &str = "new session requested";

/// Whether the legacy marker file asking for a new session is there (a
/// supervisor before schema v89 read only this; the app still writes it
/// beside the request row, `tod_store::node_requests`).
fn fresh_marker(state_dir: &std::path::Path) -> bool {
    state_dir.join(tod_core::autopilot::FRESH_SESSION_REQUEST_FILE).exists()
}

/// The node's requests not yet consumed (`tod_store::node_requests`).
fn pending_requests(fleet: &FleetStore, node: Uuid) -> Result<Vec<tod_store::node_requests::NodeRequest>> {
    fleet.read(|conn| tod_store::node_requests::pending_for_node(conn, node))
}

/// Whether a new session was asked for: the marker file or a pending
/// `fresh_session` row.
fn fresh_requested(state_dir: &std::path::Path, fleet: &FleetStore, node: Uuid) -> Result<bool> {
    Ok(fresh_marker(state_dir)
        || tod_store::node_requests::any_of(&pending_requests(fleet, node)?, tod_store::node_requests::KIND_FRESH_SESSION))
}

/// Consumes `requests` (`by`, now): the record that the supervisor took them.
fn consume_requests(fleet: &FleetStore, requests: &[tod_store::node_requests::NodeRequest], by: &str) -> Result<()> {
    let at = waits_now_ms();
    for request in requests {
        fleet
            .interview(by, tod_store::interview::InterviewCommand::ConsumeNodeRequest {
                request_id: request.id,
                by: by.to_string(),
                at,
            })
            .map_err(|e| anyhow::anyhow!("consume the {} request {}: {e}", request.kind, request.id))?;
        tracing::info!(kind = %request.kind, id = %request.id, "consumed a request");
    }
    Ok(())
}

/// Takes the request for a new session: deletes the marker file (if any; an
/// error removing one that is there is returned, never left to be taken again
/// as a new request) and consumes the pending `fresh_session` rows.
fn take_fresh_request(state_dir: &std::path::Path, fleet: &FleetStore, node: Uuid, by: &str) -> Result<()> {
    let path = state_dir.join(tod_core::autopilot::FRESH_SESSION_REQUEST_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(anyhow::anyhow!("remove {}: {e}", path.display())),
    }
    let fresh: Vec<_> = pending_requests(fleet, node)?
        .into_iter()
        .filter(|r| r.kind == tod_store::node_requests::KIND_FRESH_SESSION)
        .collect();
    consume_requests(fleet, &fresh, by)
}

impl StepHook for Hook<'_> {
    fn watch(&mut self, turn: tod_core::autopilot::Turn<'_>) -> Option<String> {
        // The user wants this turn replaced: cancel it (the run keeps it as
        // current, and taking the request drops it).
        // Only the marker file is seen mid-turn: a request row arrives with
        // the next pull, at a boundary ([`StepHook::at`]).
        if fresh_marker(self.state_dir) {
            self.fresh_pending = true;
            return Some(FRESH_SESSION_REQUESTED.to_string());
        }
        if turn.status.parts != self.last_parts {
            // The agent said or did something: work is moving.
            self.progress.mark();
            self.last_parts = turn.status.parts.clone();
        }
        let (Some(conversation), parts) = (turn.conversation_id, &turn.status.parts) else {
            return None;
        };
        if parts.is_empty() {
            return None;
        }
        if let Some((at, sent)) = &self.live_sent
            && (at.elapsed() < LIVE_EVERY || sent == parts)
        {
            return None;
        }
        let sent = self
            .replica
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_live_turn(self.node, conversation, parts);
        match sent {
            // Kept even when it failed, so a broken send is retried every
            // `LIVE_EVERY`, not on every poll.
            Ok(()) => self.live_sent = Some((std::time::Instant::now(), parts.clone())),
            Err(err) => {
                tracing::warn!("sending the reply in flight: {err:#}");
                self.live_sent = Some((std::time::Instant::now(), Vec::new()));
            }
        }
        None
    }

    fn at(&mut self, fleet: &FleetStore, boundary: Boundary) -> Result<Option<String>> {
        // A turn ended or a step did: what was in flight is stored now, or
        // gone. The push below carries the deletion.
        self.live_sent = None;
        self.progress.mark();
        self.replica.lock().unwrap_or_else(|e| e.into_inner()).clear_live_turn(self.node)?;
        if signal::take_poke() {
            tracing::info!("poked: looking again");
        }
        {
            let mut replica = self.replica.lock().unwrap_or_else(|e| e.into_inner());
            replica.push()?;
            replica.pull()?;
        }
        if boundary == Boundary::Step && self.push_branch {
            if let Err(err) = git::push_branch(self.workspace) {
                tracing::warn!("pushing the branch: {err:#}");
            }
        }
        let pending = pending_requests(fleet, self.node)?;
        if fresh_marker(self.state_dir)
            || tod_store::node_requests::any_of(&pending, tod_store::node_requests::KIND_FRESH_SESSION)
        {
            self.fresh_pending = true;
            return Ok(Some(FRESH_SESSION_REQUESTED.to_string()));
        }
        // Any other request is taken here, at the first boundary after it
        // arrived: the run is going, which is all it asked for.
        consume_requests(fleet, &pending, &self.consumer)?;
        if let Some(at) = context::changed_since(fleet, self.node, self.context_seen)? {
            self.context_pending = Some(at);
            return Ok(Some(context::CONTEXT_CHANGED.to_string()));
        }
        Ok(match waits::check(fleet, self.node, self.workspace)? {
            waits::WaitStatus::Clear => None,
            waits::WaitStatus::Waiting { reason } => Some(format!("waiting: {reason}")),
        })
    }
}

/// Schedules the wake the node's waits need, if it has a scheduler, and
/// cancels one left from before that they no longer need.
fn schedule(store: &FleetStore, config: &Config) -> Result<()> {
    match &config.scheduler {
        Some(scheduler) => {
            waits::reconcile_wake(store, config.node, scheduler.as_ref(), &config.sandbox, &config.state_dir)?;
        }
        None => tracing::warn!("no scheduler: nothing will wake this node but a poke"),
    }
    Ok(())
}

/// The reason a run stopped for a usage limit (distinct from a failure).
pub fn usage_limit_reason(limit: &usage_limit::UsageLimit) -> String {
    format!("usage limit: waiting until {}", limit.reset_at_ms)
}

fn waits_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Hours for a sentence: "16", "1.5".
fn fmt_hours(h: f64) -> String {
    if (h - h.round()).abs() < 0.05 { format!("{}", h.round() as u64) } else { format!("{h:.1}") }
}

fn hours(d: Duration) -> String {
    fmt_hours(d.as_secs_f64() / 3600.0)
}

/// What the node spent of `budget` when `limit` stopped it.
fn budget_spent(limit: &BudgetLimit, budget: &Budget) -> (u32, f64) {
    match limit {
        BudgetLimit::Sessions { used, elapsed_secs } => (*used, *elapsed_secs as f64 / 3600.0),
        BudgetLimit::Time { elapsed_secs, used } => (*used, *elapsed_secs as f64 / 3600.0),
    }
}

fn budget_question(limit: &BudgetLimit, budget: &Budget) -> String {
    let (sessions, spent_hours) = budget_spent(limit, budget);
    let spent_hours = fmt_hours(spent_hours);
    format!(
        "Used {sessions} of {} sessions ({spent_hours} of {} hours). Keep going gives it {} more sessions \
         and {} more hours. Leave it stopped keeps it stopped until you change this answer.",
        budget.max_sessions,
        hours(budget.max_duration),
        budget.max_sessions,
        hours(budget.max_duration),
    )
}

/// A spent budget in plain words, with what was used and what answers it.
fn budget_reason(limit: &BudgetLimit, budget: &Budget) -> String {
    let (sessions, spent_hours) = budget_spent(limit, budget);
    format!(
        "its budget is spent ({sessions} of {} sessions, {} of {} hours). \
         Answer the budget question \"{}\" to give it more.",
        budget.max_sessions,
        fmt_hours(spent_hours),
        hours(budget.max_duration),
        tod_core::stop_questions::KEEP_GOING,
    )
}

/// Why a wake that ended with `outcome` leaves the node needing someone, in
/// plain words, for the app to show (`cloud_activity.stopped`); `None` when
/// it does not: the node is done, waits on something outside (its wait row
/// says what), or stopped for a request the user answers where requests are
/// answered (which the app reads from the request itself, and which would be
/// stale once answered). Every variant is named, so a new one is a compile
/// error here, not a node that reads as "not started".
pub fn stop_reason(outcome: &Outcome, budget: &Budget) -> Option<String> {
    match outcome {
        Outcome::Done | Outcome::Waiting { .. } => None,
        // The hook stops for a wait as "waiting: ..."; a usage limit is a
        // wait the supervisor recorded and scheduled.
        Outcome::Stopped { reason } if reason.starts_with("waiting") || reason.starts_with("usage limit: waiting") => None,
        Outcome::Stopped { reason } => Some(format!("the supervisor stopped: {reason}")),
        Outcome::BudgetExhausted { limit } => Some(budget_reason(limit, budget)),
        Outcome::NeedsHuman { reason } => match reason {
            // Requests: shown from the request itself.
            NeedsHuman::Decision { .. }
            | NeedsHuman::BlockedSteps { .. }
            | NeedsHuman::FailingCriteria { .. }
            | NeedsHuman::GateNotPassed { .. } => None,
            NeedsHuman::NoPlan
            | NeedsHuman::EvaluationStuck { .. }
            | NeedsHuman::PrBlocked { .. }
            | NeedsHuman::PrStuck { .. }
            | NeedsHuman::Permission { .. }
            | NeedsHuman::AgentFailed { .. }
            | NeedsHuman::NoProgress { .. }
            | NeedsHuman::Looping { .. } => Some(reason.describe()),
        },
    }
}

/// The reason a wake that ended with `woke` leaves in the beat.
fn woke_reason(woke: &Woke, budget: &Budget) -> Option<String> {
    match woke {
        Woke::StillWaiting(_) | Woke::AlreadyRunning { .. } => None,
        Woke::LeftStopped(reason) => Some(reason.clone()),
        Woke::Ran(outcome) => stop_reason(outcome, budget),
    }
}

/// Asks the user through a pending decision on the node, as an agent's
/// `ask` would; the next wake stops on it until it is answered. Not again
/// while an earlier one is unanswered.
fn ask(store: &FleetStore, node: Uuid, conversation: Option<Uuid>, kind: &str, question: String) -> Result<()> {
    let options = if kind == tod_core::stop_questions::CONFIG {
        tod_core::stop_questions::CONFIG_OPTIONS
    } else {
        tod_core::stop_questions::SUPERVISOR_OPTIONS
    };
    if answers::still_asking(store, node)? {
        tracing::info!(%question, "already asked; still waiting on the answer");
        return Ok(());
    }
    tracing::info!(%question, "asking the user");
    store
        .interview(
            waits::ACTOR,
            tod_store::interview::InterviewCommand::AskDecision {
                node_id: node,
                conversation_id: conversation,
                protocol: Some(kind.to_string()),
                decision: tod_store::decisions::NewDecision {
                    question,
                    options: options.iter().map(|o| o.to_string()).collect(),
                    evidence: Vec::new(),
                    ..Default::default()
                },
            },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// What is added to the agent's hang limit to give the time without progress
/// after which the supervisor stops claiming to be working.
const STALL_MARGIN: Duration = Duration::from_secs(10 * 60);

/// A phase of starting up that takes longer than this is a warning, so it
/// reaches the app's log panel (only warnings and errors are forwarded):
/// the app waits `SUPERVISOR_REPORTS_WITHIN` for the first heartbeat, and
/// what the supervisor spent that on should be visible when it is missed.
const SLOW_PHASE: Duration = Duration::from_secs(15);

/// Runs `f`, logging how long the startup phase `name` took (and warning when
/// it was slow, see [`SLOW_PHASE`]).
fn phase<T>(name: &str, since_wake: std::time::Instant, progress: &progress::Progress, f: impl FnOnce() -> T) -> T {
    let started = std::time::Instant::now();
    let out = f();
    progress.mark();
    let took = started.elapsed();
    let at = since_wake.elapsed().as_secs();
    if took >= SLOW_PHASE {
        tracing::warn!(phase = name, took_secs = took.as_secs(), seconds_since_wake_began = at, "a slow start: this phase delayed the first heartbeat");
    } else {
        tracing::info!(phase = name, took_ms = took.as_millis() as u64, seconds_since_wake_began = at, "startup phase done");
    }
    out
}

/// One wake. See the module docs.
pub fn wake(config: Config) -> Result<Woke> {
    let began = std::time::Instant::now();
    signal::install();
    // One supervisor per node: before anything is touched, the schedule
    // included. A second one (the dead-man retry wake firing during a long
    // run, a poke racing the relay's own start) says so and leaves.
    let _lock = match lock::acquire(&config.state_dir)? {
        lock::Taken::Held(lock) => lock,
        lock::Taken::Busy { holder } => {
            match holder {
                Some(pid) => {
                    tracing::info!(pid, "another supervisor is already running this node; poking it to look again");
                    if let Err(err) = lock::poke_holder(pid) {
                        tracing::warn!("could not poke the running supervisor: {err:#}");
                    }
                }
                None => tracing::info!("another supervisor is already running this node"),
            }
            return Ok(Woke::AlreadyRunning { holder });
        }
    };
    let progress = progress::Progress::new(progress::path(&config.state_dir));
    // The relay sizes its stall limit from this (`tod-relay`'s `liveness`).
    progress::write_hang_limit(&config.state_dir, config.guards.hang_after)
        .context("recording the hang limit for the relay")?;
    // How long without a mark means wedged: a hung agent is cancelled by its
    // guard after `hang_after`, so this is that and the time a cancelled run
    // takes to reach a boundary.
    let stall = config.guards.hang_after + STALL_MARGIN;
    // Hold the sandbox awake from the start: seeding a new replica from the
    // orchestrator can outlast the relay's poke hold, and the sandbox must
    // not go to standby mid-seed. Released when the wake returns, including
    // when it finds nothing to do.
    let hold = HoldGuard::take_gated(config.holder.clone(), hold::LEASE_SECS, hold::RENEW_EVERY, Some((progress.clone(), stall)));
    // Say so before anything slow: seeding a new copy and restoring
    // transcripts can take minutes, and the app waits `SUPERVISOR_REPORTS_WITHIN`
    // for a first word. The local copy does not exist yet to record it in, so
    // this goes straight to the orchestrator as the node's `cloud_activity`
    // row (`Orchestrator::push_starting_beat`); the copy is seeded with it.
    // A failure is a warning, not a stop: the first beat recorded in the copy
    // repeats the request and fails the wake with the real reason if the
    // orchestrator cannot be reached.
    if let Err(err) = config.orchestrator.push_starting_beat(config.node, waits_now_ms()) {
        tracing::warn!("the starting heartbeat could not be sent: {err:#}");
    }
    let replica = phase("open the local copy (seeding it from the orchestrator when new)", began, &progress, || {
        Replica::open(&config.state_dir, config.orchestrator.clone(), config.node, &config.workspace)
    })?;
    let replica = Arc::new(Mutex::new(replica));
    let store = replica.lock().unwrap_or_else(|e| e.into_inner()).store().clone();
    phase("pull the orchestrator's changes", began, &progress, || replica.lock().unwrap_or_else(|e| e.into_inner()).pull())?;

    // The user's explicit requests (rows, and the marker an older app left):
    // any that asks to run supersedes an older stop answer. Nothing is consumed
    // until the run starts, so a wake that leaves early leaves them for the next.
    let consumer = format!("supervisor:{}:pid{}", config.sandbox, std::process::id());
    let requested = pending_requests(&store, config.node)?;
    let run_requested = fresh_marker(&config.state_dir) || tod_store::node_requests::asks_to_run(&requested);
    let budget = match answers::before_wake_with(&store, config.node, &config.state_dir, config.budget, run_requested)? {
        answers::Before::LeaveStopped(reason) => {
            // Say so, as a wait does below: the app waits to hear from a
            // supervisor it woke, and a wake that leaves the node stopped is
            // an answer, not silence.
            replica.lock().unwrap_or_else(|e| e.into_inner()).end_cloud_beat(
                config.node,
                Some(&reason),
                &tod_core::run_state::left_stopped(&reason),
            )?;
            tracing::info!(%reason, "left stopped; going back to sleep");
            return Ok(Woke::LeftStopped(reason));
        }
        answers::Before::Go(budget) => budget,
    };

    // A request for a new session is a request to run now: what the node
    // waits on is looked at again by the run, which records it afresh. Left
    // pending, a wait would stop the run at its first step, before any agent.
    if run_requested {
        waits::cancel_pending(&store, config.node)?;
    }
    let status = waits::check(&store, config.node, &config.workspace)?;
    // What `check` settled or moved goes up before anything else.
    replica.lock().unwrap_or_else(|e| e.into_inner()).push()?;
    if let waits::WaitStatus::Waiting { reason } = status {
        // A "Check now" asked for this look and it was made; a request to run
        // is left for the wake that does.
        let looked: Vec<_> =
            requested.iter().filter(|r| r.kind == tod_store::node_requests::KIND_CHECK_NOW).cloned().collect();
        consume_requests(&store, &looked, &consumer)?;
        schedule(&store, &config)?;
        // Say so: the app waits to hear from a supervisor it started (a
        // reinstall, "Check now"), and a wake that finds nothing due is an
        // answer, not silence.
        replica.lock().unwrap_or_else(|e| e.into_inner()).end_cloud_beat(
            config.node,
            None,
            &tod_core::run_state::waiting_outside(&reason),
        )?;
        tracing::info!(%reason, "still waiting; going back to sleep");
        return Ok(Woke::StillWaiting(reason));
    }

    // Never a window with no wake: replace the wait's schedule (satisfied
    // some other way, e.g. by a webhook, it would only poke later) with a
    // retry wake before any work. An orderly end replaces it with the wake
    // the node's waits need or drops it; an error out of here leaves it (and
    // `main` replaces it with a backoff retry), so the node is never stranded.
    if let Some(scheduler) = &config.scheduler {
        waits::arm_retry(
            scheduler.as_ref(),
            config.node,
            &config.sandbox,
            &config.state_dir,
            waits_now_ms() + waits::DEAD_MAN_MS,
        )?;
    }
    let follow = match &config.transcripts {
        Some((projects, sink)) => {
            let mirror = Arc::new(Mirror::new(projects.clone(), sink.clone()));
            match phase("restore agent transcripts", began, &progress, || mirror.restore()) {
                Ok(0) => {}
                Ok(n) => tracing::info!(n, "restored agent transcripts"),
                Err(err) => tracing::warn!("restoring transcripts: {err:#}"),
            }
            Some(mirror.follow(Duration::from_secs(2)))
        }
        None => None,
    };

    let data_root = replica.lock().unwrap_or_else(|e| e.into_inner()).root().to_path_buf();
    let conversation = ConversationConfig {
        data_root: data_root.clone(),
        media: config.media.clone(),
        launch: AgentLaunchOptions::for_platform(AgentPlatform::Claude),
        settings_path: None,
        context: InterviewContextSettings::default(),
    };
    // From here the autopilot runs: say so to the app, and keep saying it.
    let beating = phase("first heartbeat", began, &progress, || {
        beat::Beating::start_gated(replica.clone(), config.node, beat::BEAT_EVERY, Some((progress.clone(), stall)))
    })?;
    let mut agent = Guarded::new(
        Syncing::new(config.agent.provider(&data_root), replica.clone()),
        config.guards.hang_after,
        Arc::new(guard::SystemClock),
    );
    // Consecutive failed runs, and the steps finished at the last one: a
    // step finished since then means the failures were not in a row.
    // Persisted: the backoff is a wait the node sleeps through, so the next
    // wake carries on counting.
    let mut failures = waits::load_count(&config.state_dir, waits::AGENT_FAILURES_FILE);
    let mut steps_at_failure = 0usize;
    let mut backing_off = false;
    let mut hook = Hook {
        replica: &replica,
        node: config.node,
        workspace: &config.workspace,
        push_branch: config.push_branch,
        state_dir: &config.state_dir,
        fresh_pending: false,
        consumer: consumer.clone(),
        context_seen: context::load_seen(&config.state_dir),
        context_pending: None,
        progress: &progress,
        last_parts: Vec::new(),
        live_sent: None,
    };
    // Asked for while the supervisor slept: this run starts in a new session.
    // The request is saved in the autopilot's state (and survives a failure
    // here), so deleting the file is what takes it.
    if fresh_requested(&config.state_dir, &store, config.node)? {
        tod_core::autopilot::AutopilotState::request_fresh_session(&data_root, config.node)?;
        take_fresh_request(&config.state_dir, &store, config.node, &consumer)?;
        tracing::info!("a new session was requested: the run starts in one");
    }
    // The run starts: the rest of what was asked is taken with it.
    consume_requests(&store, &pending_requests(&store, config.node)?, &consumer)?;
    let run = Autopilot::new(conversation, config.node, budget)
        .map(|pilot| pilot.with_poll_interval(config.poll))
        .and_then(|mut pilot| loop {
            let outcome = pilot.run_with(&store, &mut agent, &mut hook)?;
            if std::mem::take(&mut hook.fresh_pending) {
                // Stopped for a request made while it ran: end the turn's
                // agent session and go on in a new one.
                let current = pilot.state().current.as_ref().map(|c| c.conversation_id);
                if let Some(id) = current {
                    agent.with(|a| a.close_session(&ConversationDriver::session_key(id)));
                }
                waits::cancel_pending(&store, config.node)?;
                take_fresh_request(&config.state_dir, &store, config.node, &consumer)?;
                pilot.start_fresh_session()?;
                tracing::info!("took a request for a new session");
                continue;
            }
            let Some(at) = hook.context_pending.take() else {
                let current = pilot.state().current.as_ref().map(|c| c.conversation_id);
                match &outcome {
                    Outcome::NeedsHuman { reason: NeedsHuman::AgentFailed { error } } => {
                        let done = pilot.state().steps.len();
                        if tod_core::skills_context::is_missing_skill(error) {
                            // Configuration, not a flaky agent: not counted,
                            // not retried.
                            tracing::warn!(%error, "a configured skill is missing; stopping");
                            ask(
                                &store,
                                config.node,
                                current,
                                tod_core::stop_questions::CONFIG,
                                format!(
                                    "The lifecycle configuration cannot be used: {error}. Fix it (add the skill \
                                     to the repository or the home directory, or correct the lifecycle config), \
                                     then try again."
                                ),
                            )?;
                            break Ok(outcome);
                        }
                        if done > steps_at_failure {
                            failures = 0;
                        }
                        failures += 1;
                        steps_at_failure = done;
                        waits::save_count(&config.state_dir, waits::AGENT_FAILURES_FILE, failures)?;
                        match guard::on_failure(error, failures, &config.guards, waits_now_ms()) {
                            OnFailure::UsageLimit(limit) => {
                                usage_limit::record(&store, config.node, &limit)?;
                                tracing::info!(reset_at = limit.reset_at_ms, parsed = limit.parsed, "usage limit reached");
                                break Ok(Outcome::Stopped { reason: usage_limit_reason(&limit) });
                            }
                            OnFailure::Backoff { until_ms } => {
                                // An `until` wait, as for a usage limit: the node
                                // sleeps and the wake it schedules runs the step again.
                                waits::record_until(&store, config.node, until_ms)?;
                                backing_off = true;
                                tracing::warn!(failures, %error, until_ms, "the agent failed; running the step again later");
                                break Ok(Outcome::Stopped {
                                    reason: format!(
                                        "waiting: the agent failed ({failures} in a row), trying again at {until_ms}: {error}"
                                    ),
                                });
                            }
                            OnFailure::Config => unreachable!("a missing skill is handled before counting"),
                            OnFailure::Ask => {
                                ask(
                                    &store,
                                    config.node,
                                    current,
                                    tod_core::stop_questions::FAILURES,
                                    format!(
                                        "The agent failed {failures} times in a row, most recently: {error}. Keep going?"
                                    ),
                                )?;
                                break Ok(outcome);
                            }
                        }
                    }
                    Outcome::BudgetExhausted { limit } => {
                        ask(&store, config.node, current, tod_core::stop_questions::BUDGET, budget_question(limit, &budget))?;
                        break Ok(outcome);
                    }
                    _ => break Ok(outcome),
                }
            };
            // Stopped to take a context change: take it and go on.
            let current = pilot.state().current.as_ref().map(|c| c.conversation_id);
            let taken = context::take(&store, &mut agent, config.node, current)?;
            tracing::info!(?taken, "took a context change");
            hook.context_seen = at;
            context::save_seen(&config.state_dir, at)?;
            replica.lock().unwrap_or_else(|e| e.into_inner()).push()?;
        });

    // Whatever happened, leave the orchestrator and the branch with it. The
    // heartbeat ends first, so that the push below carries the run's end.
    let stopped = match &run {
        Ok(outcome) => woke_reason(&Woke::Ran(outcome.clone()), &budget),
        Err(err) => Some(format!("the supervisor failed: {err:#}")),
    };
    let state = match &run {
        Ok(outcome) => tod_core::run_state::for_outcome(
            outcome,
            tod_core::run_state::Words {
                budget: &|limit| budget_reason(limit, &budget),
                stopped_by: Some("the supervisor"),
            },
        ),
        Err(err) => tod_core::run_state::runner_failed(&format!("the supervisor failed: {err:#}")),
    };
    let ended = beating.finish(stopped, &state);
    let pushed = replica.lock().unwrap_or_else(|e| e.into_inner()).push();
    if config.push_branch
        && let Err(err) = git::push_branch(&config.workspace)
    {
        tracing::warn!("pushing the branch: {err:#}");
    }
    if let Some(follow) = follow {
        follow.stop();
    }
    let outcome = run?;
    pushed?;
    ended?;
    tracing::info!(?outcome, "autopilot stopped");
    if !backing_off {
        waits::save_count(&config.state_dir, waits::AGENT_FAILURES_FILE, 0)?;
    }
    if matches!(outcome, Outcome::Stopped { .. } | Outcome::Waiting { .. }) {
        schedule(&store, &config)?;
    } else if let Some(scheduler) = &config.scheduler {
        // An orderly end that needs no wake: clear the retry.
        waits::cancel_scheduled(scheduler.as_ref(), &config.state_dir)?;
    }
    drop(agent);
    drop(hold);
    Ok(Woke::Ran(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> Budget {
        Budget { max_sessions: 60, max_duration: Duration::from_secs(16 * 3600) }
    }

    #[test]
    fn every_outcome_that_leaves_the_node_needing_someone_leaves_its_reason() {
        let reason = |o: Outcome| stop_reason(&o, &budget());
        assert_eq!(reason(Outcome::Done), None);
        assert_eq!(reason(Outcome::Waiting { what: "checks".into(), due_at_ms: 1 }), None);
        // A wait the hook stopped for, and a usage limit (a wait of its own).
        assert_eq!(reason(Outcome::Stopped { reason: "waiting: a review".into() }), None);
        assert_eq!(reason(Outcome::Stopped { reason: usage_limit_reason(&usage_limit::UsageLimit { reset_at_ms: 5, parsed: true }) }), None);
        assert_eq!(
            reason(Outcome::Stopped { reason: "the user paused it".into() }).as_deref(),
            Some("the supervisor stopped: the user paused it")
        );
        let spent = reason(Outcome::BudgetExhausted { limit: BudgetLimit::Sessions { used: 60, elapsed_secs: 5 * 60 } }).unwrap();
        assert!(spent.contains("60 of 60 sessions") && spent.contains("0.1 of 16 hours") && spent.contains("Keep going"), "{spent}");
        let timed = reason(Outcome::BudgetExhausted { limit: BudgetLimit::Time { elapsed_secs: 16 * 3600, used: 7 } }).unwrap();
        assert!(timed.contains("7 of 60 sessions") && timed.contains("16 of 16 hours"), "{timed}");
        // Requests are shown from the request itself, and stale once answered.
        for needs in [
            NeedsHuman::Decision { pending: 1 },
            NeedsHuman::BlockedSteps { count: 1 },
            NeedsHuman::FailingCriteria { criteria: vec!["tests".into()] },
            NeedsHuman::GateNotPassed { result: "fail".into(), summary: "s".into() },
        ] {
            assert_eq!(reason(Outcome::NeedsHuman { reason: needs.clone() }), None, "{needs:?}");
        }
        for needs in [
            NeedsHuman::NoPlan,
            NeedsHuman::EvaluationStuck { fixes: vec!["f".into()] },
            NeedsHuman::PrBlocked { why: "conflict".into() },
            NeedsHuman::PrStuck { why: "threads".into() },
            NeedsHuman::Permission { title: "Write x".into() },
            NeedsHuman::AgentFailed { error: "boom".into() },
            NeedsHuman::NoProgress { step: "fix_failed".into() },
        ] {
            let text = reason(Outcome::NeedsHuman { reason: needs.clone() }).expect("a reason");
            assert_eq!(text, needs.describe());
            assert!(!text.contains("fix_failed") && !text.contains("_"), "internal names leak: {text}");
        }
    }

    /// The beat's stop reason (kept for a supervisor's old readers) and the
    /// run state agree on every outcome: one says "needs someone" exactly
    /// when the other is `stopped`, with the same words where both have them.
    #[test]
    fn the_run_state_and_the_idle_beat_agree_on_every_outcome() {
        use tod_store::run_state::Phase;
        let budget = budget();
        let words = tod_core::run_state::Words {
            budget: &|limit| budget_reason(limit, &budget),
            stopped_by: Some("the supervisor"),
        };
        let mut outcomes = vec![
            Outcome::Done,
            Outcome::Waiting { what: "checks".into(), due_at_ms: 1 },
            Outcome::Stopped { reason: "waiting: a review".into() },
            Outcome::Stopped { reason: usage_limit_reason(&usage_limit::UsageLimit { reset_at_ms: 5, parsed: true }) },
            Outcome::Stopped { reason: "a new session was requested".into() },
            Outcome::BudgetExhausted { limit: BudgetLimit::Sessions { used: 60, elapsed_secs: 5 * 60 } },
            Outcome::BudgetExhausted { limit: BudgetLimit::Time { elapsed_secs: 16 * 3600, used: 7 } },
        ];
        for needs in [
            NeedsHuman::Decision { pending: 1 },
            NeedsHuman::BlockedSteps { count: 1 },
            NeedsHuman::FailingCriteria { criteria: vec!["tests".into()] },
            NeedsHuman::GateNotPassed { result: "fail".into(), summary: "s".into() },
            NeedsHuman::NoPlan,
            NeedsHuman::EvaluationStuck { fixes: vec!["f".into()] },
            NeedsHuman::PrBlocked { why: "conflict".into() },
            NeedsHuman::PrStuck { why: "threads".into() },
            NeedsHuman::Permission { title: "Write x".into() },
            NeedsHuman::AgentFailed { error: "boom".into() },
            NeedsHuman::NoProgress { step: "fix_failed".into() },
            NeedsHuman::Looping { what: "a test".into(), times: 3 },
        ] {
            outcomes.push(Outcome::NeedsHuman { reason: needs });
        }
        for outcome in outcomes {
            let beat = stop_reason(&outcome, &budget);
            let state = tod_core::run_state::for_outcome(&outcome, words);
            assert_eq!(beat.is_some(), state.phase == Phase::Stopped, "{outcome:?}: {beat:?} vs {state:?}");
            if let (Some(beat), Outcome::BudgetExhausted { .. } | Outcome::Stopped { .. }) = (&beat, &outcome) {
                assert_eq!(&state.reason_text, beat, "{outcome:?}");
            }
        }
        // The supervisor's budget words, on the row.
        let spent = tod_core::run_state::for_outcome(
            &Outcome::BudgetExhausted { limit: BudgetLimit::Sessions { used: 60, elapsed_secs: 5 * 60 } },
            words,
        );
        assert!(spent.reason_text.contains("60 of 60 sessions") && spent.reason_text.contains("Keep going"));
    }

    #[test]
    fn a_wake_that_only_waited_leaves_no_reason_and_one_that_left_it_stopped_does() {
        assert_eq!(woke_reason(&Woke::StillWaiting("a review".into()), &budget()), None);
        assert_eq!(
            woke_reason(&Woke::LeftStopped("left stopped: you answered".into()), &budget()).as_deref(),
            Some("left stopped: you answered")
        );
    }

    #[test]
    fn the_budget_question_says_what_was_used_and_what_each_answer_does() {
        let q = budget_question(&BudgetLimit::Sessions { used: 60, elapsed_secs: 5 * 60 }, &budget());
        assert_eq!(
            q,
            "Used 60 of 60 sessions (0.1 of 16 hours). Keep going gives it 60 more sessions and 16 more hours. \
             Leave it stopped keeps it stopped until you change this answer."
        );
        let q = budget_question(&BudgetLimit::Time { elapsed_secs: 16 * 3600 + 600, used: 7 }, &budget());
        assert!(q.starts_with("Used 7 of 60 sessions (16.2 of 16 hours)."), "{q}");
    }

    #[test]
    fn the_default_budget_is_sixty_sessions_and_sixteen_hours() {
        assert_eq!(Budget::default(), budget());
    }
}
