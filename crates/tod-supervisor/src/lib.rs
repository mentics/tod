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
pub mod orchestrator;
pub mod problems;
pub mod replica;
pub mod signal;
pub mod transcripts;
pub mod usage_limit;
pub mod waits;

use agent::{AgentKind, Syncing};
use anyhow::Result;
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
    /// The last context mark taken.
    context_seen: i64,
    /// A newer mark found at the last boundary, to take.
    context_pending: Option<i64>,
    /// The reply in flight last sent to the app, and when.
    live_sent: Option<(std::time::Instant, Vec<tod_agent::ReplyPart>)>,
}

/// The least time between two sends of the reply in flight: each carries the
/// whole reply, and the app only looks every few seconds.
const LIVE_EVERY: Duration = Duration::from_secs(3);

/// The reason the autopilot is stopped with to take a request for a new session.
pub const FRESH_SESSION_REQUESTED: &str = "new session requested";

/// Whether the app asked for a new session (it leaves a file in `state_dir`).
fn fresh_requested(state_dir: &std::path::Path) -> bool {
    state_dir.join(tod_core::autopilot::FRESH_SESSION_REQUEST_FILE).exists()
}

/// Takes the request ([`fresh_requested`]): deletes the file. An error is
/// returned, never left to be taken again as a new request.
fn take_fresh_request(state_dir: &std::path::Path) -> Result<()> {
    let path = state_dir.join(tod_core::autopilot::FRESH_SESSION_REQUEST_FILE);
    std::fs::remove_file(&path).map_err(|e| anyhow::anyhow!("remove {}: {e}", path.display()))
}

impl StepHook for Hook<'_> {
    fn watch(&mut self, turn: tod_core::autopilot::Turn<'_>) -> Option<String> {
        // The user wants this turn replaced: cancel it (the run keeps it as
        // current, and taking the request drops it).
        if fresh_requested(self.state_dir) {
            self.fresh_pending = true;
            return Some(FRESH_SESSION_REQUESTED.to_string());
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
        if fresh_requested(self.state_dir) {
            self.fresh_pending = true;
            return Ok(Some(FRESH_SESSION_REQUESTED.to_string()));
        }
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

fn budget_question(limit: &BudgetLimit) -> String {
    let spent = match limit {
        BudgetLimit::Sessions { used } => format!("{used} agent sessions"),
        BudgetLimit::Time { elapsed_secs } => format!("{:.1} hours of work", *elapsed_secs as f64 / 3600.0),
    };
    format!("This node has spent its budget ({spent}) without finishing. Keep going?")
}

/// Asks the user through a pending decision on the node, as an agent's
/// `ask` would; the next wake stops on it until it is answered. Not again
/// while an earlier one is unanswered.
fn ask(store: &FleetStore, node: Uuid, conversation: Option<Uuid>, kind: &str, question: String) -> Result<()> {
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
                    options: tod_core::stop_questions::SUPERVISOR_OPTIONS.iter().map(|o| o.to_string()).collect(),
                    evidence: Vec::new(),
                    ..Default::default()
                },
            },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// A phase of starting up that takes longer than this is a warning, so it
/// reaches the app's log panel (only warnings and errors are forwarded):
/// the app waits `SUPERVISOR_REPORTS_WITHIN` for the first heartbeat, and
/// what the supervisor spent that on should be visible when it is missed.
const SLOW_PHASE: Duration = Duration::from_secs(15);

/// Runs `f`, logging how long the startup phase `name` took (and warning when
/// it was slow, see [`SLOW_PHASE`]).
fn phase<T>(name: &str, since_wake: std::time::Instant, f: impl FnOnce() -> T) -> T {
    let started = std::time::Instant::now();
    let out = f();
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
    // Hold the sandbox awake from the start: seeding a new replica from the
    // orchestrator can outlast the relay's poke hold, and the sandbox must
    // not go to standby mid-seed. Released when the wake returns, including
    // when it finds nothing to do.
    let hold = HoldGuard::take(config.holder.clone());
    let replica = phase("open the local copy (seeding it from the orchestrator when new)", began, || {
        Replica::open(&config.state_dir, config.orchestrator.clone(), config.node, &config.workspace)
    })?;
    let replica = Arc::new(Mutex::new(replica));
    let store = replica.lock().unwrap_or_else(|e| e.into_inner()).store().clone();
    phase("pull the orchestrator's changes", began, || replica.lock().unwrap_or_else(|e| e.into_inner()).pull())?;

    let budget = match answers::before_wake(&store, config.node, &config.state_dir, config.budget)? {
        answers::Before::LeaveStopped(reason) => {
            tracing::info!(%reason, "left stopped; going back to sleep");
            return Ok(Woke::LeftStopped(reason));
        }
        answers::Before::Go(budget) => budget,
    };

    let status = waits::check(&store, config.node, &config.workspace)?;
    // What `check` settled or moved goes up before anything else.
    replica.lock().unwrap_or_else(|e| e.into_inner()).push()?;
    // A request for a new session is a request to run now: what the node
    // waits on is looked at again by the run, which records it afresh.
    if let waits::WaitStatus::Waiting { reason } = status
        && !fresh_requested(&config.state_dir)
    {
        schedule(&store, &config)?;
        // Say so: the app waits to hear from a supervisor it started (a
        // reinstall, "Check now"), and a wake that finds nothing due is an
        // answer, not silence.
        replica.lock().unwrap_or_else(|e| e.into_inner()).end_cloud_beat(config.node)?;
        tracing::info!(%reason, "still waiting; going back to sleep");
        return Ok(Woke::StillWaiting(reason));
    }

    // A wake still scheduled (its wait was satisfied some other way, e.g.
    // by a webhook) would only poke later; drop it now.
    if let Some(scheduler) = &config.scheduler {
        waits::cancel_scheduled(scheduler.as_ref(), &config.state_dir)?;
    }
    let follow = match &config.transcripts {
        Some((projects, sink)) => {
            let mirror = Arc::new(Mirror::new(projects.clone(), sink.clone()));
            match phase("restore agent transcripts", began, || mirror.restore()) {
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
    let beating = phase("first heartbeat", began, || beat::Beating::start(replica.clone(), config.node, beat::BEAT_EVERY))?;
    let mut agent = Guarded::new(
        Syncing::new(config.agent.provider(&data_root), replica.clone()),
        config.guards.hang_after,
        Arc::new(guard::SystemClock),
    );
    // Consecutive failed runs, and the steps finished at the last one: a
    // step finished since then means the failures were not in a row.
    let mut failures = 0u32;
    let mut steps_at_failure = 0usize;
    let mut hook = Hook {
        replica: &replica,
        node: config.node,
        workspace: &config.workspace,
        push_branch: config.push_branch,
        state_dir: &config.state_dir,
        fresh_pending: false,
        context_seen: context::load_seen(&config.state_dir),
        context_pending: None,
        live_sent: None,
    };
    // Asked for while the supervisor slept: this run starts in a new session.
    // The request is saved in the autopilot's state (and survives a failure
    // here), so deleting the file is what takes it.
    if fresh_requested(&config.state_dir) {
        tod_core::autopilot::AutopilotState::request_fresh_session(&data_root, config.node)?;
        take_fresh_request(&config.state_dir)?;
        tracing::info!("a new session was requested: the run starts in one");
    }
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
                take_fresh_request(&config.state_dir)?;
                pilot.start_fresh_session()?;
                tracing::info!("took a request for a new session");
                continue;
            }
            let Some(at) = hook.context_pending.take() else {
                let current = pilot.state().current.as_ref().map(|c| c.conversation_id);
                match &outcome {
                    Outcome::NeedsHuman { reason: NeedsHuman::AgentFailed { error } } => {
                        let done = pilot.state().steps.len();
                        if done > steps_at_failure {
                            failures = 0;
                        }
                        failures += 1;
                        steps_at_failure = done;
                        match guard::on_failure(error, failures, &config.guards, waits_now_ms()) {
                            OnFailure::UsageLimit(limit) => {
                                usage_limit::record(&store, config.node, &limit)?;
                                tracing::info!(reset_at = limit.reset_at_ms, parsed = limit.parsed, "usage limit reached");
                                break Ok(Outcome::Stopped { reason: usage_limit_reason(&limit) });
                            }
                            OnFailure::Retry => {
                                tracing::warn!(failures, %error, "the agent failed; running the step again");
                                continue;
                            }
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
                        ask(&store, config.node, current, tod_core::stop_questions::BUDGET, budget_question(limit))?;
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
    let ended = beating.finish();
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
    if matches!(outcome, Outcome::Stopped { .. } | Outcome::Waiting { .. }) {
        schedule(&store, &config)?;
    }
    drop(agent);
    drop(hold);
    Ok(Woke::Ran(outcome))
}
