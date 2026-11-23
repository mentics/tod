//! The task panel's runner menu, without the panel: what `tod-cli runner`
//! (the hidden operator tier, `TOD_OPERATOR=shepherd`) does.
//!
//! Nothing here is a second implementation. The entries offered are
//! [`runner_menu::menu`] over the same facts the panel gathers
//! ([`runner_menu::load_runner`], [`runner_menu::derive_run_state`], the
//! node's standing from [`crate::next_actor::compute`], the daemon's runner
//! state). Each verb then does what the panel's click does: the daemon's
//! runner for a node that runs on this machine, the supervisor's requests and
//! jobs ([`crate::cloud_jobs`]) for a node in a cloud sandbox,
//! [`crate::lifecycle::check_now`] and [`crate::lifecycle::restart_stay`], and
//! the migration the Locations dialog wraps.
//!
//! What the panel does that a command cannot: it never pairs a click with a
//! hand-started conversation (the `HandTurn` state is a conversation the user
//! began in the app; a command does not see it), and where it asks a question
//! first (a running node being migrated or its sandbox deleted, uncommitted
//! files in the way) the command refuses and names the option that answers it
//! (`--stop`, `--commit`, `--discard`).
//!
//! Who acted: the app's journey records a `UserAction` for each click, and the
//! journey recorder is a no-op in `tod-cli`, so the act is left on the node's
//! activity instead, as a note by the operator or by `shepherd:<id>`
//! (`TOD_ACTOR`).

use crate::cloud_jobs;
use crate::cloud_sync;
use crate::next_actor::{NextActor, NodeStanding};
use crate::runner_menu::{
    self, EndedHow, RunFacts, RunState, RunnerAction, RunnerActions, RunnerFacts, RunnerInputs,
};
use anyhow::{Context, Result, anyhow, bail};
use std::path::{Path, PathBuf};
use tod_agentd_client::remote::DaemonWriter;
use tod_agentd_client::{Paths as DaemonPaths, client as daemon_client};
use tod_store::fleet::{FleetStore, FilesPlace};
use tod_store::interview::{ACTOR_USER, SHEPHERD_ACTOR_PREFIX};
use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};
use uuid::Uuid;

/// `TOD_OPERATOR=shepherd` shows the operator verbs and lets them run.
pub const OPERATOR_ENV: &str = "TOD_OPERATOR";
pub const OPERATOR_SHEPHERD: &str = "shepherd";
/// `TOD_ACTOR=shepherd:<id>` names the shepherd acting; unset means the
/// operator.
pub const ACTOR_ENV: &str = "TOD_ACTOR";

/// Whether the operator tier is on in `env`.
pub fn enabled(env: impl Fn(&str) -> Option<String>) -> bool {
    env(OPERATOR_ENV).is_some_and(|v| v.trim() == OPERATOR_SHEPHERD)
}

/// Who is acting through the operator tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Who {
    /// The operator, as `tod-cli lifecycle set` is.
    Operator,
    /// A shepherd, by the id it gave in `TOD_ACTOR=shepherd:<id>`.
    Shepherd(String),
}

impl Who {
    pub fn from_env(env: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let Some(raw) = env(ACTOR_ENV).filter(|v| !v.trim().is_empty()) else {
            return Ok(Who::Operator);
        };
        match raw.trim().strip_prefix(SHEPHERD_ACTOR_PREFIX) {
            Some(id) if !id.trim().is_empty() => Ok(Who::Shepherd(id.trim().to_string())),
            _ => bail!("{ACTOR_ENV} must be `{SHEPHERD_ACTOR_PREFIX}<id>` (got `{}`)", raw.trim()),
        }
    }

    /// The actor the store records: `user` for the operator, else
    /// `shepherd:<id>`.
    pub fn actor(&self) -> String {
        match self {
            Who::Operator => ACTOR_USER.to_string(),
            Who::Shepherd(id) => format!("{SHEPHERD_ACTOR_PREFIX}{id}"),
        }
    }

    /// How the node's activity names it.
    pub fn label(&self) -> String {
        match self {
            Who::Operator => "operator".to_string(),
            Who::Shepherd(id) => format!("shepherd {id}"),
        }
    }
}

/// Every runner-menu entry, as a `tod-cli runner` verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Start,
    Resume,
    ResumeFreshBudget,
    RunFresh,
    Pause,
    StopNow,
    CheckNow,
    RerunPhase,
    Migrate,
    ResetEnvironment,
    UpdateWorkspace,
    DeleteSandbox,
    DeleteSandboxAnyway,
    SyncNow,
    AdoptSandbox,
}

impl Verb {
    pub const ALL: [Verb; 15] = [
        Verb::Start,
        Verb::Resume,
        Verb::ResumeFreshBudget,
        Verb::RunFresh,
        Verb::Pause,
        Verb::StopNow,
        Verb::CheckNow,
        Verb::RerunPhase,
        Verb::Migrate,
        Verb::ResetEnvironment,
        Verb::UpdateWorkspace,
        Verb::DeleteSandbox,
        Verb::DeleteSandboxAnyway,
        Verb::SyncNow,
        Verb::AdoptSandbox,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Verb::Start => "start",
            Verb::Resume => "resume",
            Verb::ResumeFreshBudget => "resume-fresh-budget",
            Verb::RunFresh => "run-fresh",
            Verb::Pause => "pause",
            Verb::StopNow => "stop-now",
            Verb::CheckNow => "check-now",
            Verb::RerunPhase => "rerun-phase",
            Verb::Migrate => "migrate",
            Verb::ResetEnvironment => "reset-environment",
            Verb::UpdateWorkspace => "update-workspace",
            Verb::DeleteSandbox => "delete-sandbox",
            Verb::DeleteSandboxAnyway => "delete-sandbox-anyway",
            Verb::SyncNow => "sync-now",
            Verb::AdoptSandbox => "adopt-sandbox",
        }
    }

    pub fn parse(name: &str) -> Option<Verb> {
        Verb::ALL.into_iter().find(|v| v.name() == name)
    }

    /// The menu entry this verb is.
    pub fn action(self) -> RunnerAction {
        match self {
            Verb::Start => RunnerAction::Start,
            Verb::Resume => RunnerAction::Resume,
            Verb::ResumeFreshBudget => RunnerAction::ResumeFreshBudget,
            Verb::RunFresh => RunnerAction::RunFresh,
            Verb::Pause => RunnerAction::Pause,
            Verb::StopNow => RunnerAction::StopNow,
            Verb::CheckNow => RunnerAction::CheckNow,
            Verb::RerunPhase => RunnerAction::RerunPhase,
            Verb::Migrate => RunnerAction::MigrateTo,
            Verb::ResetEnvironment => RunnerAction::ResetEnvironment,
            Verb::UpdateWorkspace => RunnerAction::UpdateWorkspace,
            Verb::DeleteSandbox => RunnerAction::DeleteSandbox,
            Verb::DeleteSandboxAnyway => RunnerAction::DeleteSandboxAnyway,
            Verb::SyncNow => RunnerAction::SyncNow,
            Verb::AdoptSandbox => RunnerAction::AdoptSandbox,
        }
    }

    /// The verb of a menu entry (every entry has one).
    pub fn of(action: RunnerAction) -> Verb {
        match action {
            RunnerAction::Start => Verb::Start,
            RunnerAction::Resume => Verb::Resume,
            RunnerAction::ResumeFreshBudget => Verb::ResumeFreshBudget,
            RunnerAction::RunFresh => Verb::RunFresh,
            RunnerAction::Pause => Verb::Pause,
            RunnerAction::StopNow => Verb::StopNow,
            RunnerAction::CheckNow => Verb::CheckNow,
            RunnerAction::RerunPhase => Verb::RerunPhase,
            RunnerAction::MigrateTo => Verb::Migrate,
            RunnerAction::ResetEnvironment => Verb::ResetEnvironment,
            RunnerAction::UpdateWorkspace => Verb::UpdateWorkspace,
            RunnerAction::DeleteSandbox => Verb::DeleteSandbox,
            RunnerAction::DeleteSandboxAnyway => Verb::DeleteSandboxAnyway,
            RunnerAction::SyncNow => Verb::SyncNow,
            RunnerAction::AdoptSandbox => Verb::AdoptSandbox,
        }
    }
}

/// The daemon's runner, as far as the verbs need it. [`AgentDaemon`] is the
/// real one; a test plays it.
pub trait Daemon {
    /// Every node's runner state.
    fn snapshot(&self) -> Result<Vec<crate::autopilot::local::RunnerState>>;
    fn start(&self, node: Uuid, renew_budget: bool) -> Result<()>;
    fn pause(&self, node: Uuid) -> Result<()>;
    fn stop(&self, node: Uuid) -> Result<()>;
}

/// `tod-agentd` for a data root, reached the way the app reaches it. It is
/// never started from here (a daemon the app has not started is not this
/// command's to start).
pub struct AgentDaemon {
    writer: DaemonWriter,
}

impl AgentDaemon {
    pub fn new(root: &Path) -> Self {
        Self { writer: DaemonWriter::new(root, None) }
    }
}

impl Daemon for AgentDaemon {
    fn snapshot(&self) -> Result<Vec<crate::autopilot::local::RunnerState>> {
        let value = self.writer.runner_snapshot()?;
        serde_json::from_value(value).context("read the daemon's runner states")
    }

    fn start(&self, node: Uuid, renew_budget: bool) -> Result<()> {
        self.writer.runner_start(node, renew_budget)
    }

    fn pause(&self, node: Uuid) -> Result<()> {
        self.writer.runner_pause(node)
    }

    fn stop(&self, node: Uuid) -> Result<()> {
        self.writer.runner_stop(node)
    }
}

/// A store and the daemon beside it, opened for one command.
pub struct Session {
    pub fleet: FleetStore,
    pub root: PathBuf,
    /// `Some` when `tod-agentd` runs for the data root: the store is then its
    /// client, as the app's is. `None` otherwise; the store is opened directly
    /// and a verb that needs the daemon says so.
    pub daemon: Option<AgentDaemon>,
}

impl Session {
    pub fn open(root: &Path) -> Result<Self> {
        // The app pins its data root at start. A verb that makes the node's
        // files (`start` on a node with none yet) reads `TodPaths::discover()`,
        // which fails with "data root is not configured" in `tod-cli` unless it
        // is pinned here.
        tod_store::paths::set_data_root(root.to_path_buf());
        let daemon_paths = DaemonPaths::new(root);
        let (fleet, running) = match daemon_client::probe(&daemon_paths) {
            // Explained like every other open: a daemon of another build, or
            // one that answers the hello but not the store, names itself.
            Ok(connection) => (crate::direct_store::through_daemon(root, connection)?, true),
            // Nothing answers: open directly, which retries and, if the lock
            // is held, says why the daemon could not be reached.
            Err(_) => (crate::direct_store::open(root, false)?, false),
        };
        Ok(Self { fleet, root: root.to_path_buf(), daemon: running.then(|| AgentDaemon::new(root)) })
    }

    pub fn ctx(&self) -> Ctx<'_> {
        Ctx { fleet: &self.fleet, root: &self.root, daemon: self.daemon.as_ref().map(|d| d as &dyn Daemon) }
    }
}

/// What the verbs run against.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub fleet: &'a FleetStore,
    pub root: &'a Path,
    pub daemon: Option<&'a dyn Daemon>,
}

impl<'a> Ctx<'a> {
    fn daemon(&self, why: &str) -> Result<&'a dyn Daemon> {
        self.daemon.ok_or_else(|| {
            anyhow!(
                "{why}, and tod-agentd is not running for {}: a node that runs on this machine is run by the \
                 daemon, which the app starts. Start the app (or `tod-agentd`) for this data root and try again.",
                self.root.display()
            )
        })
    }
}

/// Everything the menu was decided from.
pub struct Gathered {
    pub facts: RunnerFacts,
    pub inputs: RunnerInputs,
    pub actions: RunnerActions,
    pub standing: Option<NodeStanding>,
    pub runner: Option<crate::autopilot::local::RunnerState>,
}

/// Read what the task panel reads, and decide the menu from it. Reads only.
pub fn gather(ctx: &Ctx, node: Uuid) -> Result<Gathered> {
    let facts = runner_menu::load_runner(ctx.fleet, node);
    if let Some(error) = &facts.error {
        bail!("{error}");
    }
    let place = facts.place.context("where the node's files are could not be read")?;
    let standing = crate::next_actor::compute(ctx.fleet, ctx.root)?.remove(&node);
    // A supervised node's runs are the supervisor's, and its standing says
    // how they stand; only a local one asks the daemon.
    let runner = if facts.supervised {
        None
    } else {
        let daemon = ctx.daemon("this node's run is the daemon's to report")?;
        daemon.snapshot()?.into_iter().find(|s| s.node == node)
    };
    let done = match &standing {
        Some(s) => matches!(s.actor, NextActor::Done),
        None => facts.lifecycle == crate::runner_status::DONE_STATE,
    };
    let run = runner_menu::derive_run_state(&RunFacts {
        done,
        supervised: facts.supervised,
        runner_running: runner.as_ref().is_some_and(|s| s.running),
        pausing: runner.as_ref().is_some_and(|s| s.pausing),
        hand_turn: false,
        outcome: runner.as_ref().and_then(|s| s.outcome.as_ref()),
        actor: standing.as_ref().map(|s| &s.actor),
        pause_asked: false,
    });
    let inputs = RunnerInputs {
        place,
        supervised: facts.supervised,
        run,
        has_checkout: facts.has_checkout,
        has_children: facts.has_children,
        adoptable: facts.adoptable,
        busy: None,
    };
    let actions = runner_menu::menu(&inputs);
    Ok(Gathered { facts, inputs, actions, standing, runner })
}

/// `runner menu`: what the split button would offer now.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MenuReport {
    pub lifecycle: String,
    pub runs_on: String,
    pub run: String,
    /// The button itself, when it has an action.
    pub default: Option<MenuEntry>,
    /// The menu's entries, in display order.
    pub entries: Vec<MenuEntry>,
    /// The locations `migrate` takes, when it is offered.
    pub locations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MenuEntry {
    pub verb: &'static str,
    pub label: &'static str,
}

impl MenuEntry {
    fn of(action: RunnerAction) -> Self {
        Self { verb: Verb::of(action).name(), label: action.label() }
    }
}

impl MenuReport {
    pub fn text(&self) -> String {
        let mut out = format!("lifecycle: {}\nruns on: {}\nrun: {}\n", self.lifecycle, self.runs_on, self.run);
        match &self.default {
            Some(e) => out.push_str(&format!("default: {} ({})\n", e.verb, e.label)),
            None => out.push_str("default: none\n"),
        }
        if self.entries.is_empty() {
            out.push_str("menu: empty\n");
        } else {
            out.push_str("menu:\n");
            for e in &self.entries {
                out.push_str(&format!("  {} ({})\n", e.verb, e.label));
            }
        }
        if !self.locations.is_empty() {
            out.push_str(&format!("locations for migrate: {}\n", self.locations.join(", ")));
        }
        out.trim_end().to_string()
    }
}

pub fn menu_for(ctx: &Ctx, node: Uuid) -> Result<MenuReport> {
    let g = gather(ctx, node)?;
    Ok(MenuReport {
        lifecycle: g.facts.lifecycle.clone(),
        runs_on: g.facts.place_text.clone(),
        run: run_state_text(g.inputs.run),
        default: g.actions.default.map(MenuEntry::of),
        entries: g.actions.menu.iter().copied().map(MenuEntry::of).collect(),
        locations: if g.actions.menu.contains(&RunnerAction::MigrateTo) {
            g.facts.migration_targets().into_iter().map(|p| p.name.clone()).collect()
        } else {
            Vec::new()
        },
    })
}

pub fn run_state_text(run: RunState) -> String {
    match run {
        RunState::NeverRun => "not started".into(),
        RunState::Active { pausing: false } => "running".into(),
        RunState::Active { pausing: true } => "running, pausing".into(),
        RunState::HandTurn => "a conversation started by hand is running".into(),
        RunState::Ended(EndedHow::Paused) => "paused".into(),
        RunState::Ended(EndedHow::Stopped) => "stopped".into(),
        RunState::Ended(EndedHow::Failed) => "failed".into(),
        RunState::Ended(EndedHow::Request) => "stopped for a request to the user".into(),
        RunState::Ended(EndedHow::Budget) => "stopped: its turn or time budget is spent".into(),
        RunState::Ended(EndedHow::Waiting) => "waiting on something else".into(),
        RunState::Done => "done".into(),
    }
}

/// What the uncommitted files of a location being left are to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dirty {
    /// Refuse and say how to answer (the dialog's question).
    #[default]
    Refuse,
    /// Commit them on the branch first (the dialog's Commit).
    Commit,
    /// Throw them away (the dialog's Discard).
    Discard,
}

/// What a verb takes beyond the node.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `migrate`: the location to migrate to, by name or id.
    pub location: Option<String>,
    /// `delete-sandbox*`: stop an active run first (the panel's "Stop and delete").
    pub stop: bool,
    /// `migrate`: uncommitted files in the location being left.
    pub dirty: Dirty,
}

/// What a verb did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Done {
    pub message: String,
    /// A check that found the gate failing, or GitHub unreadable: a result,
    /// not a failure of the verb, but one a caller reads.
    pub failed: bool,
}

impl Done {
    fn ok(message: impl Into<String>) -> Self {
        Self { message: message.into(), failed: false }
    }
}

fn job(result: cloud_jobs::JobResult) -> Result<Done> {
    result.map(Done::ok).map_err(|message| anyhow!(message))
}

/// Do `verb` on `node` as the panel's click would, after checking the menu
/// offers it now, and leave a note on the node's activity saying who did it.
pub fn perform(
    ctx: &Ctx,
    node: Uuid,
    verb: Verb,
    opts: &Options,
    who: &Who,
    progress: &mut dyn FnMut(&str),
) -> Result<Done> {
    let g = gather(ctx, node)?;
    let action = verb.action();
    ensure_offered(&g.actions, g.inputs.run, action)?;
    let result = run_verb(ctx, node, verb, opts, &g, progress);
    let (title, failed, detail) = match &result {
        Ok(done) => (format!("{} ran \"{}\"", who.label(), action.label()), done.failed, done.message.clone()),
        Err(err) => (format!("{} tried \"{}\" and it failed", who.label(), action.label()), true, format!("{err:#}")),
    };
    let noted = record_note(ctx.fleet, node, &g.facts.lifecycle, title, failed, detail);
    match (result, noted) {
        (Ok(done), Ok(())) => Ok(done),
        (Ok(done), Err(err)) => Err(err.context(format!("{} (it was done, but who did it was not recorded)", done.message))),
        (Err(err), Ok(())) => Err(err),
        (Err(err), Err(note_err)) => Err(err.context(format!("also, who tried it was not recorded: {note_err:#}"))),
    }
}

/// Refuse an action that is no longer offered by the runner menu.
pub fn ensure_offered(actions: &RunnerActions, run: RunState, action: RunnerAction) -> Result<()> {
    if actions.default == Some(action) || actions.menu.contains(&action) {
        return Ok(());
    }
    let offered: Vec<&str> = actions.default.iter().chain(&actions.menu).map(|a| Verb::of(*a).name()).collect();
    bail!(
        "`{}` ({}) is not offered for this node now (it is {}). The menu offers: {}",
        Verb::of(action).name(), action.label(), run_state_text(run),
        if offered.is_empty() { "nothing".to_string() } else { offered.join(", ") }
    )
}

/// A note on the node's activity, by the operator or a shepherd.
pub fn record_note(fleet: &FleetStore, node: Uuid, phase: &str, title: String, failed: bool, detail: String) -> Result<()> {
    let mut entry = TimelineEntry::new(node, KIND_NOTE, phase, title, failed, detail);
    entry.by_user = true;
    fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
    fleet.writer().flush().context("save the note on the node's activity")?;
    Ok(())
}

/// Start, or go on with, a supervised node (`TaskPanel::supervised_go`): the
/// one place that gives a cloud node's agent work ([`cloud_jobs::submit_work`]),
/// which makes the sandbox when the node has none.
fn supervised_go(
    ctx: &Ctx,
    node: Uuid,
    _g: &Gathered,
    kind: Option<&'static str>,
    progress: &mut dyn FnMut(&str),
) -> Result<Done> {
    job(cloud_jobs::submit_work(ctx.fleet, ctx.root, &node.to_string(), kind, progress))
}

fn start_local(ctx: &Ctx, node: Uuid, renew_budget: bool) -> Result<Done> {
    ctx.daemon("starting a run on this machine goes through the daemon")?.start(node, renew_budget)?;
    Ok(Done::ok("Started: the daemon runs it on this machine."))
}

/// Prepare a fresh session or phase before the surface starts the runner.
/// The app can host a runner in-process, whereas the CLI uses the daemon;
/// both must make the same durable changes before asking their host to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshRun {
    Session,
    Phase,
}

pub fn prepare_fresh_run(
    fleet: &FleetStore,
    root: &Path,
    node: Uuid,
    supervised: bool,
    fresh: FreshRun,
) -> Result<()> {
    if fresh == FreshRun::Phase {
        crate::lifecycle::restart_stay(fleet, node)?;
    }
    if !supervised {
        match fresh {
            FreshRun::Session => crate::autopilot::AutopilotState::request_fresh_session(root, node)?,
            FreshRun::Phase => crate::autopilot::AutopilotState::request_rerun_phase(root, node)?,
        }
    }
    Ok(())
}

fn run_verb(
    ctx: &Ctx,
    node: Uuid,
    verb: Verb,
    opts: &Options,
    g: &Gathered,
    progress: &mut dyn FnMut(&str),
) -> Result<Done> {
    use cloud_sync::{REQUEST_FRESH_BUDGET, REQUEST_FRESH_SESSION, REQUEST_KEEP_GOING, REQUEST_PAUSE, REQUEST_STOP_NOW};
    let supervised = g.inputs.supervised;
    let id = node.to_string();
    match verb {
        Verb::Start if supervised => supervised_go(ctx, node, g, None, progress),
        Verb::Resume if supervised => supervised_go(ctx, node, g, Some(REQUEST_KEEP_GOING), progress),
        Verb::Start | Verb::Resume => start_local(ctx, node, false),
        Verb::ResumeFreshBudget if supervised => supervised_go(ctx, node, g, Some(REQUEST_FRESH_BUDGET), progress),
        Verb::ResumeFreshBudget => start_local(ctx, node, true),
        Verb::RunFresh if supervised => supervised_go(ctx, node, g, Some(REQUEST_FRESH_SESSION), progress),
        Verb::RunFresh => {
            let daemon = ctx.daemon("starting a run on this machine goes through the daemon")?;
            prepare_fresh_run(ctx.fleet, ctx.root, node, false, FreshRun::Session)?;
            daemon.start(node, false)?;
            Ok(Done::ok("Started in a new session: the daemon runs it on this machine."))
        }
        Verb::Pause if supervised => job(cloud_jobs::stop_request(ctx.fleet, ctx.root, &id, REQUEST_PAUSE, "Pause")),
        Verb::Pause => {
            ctx.daemon("pausing a run on this machine goes through the daemon")?.pause(node)?;
            Ok(Done::ok("Pause asked: the run stops once its turn ends."))
        }
        Verb::StopNow if supervised => {
            job(cloud_jobs::stop_request(ctx.fleet, ctx.root, &id, REQUEST_STOP_NOW, "Stop now"))
        }
        Verb::StopNow => {
            ctx.daemon("stopping a run on this machine goes through the daemon")?.stop(node)?;
            Ok(Done::ok("Stop asked: the turn in flight is cancelled and the run stops."))
        }
        Verb::CheckNow => {
            let found = crate::lifecycle::check_now(ctx.fleet, node, ctx.root).context("Could not check")?;
            let (mut message, failed) = runner_menu::describe_check(&found);
            // A check that finds only the phase agent's work left does that
            // work, as Start would: a checked node must not just sit.
            if found.needs_agent {
                let started = if supervised { supervised_go(ctx, node, g, None, progress)? } else { start_local(ctx, node, false)? };
                message.push(' ');
                message.push_str(&started.message);
            }
            Ok(Done { message, failed })
        }
        Verb::RerunPhase => {
            prepare_fresh_run(ctx.fleet, ctx.root, node, supervised, FreshRun::Phase)?;
            if supervised {
                return supervised_go(ctx, node, g, Some(REQUEST_FRESH_SESSION), progress);
            }
            let daemon = ctx.daemon("starting a run on this machine goes through the daemon")?;
            daemon.start(node, false)?;
            Ok(Done::ok("The phase starts again in a new session: the daemon runs it on this machine."))
        }
        Verb::ResetEnvironment => job(cloud_jobs::reset_environment(ctx.fleet, node, progress)),
        Verb::AdoptSandbox => job(cloud_jobs::adopt_sandbox(ctx.fleet, &id, progress)),
        Verb::UpdateWorkspace => tod_store::fleet::workspace_update::update_workspace_to_main(ctx.fleet, &id)
            .map(Done::ok)
            .context("Could not update the workspace"),
        Verb::SyncNow => job(cloud_jobs::sync_now(ctx.fleet, ctx.root, Some(node))),
        Verb::DeleteSandbox | Verb::DeleteSandboxAnyway => {
            let checkpoint = verb == Verb::DeleteSandbox;
            if matches!(g.inputs.run, RunState::Active { .. }) {
                if !opts.stop {
                    bail!(
                        "the node's run is active: pass --stop to stop it and then delete the sandbox, or pause it \
                         first and wait for it to end"
                    );
                }
                // A run the daemon hosts is stopped here; a supervisor's is
                // stopped by the job itself.
                if g.runner.as_ref().is_some_and(|s| s.running) {
                    ctx.daemon("stopping the node's local run goes through the daemon")?.stop(node)?;
                }
                return job(cloud_jobs::stop_and_delete_sandbox(ctx.fleet, &id, checkpoint, progress));
            }
            if checkpoint {
                job(cloud_jobs::delete_sandbox(ctx.fleet, &id))
            } else {
                job(cloud_jobs::delete_sandbox_anyway(ctx.fleet, &id))
            }
        }
        Verb::Migrate => migrate(ctx, node, g, opts, progress),
    }
}

/// The place named `wanted` (its name or id) among `places`.
pub fn find_place<'a>(places: &'a [FilesPlace], wanted: &str) -> Result<&'a FilesPlace> {
    places.iter().find(|p| p.id == wanted).or_else(|| places.iter().find(|p| p.name == wanted)).ok_or_else(|| {
        let names: Vec<&str> = places.iter().map(|p| p.name.as_str()).collect();
        anyhow!(
            "no location is named `{wanted}`. The locations are: {}",
            if names.is_empty() { "none".to_string() } else { names.join(", ") }
        )
    })
}

/// Gather the files that migration affects and preserve the node's sessions
/// before either surface asks the user how to handle those files.
pub fn prepare_migration(
    fleet: &FleetStore,
    node: &str,
    place: &str,
) -> Result<Vec<tod_store::fleet::provision::AffectedLocation>> {
    let affected = tod_store::fleet::places::migrate_impact(fleet, node, place)?;
    tod_store::fleet::session_log::keep_node_state(fleet, node).context("keep the node's sessions")?;
    Ok(affected)
}

/// Finish a migration after its affected files have been removed.
pub fn finish_migration(fleet: &FleetStore, node: &str, place: &str, left_sandbox: bool) -> Result<()> {
    tod_store::fleet::places::migrate(fleet, node, place)?;
    if left_sandbox {
        cloud_sync::lost::announce_left_cloud(fleet, node)?;
    }
    Ok(())
}

/// Migrate `node` to a location: what `ui::files_impact::confirm_migrate`
/// does, with the dialog's questions answered by the options. Nothing is
/// started. The node's sessions are kept, its current files are removed (each
/// branch pushed first), the location is chosen, and a sandbox it leaves is
/// announced to the orchestrator.
fn migrate(ctx: &Ctx, node: Uuid, g: &Gathered, opts: &Options, progress: &mut dyn FnMut(&str)) -> Result<Done> {
    use tod_store::fleet::{LocationState, provision};
    if matches!(g.inputs.run, RunState::Active { .. } | RunState::HandTurn) {
        bail!("the node's run is active: pause it (`runner pause`) or stop it, wait for it to end, then migrate");
    }
    let wanted = opts.location.as_deref().context("migrate needs --to <LOCATION> (by name or id)")?;
    let place = find_place(&g.facts.locations, wanted)?;
    let id = node.to_string();
    if let Some(stale) = provision::stale_location_of(ctx.fleet, &id)?
        && Some(place.id.as_str()) == g.facts.current_location.as_deref()
    {
        // Moving to the place it is in would pin an override to no purpose;
        // what is wrong is the stale files, and Reset removes exactly them.
        bail!(
            "the node is already in location `{}`, but its {} was made from Files settings that have since \
             changed. Run `runner reset-environment`: it removes it (the branch is pushed first, uncommitted work \
             is committed on it) and the next launch makes a fresh one, with no override pinned",
            place.name,
            stale.describe()
        );
    }
    g.facts.check_migration_target(place)?;
    let paths = tod_store::TodPaths::at(ctx.root);
    let settings = tod_store::TodSettings::load(&paths).context("read the settings")?;
    let affected = prepare_migration(ctx.fleet, &id, &place.id).context("Could not migrate")?;
    let left_sandbox = affected.iter().any(|a| a.location.sandbox().is_some());
    for a in &affected {
        let mut state = provision::location_state(ctx.fleet, &paths, &a.node_id)?;
        if let LocationState::Dirty(lines) = &state {
            match opts.dirty {
                Dirty::Refuse => bail!(
                    "{}'s files have uncommitted changes ({}{}). Pass --commit to commit them on its branch first, \
                     or --discard to throw them away",
                    a.node_title,
                    lines.iter().take(3).cloned().collect::<Vec<_>>().join("; "),
                    if lines.len() > 3 { format!("; and {} more", lines.len() - 3) } else { String::new() }
                ),
                Dirty::Commit => {
                    progress(&format!("{}: committing its changes…", a.node_title));
                    provision::commit_location_changes(ctx.fleet, &a.node_id)?;
                }
                Dirty::Discard => {
                    progress(&format!("{}: discarding its changes…", a.node_title));
                    provision::discard_location_changes(ctx.fleet, &a.node_id)?;
                }
            }
            state = provision::location_state(ctx.fleet, &paths, &a.node_id)?;
        }
        match state {
            LocationState::Clean | LocationState::Gone => {}
            LocationState::Busy(reason) => bail!("{}'s files are in use: {reason}", a.node_title),
            LocationState::Dirty(_) => bail!("{}'s files still have uncommitted changes", a.node_title),
        }
    }
    for a in &affected {
        let title = a.node_title.clone();
        progress(&format!("{title}: pushing its branch…"));
        provision::remove_location(ctx.fleet, &paths, &settings, &a.node_id, &mut |step| {
            progress(&format!("{title}: {step}"))
        })
        .with_context(|| format!("Could not remove {title}'s files"))?;
    }
    finish_migration(ctx.fleet, &id, &place.id, left_sandbox)?;
    Ok(Done::ok(format!(
        "Migrated to {}. Nothing was started. The node now has a Files override of its own (location {}), which \
         every node below it inherits; `tod-cli capabilities disable {id} files` hands it back to the nearest \
         ancestor's, and `runner reset-environment` then removes the worktree made for the override.",
        place.name, place.name
    )))
}

/// What answering a request did, for `decisions answer`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Answer {
    pub decision: Uuid,
    pub node: Uuid,
    /// "option 2 (\"per invoice\")" or the free text, as the panel words it.
    pub answered: String,
    /// What the answer also did to the node, when it sent it back.
    pub moved_to: Option<String>,
    /// The request was already answered: its earlier answer, now replaced
    /// (the task panel's "Change" does the same; the log keeps both).
    pub changed_from: Option<String>,
    /// A problem waking the node's supervisor after the answer was recorded.
    pub wake_problem: Option<String>,
}

/// Answer request `decision_id` exactly as the task panel does
/// ([`crate::lifecycle::send_back::answer_decision_as`]: the move an option
/// stands for first, a permission's standing grant, a bot-silence answer
/// releasing the parked run), recorded as `who`, then wake a node that runs
/// in the cloud ([`cloud_sync::wake_after_answer`]). The in-app conversation
/// that asked is not sent a message (that is the app's); the answer reaches
/// the agent's next turn in the node's answered questions.
pub fn answer(
    ctx: &Ctx,
    decision_id: Uuid,
    option: Option<usize>,
    text: Option<String>,
    who: &Who,
) -> Result<Answer> {
    use tod_store::decisions::DecisionRepo;
    let before = ctx
        .fleet
        .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))?
        .with_context(|| format!("decision {decision_id} not found"))?;
    let node = before.decision.node_id;
    let phase = ctx.fleet.get_node(&node.to_string()).ok().flatten().map(|n| n.lifecycle).unwrap_or_default();
    let actor = who.actor();
    let result = crate::lifecycle::answer::record_decision_answer_as(ctx.fleet, decision_id, option, text.clone(), &actor);
    let title = format!("{} answered a request", who.label());
    match result {
        Ok(_) => {}
        Err(err) => {
            let detail = format!("{}: {err:#}", before.decision.question.lines().next().unwrap_or(""));
            let noted = record_note(ctx.fleet, node, &phase, format!("{} tried to answer a request and it failed", who.label()), true, detail);
            return Err(match noted {
                Ok(()) => err,
                Err(note_err) => err.context(format!("also, who tried it was not recorded: {note_err:#}")),
            });
        }
    }
    let after = ctx
        .fleet
        .read(|conn| DecisionRepo::new(conn).get_with_answers(decision_id))?
        .with_context(|| format!("decision {decision_id} vanished after answering"))?;
    let option_index = option.map(|o| o as i64);
    let answered = crate::lifecycle::answer::describe_answer(&after.decision, option_index, text.as_deref());
    let moved_to = option_index.and_then(|o| before.decision.move_for(o)).map(str::to_string);
    let changed_from = before
        .answers
        .last()
        .map(|prev| crate::lifecycle::answer::describe_answer(&before.decision, prev.option, prev.text.as_deref()));
    let detail = format!("{}: {answered}", after.decision.question.lines().next().unwrap_or(""));
    record_note(ctx.fleet, node, &phase, title, false, detail)
        .map_err(|err| err.context("the answer was recorded, but who gave it was not"))?;
    let wake_problem = crate::lifecycle::answer::wake_after_decision(ctx.fleet, decision_id).err().map(|e| format!("{e:#}"));
    Ok(Answer { decision: decision_id, node, answered, moved_to, changed_from, wake_problem })
}

#[cfg(test)]
#[path = "runner_operator_tests.rs"]
mod tests;
