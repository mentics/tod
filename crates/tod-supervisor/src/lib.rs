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
pub mod context;
pub mod git;
pub mod guard;
pub mod hold;
pub mod orchestrator;
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
use tod_core::conversation::driver::ConversationConfig;
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
    /// The last context mark taken.
    context_seen: i64,
    /// A newer mark found at the last boundary, to take.
    context_pending: Option<i64>,
}

impl StepHook for Hook<'_> {
    fn at(&mut self, fleet: &FleetStore, boundary: Boundary) -> Result<Option<String>> {
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

/// Schedules the wake the node's waits need, if it has a scheduler.
fn schedule(store: &FleetStore, config: &Config) -> Result<()> {
    match &config.scheduler {
        Some(scheduler) => {
            waits::schedule_wake(store, config.node, scheduler.as_ref(), &config.sandbox)?;
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

/// One wake. See the module docs.
pub fn wake(config: Config) -> Result<Woke> {
    signal::install();
    // Hold the sandbox awake from the start: seeding a new replica from the
    // orchestrator can outlast the relay's poke hold, and the sandbox must
    // not go to standby mid-seed. Released when the wake returns, including
    // when it finds nothing to do.
    let hold = HoldGuard::take(config.holder.clone());
    let replica = Replica::open(&config.state_dir, config.orchestrator.clone(), config.node, &config.workspace)?;
    let replica = Arc::new(Mutex::new(replica));
    let store = replica.lock().unwrap_or_else(|e| e.into_inner()).store().clone();
    replica.lock().unwrap_or_else(|e| e.into_inner()).pull()?;

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
    if let waits::WaitStatus::Waiting { reason } = status {
        schedule(&store, &config)?;
        tracing::info!(%reason, "still waiting; going back to sleep");
        return Ok(Woke::StillWaiting(reason));
    }

    let follow = match &config.transcripts {
        Some((projects, sink)) => {
            let mirror = Arc::new(Mirror::new(projects.clone(), sink.clone()));
            match mirror.restore() {
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
        context_seen: context::load_seen(&config.state_dir),
        context_pending: None,
    };
    let run = Autopilot::new(conversation, config.node, budget)
        .map(|pilot| pilot.with_poll_interval(config.poll))
        .and_then(|mut pilot| loop {
            let outcome = pilot.run_with(&store, &mut agent, &mut hook)?;
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

    // Whatever happened, leave the orchestrator and the branch with it.
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
    tracing::info!(?outcome, "autopilot stopped");
    if matches!(outcome, Outcome::Stopped { .. }) {
        schedule(&store, &config)?;
    }
    drop(agent);
    drop(hold);
    Ok(Woke::Ran(outcome))
}
