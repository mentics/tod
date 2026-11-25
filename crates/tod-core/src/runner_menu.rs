//! What the task panel's runner split button offers, as a pure function of
//! one value (`doc/runner-menu/design.md`, "The runner menu" and "The
//! runner's state"). No GPUI and no store: the panel gathers a
//! [`RunnerInputs`] from the node runners, the node's standing and its Files
//! location, calls [`menu`], and draws the answer, so every rule is tested
//! without a window.
//!
//! The task panel (`tod-ui`'s `panels/task.rs`) and `tod-cli runner`
//! ([`crate::runner_operator`]) both call it, and the facts it needs
//! ([`RunnerFacts`], [`derive_run_state`]) are gathered here too, so both read
//! a node the same way.

use crate::autopilot::Outcome;
use crate::cloud_sync::CloudNode;
use crate::next_actor::{NextActor, YouReason};
use uuid::Uuid;

/// Where the node's files, and so its work, live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// This machine: the workspace directory or a worktree of it.
    Host,
    DevContainer,
    /// A cloud sandbox; `exists` is whether the node has one now.
    Sandbox {
        exists: bool,
    },
}

/// Why a run that is not active stopped before `done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndedHow {
    Paused,
    Stopped,
    Failed,
    /// A request is waiting on the user.
    Request,
    /// The turn or time budget is spent.
    Budget,
    /// Ended only to wait for something else; the daemon or scheduler wakes
    /// it, so there is nothing to resume.
    Waiting,
}

/// Where the node's run stands. Locally it comes from `NodeRunners`;
/// supervised, from the node's standing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    NeverRun,
    /// The runner is active; `pausing` once Pause was asked for.
    Active {
        pausing: bool,
    },
    /// A conversation the user started by hand is running.
    HandTurn,
    Ended(EndedHow),
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerInputs {
    pub place: Place,
    /// The autopilot runs under the supervisor in the node's sandbox
    /// (`runner_place == Supervisor`), not the daemon's local runner.
    pub supervised: bool,
    pub run: RunState,
    pub has_checkout: bool,
    pub has_children: bool,
    /// The node's old records name a sandbox its Files location does not
    /// record, and adoption can take it
    /// (`tod_core::cloud_sync::adopt::adoptable`).
    pub adoptable: bool,
    /// A cloud job of the panel's own is running; what it is doing.
    pub busy: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerAction {
    /// Start the autopilot where the node's runner place says.
    Start,
    /// Continue the same agent session.
    Resume,
    /// Resume with the turn and time budget reset; the session is kept.
    ResumeFreshBudget,
    /// The current step in a new agent session.
    RunFresh,
    /// Stop once the current turn ends.
    Pause,
    /// Cancel the turn in flight.
    StopNow,
    /// Run the current state's gate check now, with no agent.
    CheckNow,
    /// Run the current state again as if the node had just entered it, in a
    /// new session.
    RerunPhase,
    /// A submenu of the locations; the panel lists them.
    MigrateTo,
    ResetEnvironment,
    UpdateWorkspace,
    DeleteSandbox,
    DeleteSandboxAnyway,
    SyncNow,
    /// Record the sandbox an older build started the node in as its
    /// location, after checking it is the node's and answers.
    AdoptSandbox,
}

impl RunnerAction {
    pub fn label(self) -> &'static str {
        match self {
            RunnerAction::Start => "Start",
            RunnerAction::Resume => "Resume",
            RunnerAction::ResumeFreshBudget => "Resume with a fresh budget",
            RunnerAction::RunFresh => "Run with a new session",
            RunnerAction::Pause => "Pause",
            RunnerAction::StopNow => "Stop now",
            RunnerAction::CheckNow => "Check now",
            RunnerAction::RerunPhase => "Rerun this phase",
            RunnerAction::MigrateTo => "Migrate to\u{2026}",
            RunnerAction::ResetEnvironment => "Reset environment",
            RunnerAction::UpdateWorkspace => "Update workspace to latest main",
            RunnerAction::DeleteSandbox => "Delete sandbox",
            RunnerAction::DeleteSandboxAnyway => "Delete sandbox without a checkpoint",
            RunnerAction::SyncNow => "Sync now",
            RunnerAction::AdoptSandbox => "Adopt existing sandbox",
        }
    }
}

/// The split button: the default action (the button itself, if any) and the
/// entries of its menu, in display order. The default is never repeated in
/// the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerActions {
    pub default: Option<RunnerAction>,
    pub menu: Vec<RunnerAction>,
    /// While a cloud job of the panel's own runs: the default action shows
    /// this, disabled, and the menu is empty.
    pub busy: Option<&'static str>,
}

pub fn menu(inputs: &RunnerInputs) -> RunnerActions {
    if let Some(what) = inputs.busy {
        return RunnerActions {
            default: None,
            menu: Vec::new(),
            busy: Some(what),
        };
    }

    use RunnerAction::*;
    let idle = matches!(inputs.run, RunState::NeverRun | RunState::Ended(_));
    let done = inputs.run == RunState::Done;

    let default = match inputs.run {
        RunState::NeverRun => Some(Start),
        RunState::Active { .. } => Some(Pause),
        RunState::HandTurn => Some(StopNow),
        RunState::Ended(EndedHow::Budget) => Some(ResumeFreshBudget),
        RunState::Ended(EndedHow::Waiting) => None,
        RunState::Ended(_) => Some(Resume),
        RunState::Done => None,
    };

    let mut entries = Vec::new();
    // Start's and Resume's companions: a new session, or the budget reset.
    match inputs.run {
        RunState::Ended(EndedHow::Waiting) => {}
        RunState::Ended(EndedHow::Budget) | RunState::NeverRun => entries.push(RunFresh),
        RunState::Ended(_) => entries.extend([ResumeFreshBudget, RunFresh]),
        _ => {}
    }
    if matches!(inputs.run, RunState::Active { .. }) {
        entries.push(StopNow);
    }
    if !done && inputs.adoptable {
        entries.push(AdoptSandbox);
    }
    if idle {
        entries.push(CheckNow);
        entries.push(RerunPhase);
    }
    if !done && !inputs.has_children {
        entries.push(MigrateTo);
    }
    if !done && matches!(inputs.place, Place::DevContainer | Place::Sandbox { .. }) {
        entries.push(ResetEnvironment);
    }
    if inputs.has_checkout {
        entries.push(UpdateWorkspace);
    }
    if inputs.place == (Place::Sandbox { exists: true }) {
        entries.extend([DeleteSandbox, DeleteSandboxAnyway]);
    }
    if inputs.supervised {
        entries.push(SyncNow);
    }

    // The default is the button; it is not also a menu entry.
    entries.retain(|a| Some(*a) != default);
    RunnerActions {
        default,
        menu: entries,
        busy: None,
    }
}

impl RunnerFacts {
    /// The locations "Migrate to…" can take the node to: every location but
    /// the one it is already in (the app's submenu shows that one marked and
    /// disabled; `tod-cli runner menu` leaves it out).
    pub fn migration_targets(&self) -> Vec<&tod_store::fleet::FilesPlace> {
        self.locations.iter().filter(|p| Some(p.id.as_str()) != self.current_location.as_deref()).collect()
    }

    /// Refuses a migration to the location the node is already in, saying so.
    pub fn check_migration_target(&self, place: &tod_store::fleet::FilesPlace) -> anyhow::Result<()> {
        if Some(place.id.as_str()) == self.current_location.as_deref() {
            anyhow::bail!(
                "the node is already in location `{}`: migrating there would only remove its files and start nothing",
                place.name
            );
        }
        Ok(())
    }
}

/// What `load_runner` reads: the store half of [`RunnerLine`].
#[derive(Debug, Default, Clone)]
pub struct RunnerFacts {
    pub lifecycle: String,
    pub cloud: Option<CloudNode>,
    pub cloud_sandbox: Option<String>,
    pub adoptable: bool,
    pub place: Option<Place>,
    pub supervised: bool,
    pub has_checkout: bool,
    pub has_children: bool,
    pub locations: Vec<tod_store::fleet::FilesPlace>,
    pub current_location: Option<String>,
    pub place_text: String,
    pub problems: Vec<tod_store::node_problems::NodeProblem>,
    pub error: Option<String>,
}

/// Everything the runner menu needs from the store: the node's lifecycle
/// state, its cloud row, where its files are (and whether a sandbox exists),
/// where its runner runs, whether it has a checkout of its own and children,
/// and the global locations. Reads only, but not for the UI thread. A read
/// that fails is said, never taken for "no".
pub fn load_runner(fleet: &tod_store::fleet::FleetStore, node_id: Uuid) -> RunnerFacts {
    let mut facts = RunnerFacts::default();
    let id = node_id.to_string();
    facts.lifecycle = fleet
        .get_node(&id)
        .ok()
        .flatten()
        .map(|n| n.lifecycle)
        .unwrap_or_default();
    // A row left behind by a node that has since left the cloud says nothing
    // about where it runs now (the next sync drops it).
    facts.cloud = crate::cloud_sync::cloud_node(fleet, &id);
    if facts.cloud.is_some() {
        match crate::cloud_sync::lost::left_the_cloud(fleet, node_id) {
            Ok(true) => facts.cloud = None,
            Ok(false) => {}
            Err(err) => {
                tracing::error!("could not read where node {node_id} runs: {err:#}");
                facts.error = Some(format!("Could not read where this node runs: {err:#}"));
            }
        }
    }
    match crate::node_problems::for_node(fleet, node_id) {
        Ok(problems) => facts.problems = problems,
        Err(err) => {
            tracing::error!("could not read the problems of node {node_id}: {err:#}");
            facts.error = Some(format!("Could not read this node's problems: {err:#}"));
        }
    }
    match load_place(fleet, node_id, &mut facts) {
        Ok(()) => {}
        Err(err) => {
            tracing::error!("could not read where node {node_id} runs: {err:#}");
            facts.error = Some(format!("Could not read where this node runs: {err:#}"));
        }
    }
    facts
}

fn load_place(fleet: &tod_store::fleet::FleetStore, node_id: Uuid, facts: &mut RunnerFacts) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let id = node_id.to_string();
    let files = fleet.resolve_files_for_node(&id).context("read the node's Files")?;
    let location = tod_store::fleet::provision::node_location(fleet, &id).context("read the node's location")?;
    facts.supervised = crate::runner_place::RunnerPlace::of(fleet, node_id)? == crate::runner_place::RunnerPlace::Supervisor;
    facts.has_checkout = location.is_some();
    facts.has_children = fleet.node_has_children(node_id).context("look for the node's children")?;
    facts.locations = tod_store::fleet::places::list(fleet).context("list the locations")?;
    facts.current_location = files.as_ref().and_then(|f| f.place_id.clone());
    let sandbox_made = location.as_ref().and_then(|l| l.sandbox().map(str::to_string));
    facts.cloud_sandbox = sandbox_made.clone();
    facts.adoptable = crate::cloud_sync::adopt::adoptable(fleet, fleet.paths().root(), &id)
        .context("look for an old sandbox record to adopt")?;
    let (place, text) = match files.as_ref().and_then(|files| files.dev_container.as_ref()) {
        Some(dev) if dev.sandbox => {
            // The sandbox is made when something first needs the node's
            // files, so it can exist before the runner is started.
            // Only the node's Files location says which sandbox it has.
            let exists = sandbox_made.is_some();
            let text = match &sandbox_made {
                Some(name) => format!("sandbox {name}"),
                None => "cloud sandbox, not made yet".to_string(),
            };
            (Place::Sandbox { exists }, text)
        }
        Some(dev) => match dev.container.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(container) => (Place::DevContainer, format!("this machine, dev container {container}")),
            None => (Place::DevContainer, "this machine, a dev container".to_string()),
        },
        None => (Place::Host, "this machine".to_string()),
    };
    facts.place = Some(place);
    facts.place_text = text;
    Ok(())
}

/// What `run_state` reads.
pub struct RunFacts<'a> {
    pub done: bool,
    pub supervised: bool,
    /// The daemon (or this process) is running the node's autopilot.
    pub runner_running: bool,
    pub pausing: bool,
    /// A conversation the user started by hand is running.
    pub hand_turn: bool,
    /// How the last local run ended.
    pub outcome: Option<&'a Outcome>,
    pub actor: Option<&'a NextActor>,
    /// Pause was asked of a supervisor that has not stopped yet.
    pub pause_asked: bool,
}

/// Where the node's run stands (`doc/runner-menu/design.md`, "The runner's
/// state, wherever it runs"): locally from the runner's own state, else, and
/// for a supervised node, from who is next (`crate::next_actor`).
pub fn derive_run_state(f: &RunFacts) -> RunState {
    if f.done {
        return RunState::Done;
    }
    if f.runner_running {
        return RunState::Active { pausing: f.pausing };
    }
    if f.hand_turn {
        return RunState::HandTurn;
    }
    if f.supervised && matches!(f.actor, Some(NextActor::Working { .. })) {
        return RunState::Active { pausing: f.pause_asked };
    }
    // The last local run's own word, when it has one.
    match f.outcome {
        // A spent budget, unless today's budget has outgrown the stop: who is
        // next judged it again from the run's saved counts
        // (`AutopilotState::budget_stop_outgrown`), and then it is an
        // ordinary stop that a plain Resume carries on.
        Some(Outcome::BudgetExhausted { .. }) => {
            let outgrown = matches!(f.actor, Some(NextActor::You { why, .. }) if crate::next_actor::budget_outgrown(why));
            return RunState::Ended(if outgrown { EndedHow::Stopped } else { EndedHow::Budget });
        }
        Some(Outcome::Stopped { .. }) => return RunState::Ended(EndedHow::Stopped),
        // The run stopped for a request: that is its end only while
        // who-is-next still shows one, and shows no other end beside it (a
        // failed or stopped run under a request is that failure: the answer
        // would not make it go on). A stop for a request is stale once none
        // is left (the run continues by itself: `next_actor::outcome_reason`).
        // A stop for any other reason is not decided here: who-is-next
        // below says how it ended.
        Some(Outcome::NeedsHuman { reason }) if reason.is_request() => {
            let still_asking = matches!(
                f.actor,
                Some(NextActor::You {
                    why: YouReason::Request { ended: None, .. } | YouReason::Permission { ended: None, .. },
                    ..
                })
            ) || !matches!(
                f.actor,
                Some(NextActor::You { .. }) | Some(NextActor::Others { .. }) | Some(NextActor::Working { .. })
            );
            if still_asking {
                return RunState::Ended(EndedHow::Request);
            }
        }
        Some(Outcome::NeedsHuman { .. }) => {}
        Some(Outcome::Waiting { .. }) => return RunState::Ended(EndedHow::Waiting),
        Some(Outcome::Done) | None => {}
    }
    match f.actor {
        Some(NextActor::You { why, .. }) => ended_how(why),
        // Waiting on something else, or worked by a process this one has not
        // heard from: there is nothing to start or resume.
        Some(NextActor::Others { .. }) | Some(NextActor::Working { .. }) => RunState::Ended(EndedHow::Waiting),
        Some(NextActor::Done) => RunState::Done,
        None => RunState::NeverRun,
    }
}

/// How the run ended, from who-is-next's reason. A request or permission with
/// an `ended` beside it is that end (the answer would not make the run go on).
fn ended_how(why: &YouReason) -> RunState {
    match why {
        YouReason::Request { ended: Some(ended), .. } | YouReason::Permission { ended: Some(ended), .. } => {
            ended_how(ended)
        }
        YouReason::Request { .. } | YouReason::Permission { .. } => RunState::Ended(EndedHow::Request),
        YouReason::Paused => RunState::Ended(EndedHow::Paused),
        YouReason::Stopped { reason } if crate::next_actor::is_budget_stop(reason) => RunState::Ended(EndedHow::Budget),
        YouReason::Stopped { .. } => RunState::Ended(EndedHow::Stopped),
        YouReason::Failed { .. } => RunState::Ended(EndedHow::Failed),
        YouReason::NotStarted => RunState::NeverRun,
    }
}

/// What a check found, in words for the runner line, and whether it is a
/// failure (red): the gate's verdict and where it went, then what reading
/// GitHub found, an unreadable GitHub included.
pub fn describe_check(found: &crate::lifecycle::CheckNow) -> (String, bool) {
    use crate::autopilot::GithubCheck;
    let mut failed = false;
    let mut text = if let Some(state) = found.advanced {
        format!("Checked: the gate is clear; moved to {}", tod_store::settings::lifecycle_display_name(state))
    } else if found.gate.to.is_none() {
        format!("Checked: {} has no gate", found.gate.from)
    } else if found.gate.clear() {
        match &found.held {
            Some(why) => format!("Checked: the gate is clear, but the node was not moved on: {why}"),
            None => "Checked: the gate is clear".to_string(),
        }
    } else {
        let failing: Vec<String> = found.gate.failing().map(|c| format!("{} ({})", c.criterion.label, c.detail)).collect();
        format!("Checked: the gate is not clear: {}", failing.join("; "))
    };
    match &found.github {
        Some(GithubCheck::Waiting { what, .. }) => text.push_str(&format!(". GitHub: waiting for {what}")),
        Some(GithubCheck::Feedback { what }) => text.push_str(&format!(". GitHub: {what}")),
        Some(GithubCheck::Clear) => text.push_str(". GitHub: nothing left to wait for"),
        Some(GithubCheck::Unreadable(why)) => {
            failed = true;
            text.push_str(&format!(". GitHub could not be read: {why}"));
        }
        Some(GithubCheck::NothingToWaitOn) | None => {}
    }
    if found.needs_agent {
        text.push_str(". Starting the agent to do what is left");
    }
    (text, failed)
}


#[cfg(test)]
mod tests {
    use super::*;
    use RunnerAction::*;

    fn inputs(run: RunState) -> RunnerInputs {
        RunnerInputs {
            place: Place::Host,
            supervised: false,
            run,
            has_checkout: false,
            has_children: false,
            adoptable: false,
            busy: None,
        }
    }

    fn sandbox(run: RunState) -> RunnerInputs {
        RunnerInputs {
            place: Place::Sandbox { exists: true },
            supervised: true,
            has_checkout: true,
            ..inputs(run)
        }
    }

    fn all(a: &RunnerActions) -> Vec<RunnerAction> {
        a.default
            .into_iter()
            .chain(a.menu.iter().copied())
            .collect()
    }

    #[test]
    fn start_is_the_default_for_a_node_never_run() {
        let a = menu(&inputs(RunState::NeverRun));
        assert_eq!(a.default, Some(Start));
        assert!(a.menu.contains(&RunFresh));
        assert!(a.menu.contains(&CheckNow));
        assert!(!all(&a).contains(&Resume));
    }

    #[test]
    fn resume_is_the_default_after_every_end_but_budget_and_waiting() {
        for how in [
            EndedHow::Paused,
            EndedHow::Stopped,
            EndedHow::Failed,
            EndedHow::Request,
        ] {
            let a = menu(&inputs(RunState::Ended(how)));
            assert_eq!(a.default, Some(Resume), "{how:?}");
            assert!(a.menu.contains(&ResumeFreshBudget), "{how:?}");
            assert!(a.menu.contains(&RunFresh), "{how:?}");
            assert!(a.menu.contains(&CheckNow), "{how:?}");
        }
    }

    #[test]
    fn after_a_spent_budget_a_fresh_budget_is_the_default_and_plain_resume_is_hidden() {
        let a = menu(&inputs(RunState::Ended(EndedHow::Budget)));
        assert_eq!(a.default, Some(ResumeFreshBudget));
        assert!(!all(&a).contains(&Resume));
        assert!(a.menu.contains(&RunFresh));
    }

    #[test]
    fn a_run_that_ended_to_wait_offers_no_resume_and_no_default() {
        let a = menu(&inputs(RunState::Ended(EndedHow::Waiting)));
        assert_eq!(a.default, None);
        let shown = all(&a);
        for hidden in [Resume, ResumeFreshBudget, RunFresh, Start] {
            assert!(!shown.contains(&hidden), "{hidden:?}");
        }
        assert!(a.menu.contains(&CheckNow));
    }

    #[test]
    fn pause_is_the_default_while_running_and_stop_now_is_in_the_menu() {
        let a = menu(&inputs(RunState::Active { pausing: false }));
        assert_eq!(a.default, Some(Pause));
        assert!(a.menu.contains(&StopNow));
        let shown = all(&a);
        for hidden in [Start, Resume, ResumeFreshBudget, RunFresh] {
            assert!(!shown.contains(&hidden), "{hidden:?}");
        }
    }

    #[test]
    fn stop_now_is_the_default_for_a_turn_started_by_hand() {
        let a = menu(&inputs(RunState::HandTurn));
        assert_eq!(a.default, Some(StopNow));
        assert!(!all(&a).contains(&Pause));
        assert!(!a.menu.contains(&StopNow));
    }

    #[test]
    fn check_now_only_while_idle_and_not_done() {
        for run in [
            RunState::Active { pausing: false },
            RunState::Active { pausing: true },
            RunState::HandTurn,
            RunState::Done,
        ] {
            assert!(!all(&menu(&inputs(run))).contains(&CheckNow), "{run:?}");
        }
        for run in [
            RunState::NeverRun,
            RunState::Ended(EndedHow::Paused),
            RunState::Ended(EndedHow::Budget),
        ] {
            assert!(all(&menu(&inputs(run))).contains(&CheckNow), "{run:?}");
        }
    }

    #[test]
    fn migrate_to_needs_a_node_without_children_that_is_not_done() {
        assert!(menu(&inputs(RunState::NeverRun)).menu.contains(&MigrateTo));
        // It asks first while the runner is active, so it is still offered.
        assert!(
            menu(&inputs(RunState::Active { pausing: false }))
                .menu
                .contains(&MigrateTo)
        );
        // A hand-started conversation may also be stopped before migration;
        // the shared availability check must leave that prompt reachable.
        for run in [RunState::Active { pausing: false }, RunState::HandTurn] {
            let actions = menu(&inputs(run));
            crate::runner_operator::ensure_offered(&actions, run, MigrateTo).unwrap();
        }
        let actions = menu(&inputs(RunState::HandTurn));
        crate::runner_operator::ensure_offered(&actions, RunState::HandTurn, StopNow).unwrap();
        let with_children = RunnerInputs {
            has_children: true,
            ..inputs(RunState::NeverRun)
        };
        assert!(!all(&menu(&with_children)).contains(&MigrateTo));
        assert!(!all(&menu(&inputs(RunState::Done))).contains(&MigrateTo));
    }

    #[test]
    fn reset_environment_for_a_container_or_sandbox_never_a_host_worktree() {
        assert!(!all(&menu(&inputs(RunState::NeverRun))).contains(&ResetEnvironment));
        let container = RunnerInputs {
            place: Place::DevContainer,
            ..inputs(RunState::NeverRun)
        };
        assert!(menu(&container).menu.contains(&ResetEnvironment));
        assert!(
            menu(&sandbox(RunState::NeverRun))
                .menu
                .contains(&ResetEnvironment)
        );
        let none_yet = RunnerInputs {
            place: Place::Sandbox { exists: false },
            ..inputs(RunState::NeverRun)
        };
        assert!(menu(&none_yet).menu.contains(&ResetEnvironment));
    }

    #[test]
    fn update_workspace_only_with_a_checkout_of_its_own() {
        assert!(!all(&menu(&inputs(RunState::NeverRun))).contains(&UpdateWorkspace));
        let own = RunnerInputs {
            has_checkout: true,
            ..inputs(RunState::NeverRun)
        };
        assert!(menu(&own).menu.contains(&UpdateWorkspace));
    }

    #[test]
    fn delete_sandbox_only_for_a_sandbox_the_node_has_and_shown_while_active() {
        assert!(!all(&menu(&inputs(RunState::NeverRun))).contains(&DeleteSandbox));
        let container = RunnerInputs {
            place: Place::DevContainer,
            ..inputs(RunState::NeverRun)
        };
        assert!(!all(&menu(&container)).contains(&DeleteSandbox));
        let none_yet = RunnerInputs {
            place: Place::Sandbox { exists: false },
            ..inputs(RunState::NeverRun)
        };
        let shown = all(&menu(&none_yet));
        assert!(!shown.contains(&DeleteSandbox));
        assert!(!shown.contains(&DeleteSandboxAnyway));
        for run in [
            RunState::NeverRun,
            RunState::Active { pausing: false },
            RunState::HandTurn,
            RunState::Done,
        ] {
            let a = menu(&sandbox(run));
            assert!(a.menu.contains(&DeleteSandbox), "{run:?}");
            assert!(a.menu.contains(&DeleteSandboxAnyway), "{run:?}");
        }
    }

    #[test]
    fn sync_now_only_when_supervised() {
        assert!(!all(&menu(&inputs(RunState::NeverRun))).contains(&SyncNow));
        assert!(menu(&sandbox(RunState::NeverRun)).menu.contains(&SyncNow));
        assert!(menu(&sandbox(RunState::Done)).menu.contains(&SyncNow));
    }

    #[test]
    fn a_done_node_offers_only_delete_sandbox_update_workspace_and_sync() {
        let a = menu(&sandbox(RunState::Done));
        assert_eq!(a.default, None);
        assert_eq!(
            a.menu,
            vec![UpdateWorkspace, DeleteSandbox, DeleteSandboxAnyway, SyncNow]
        );
        assert!(menu(&inputs(RunState::Done)).menu.is_empty());
    }

    #[test]
    fn a_busy_panel_shows_what_it_is_doing_and_an_empty_menu() {
        let busy = RunnerInputs {
            busy: Some("Deleting the sandbox\u{2026}"),
            ..sandbox(RunState::Ended(EndedHow::Failed))
        };
        let a = menu(&busy);
        assert_eq!(a.default, None);
        assert!(a.menu.is_empty());
        assert_eq!(a.busy, Some("Deleting the sandbox\u{2026}"));
    }

    #[test]
    fn the_default_is_never_repeated_in_the_menu() {
        for run in [
            RunState::NeverRun,
            RunState::HandTurn,
            RunState::Active { pausing: true },
            RunState::Ended(EndedHow::Paused),
            RunState::Ended(EndedHow::Budget),
        ] {
            let a = menu(&sandbox(run));
            if let Some(d) = a.default {
                assert!(!a.menu.contains(&d), "{run:?}");
            }
        }
    }

    #[test]
    fn adopt_existing_sandbox_only_when_the_node_is_in_the_adoptable_state() {
        let adoptable = |run, place| RunnerInputs { adoptable: true, place, ..inputs(run) };
        let not_made = Place::Sandbox { exists: false };
        for run in [RunState::NeverRun, RunState::Ended(EndedHow::Failed), RunState::Active { pausing: false }] {
            assert!(menu(&adoptable(run, not_made)).menu.contains(&AdoptSandbox), "{run:?}");
        }
        assert!(!all(&menu(&adoptable(RunState::Done, not_made))).contains(&AdoptSandbox));
        assert!(!all(&menu(&sandbox(RunState::NeverRun))).contains(&AdoptSandbox), "a located node has nothing to adopt");
        assert!(!all(&menu(&inputs(RunState::NeverRun))).contains(&AdoptSandbox));
        // A running job hides the whole menu, this entry with it.
        let busy = RunnerInputs { busy: Some("x"), ..adoptable(RunState::NeverRun, not_made) };
        assert!(menu(&busy).menu.is_empty());
    }

    #[test]
    fn labels_match_the_design() {
        assert_eq!(AdoptSandbox.label(), "Adopt existing sandbox");
        assert_eq!(MigrateTo.label(), "Migrate to\u{2026}");
        assert_eq!(
            DeleteSandboxAnyway.label(),
            "Delete sandbox without a checkpoint"
        );
        assert_eq!(ResumeFreshBudget.label(), "Resume with a fresh budget");
    }
    fn place(id: &str, name: &str) -> tod_store::fleet::FilesPlace {
        tod_store::fleet::FilesPlace { id: id.into(), name: name.into(), ..Default::default() }
    }

    #[test]
    fn migrating_to_the_location_the_node_is_in_is_not_offered_and_is_refused() {
        let facts = RunnerFacts {
            locations: vec![place("a", "Here"), place("b", "There")],
            current_location: Some("a".into()),
            ..Default::default()
        };
        let names: Vec<&str> = facts.migration_targets().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["There"]);
        let err = facts.check_migration_target(&facts.locations[0]).unwrap_err().to_string();
        assert!(err.contains("already in location `Here`"), "{err}");
        facts.check_migration_target(&facts.locations[1]).unwrap();
    }

    #[test]
    fn a_stop_for_a_request_is_stale_once_who_is_next_shows_none() {
        use crate::autopilot::NeedsHuman;
        let outcome = Outcome::NeedsHuman { reason: NeedsHuman::Decision { pending: 1 } };
        let run = |actor: &NextActor| {
            derive_run_state(&RunFacts {
                done: false,
                supervised: false,
                runner_running: false,
                pausing: false,
                hand_turn: false,
                outcome: Some(&outcome),
                actor: Some(actor),
                pause_asked: false,
            })
        };
        let you = |why| NextActor::You { why, since: 1 };
        assert_eq!(run(&you(YouReason::Request { count: 1, ended: None })), RunState::Ended(EndedHow::Request));
        assert_eq!(run(&you(YouReason::NotStarted)), RunState::NeverRun);
    }

    #[test]
    fn a_request_or_permission_beside_an_ended_run_reads_as_that_end() {
        use crate::autopilot::NeedsHuman;
        let run = |outcome: Option<&Outcome>, actor: &NextActor| {
            derive_run_state(&RunFacts {
                done: false,
                supervised: true,
                runner_running: false,
                pausing: false,
                hand_turn: false,
                outcome,
                actor: Some(actor),
                pause_asked: false,
            })
        };
        let boxed = |why: YouReason| Some(Box::new(why));
        let failed = || boxed(YouReason::Failed { error: "push failed".into() });
        let asked = |ended| NextActor::You { why: YouReason::Request { count: 1, ended }, since: 1 };
        let permission = |ended| NextActor::You { why: YouReason::Permission { title: "rm".into(), ended }, since: 1 };
        assert_eq!(run(None, &asked(failed())), RunState::Ended(EndedHow::Failed));
        assert_eq!(run(None, &permission(failed())), RunState::Ended(EndedHow::Failed));
        assert_eq!(run(None, &asked(boxed(YouReason::Paused))), RunState::Ended(EndedHow::Paused));
        assert_eq!(
            run(None, &asked(boxed(YouReason::Stopped { reason: "budget spent: 6 sessions".into() }))),
            RunState::Ended(EndedHow::Budget)
        );
        assert_eq!(run(None, &asked(None)), RunState::Ended(EndedHow::Request));
        // A NeedsHuman-for-a-request outcome is the run's end only while the
        // request is the only thing: with a failure beside it, the failure is.
        let outcome = Outcome::NeedsHuman { reason: NeedsHuman::Decision { pending: 1 } };
        assert_eq!(run(Some(&outcome), &asked(None)), RunState::Ended(EndedHow::Request));
        assert_eq!(run(Some(&outcome), &asked(failed())), RunState::Ended(EndedHow::Failed));
    }
}
