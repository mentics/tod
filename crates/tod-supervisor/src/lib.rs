//! `tod-supervisor`: runs one autonomous node in its cloud sandbox.
//!
//! `tod-supervisor wake` (started by the relay's poke, from a schedule, or
//! when the sandbox is provisioned) asks what the node is waiting on. If it
//! is still waiting, it schedules the next check and exits. Otherwise it
//! takes a hold on the sandbox through the relay, runs the lifecycle
//! [`Autopilot`] for the node with the agent started here, and when that
//! stops — the node is done, needs the user, or recorded a wait — pushes the
//! branch, schedules the wake a wait needs, releases the hold, and exits.
//!
//! The autopilot runs against a local copy of the user's database that is
//! synced with the orchestrator around every step and turn ([`replica`]).
//! Claude's session files are mirrored as they are written
//! ([`transcripts`]), and restored first on a new sandbox so its sessions
//! resume. A `SIGUSR1` (a poke while it runs) is taken at the next stopping
//! point.
//!
//! Design: `doc/cloud-sandboxes/autonomous-nodes.md` ("The supervisor and
//! waiting", "Holding the sandbox awake", "Transcripts").

pub mod agent;
pub mod git;
pub mod hold;
pub mod orchestrator;
pub mod replica;
pub mod signal;
pub mod transcripts;
pub mod waits;

use agent::{AgentKind, Syncing};
use anyhow::Result;
use hold::{HoldGuard, Holder};
use replica::Replica;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tod_agent::{AgentLaunchOptions, AgentPlatform};
use tod_core::autopilot::{Autopilot, Boundary, Budget, Outcome, StepHook};
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
    /// The autopilot ran and stopped.
    Ran(Outcome),
}

/// The autopilot's hook: sync at every boundary, and stop for a wait.
struct Hook<'a> {
    replica: &'a Arc<Mutex<Replica>>,
    node: Uuid,
    workspace: &'a std::path::Path,
    push_branch: bool,
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

/// One wake. See the module docs.
pub fn wake(config: Config) -> Result<Woke> {
    signal::install();
    let replica = Replica::open(&config.state_dir, config.orchestrator.clone(), config.node, &config.workspace)?;
    let replica = Arc::new(Mutex::new(replica));
    let store = replica.lock().unwrap_or_else(|e| e.into_inner()).store().clone();
    replica.lock().unwrap_or_else(|e| e.into_inner()).pull()?;

    let status = waits::check(&store, config.node, &config.workspace)?;
    // What `check` settled or moved goes up before anything else.
    replica.lock().unwrap_or_else(|e| e.into_inner()).push()?;
    if let waits::WaitStatus::Waiting { reason } = status {
        schedule(&store, &config)?;
        tracing::info!(%reason, "still waiting; going back to sleep");
        return Ok(Woke::StillWaiting(reason));
    }

    // There is work: hold the sandbox awake until it is done.
    let hold = HoldGuard::take(config.holder.clone());
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
        context: InterviewContextSettings::default(),
    };
    let mut agent = Syncing::new(config.agent.provider(&data_root), replica.clone());
    let mut hook = Hook {
        replica: &replica,
        node: config.node,
        workspace: &config.workspace,
        push_branch: config.push_branch,
    };
    let run = Autopilot::new(conversation, config.node, config.budget)
        .map(|pilot| pilot.with_poll_interval(config.poll))
        .and_then(|mut pilot| pilot.run_with(&store, &mut agent, &mut hook));

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
