//! What the task panel's runner split button offers, as a pure function of
//! one value (`doc/runner-menu/design.md`, "The runner menu" and "The
//! runner's state"). No GPUI and no store: the panel gathers a
//! [`RunnerInputs`] from the node runners, the node's standing and its Files
//! location, calls [`menu`], and draws the answer, so every rule is tested
//! without a window.
//!
//! Nothing calls this yet; the panel switches over in work item A8.

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
    /// A submenu of the locations; the panel lists them.
    MigrateTo,
    ResetEnvironment,
    UpdateWorkspace,
    DeleteSandbox,
    DeleteSandboxAnyway,
    SyncNow,
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
            RunnerAction::MigrateTo => "Migrate to\u{2026}",
            RunnerAction::ResetEnvironment => "Reset environment",
            RunnerAction::UpdateWorkspace => "Update workspace to latest main",
            RunnerAction::DeleteSandbox => "Delete sandbox",
            RunnerAction::DeleteSandboxAnyway => "Delete sandbox without a checkpoint",
            RunnerAction::SyncNow => "Sync now",
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
    if idle {
        entries.push(CheckNow);
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
    fn labels_match_the_design() {
        assert_eq!(MigrateTo.label(), "Migrate to\u{2026}");
        assert_eq!(
            DeleteSandboxAnyway.label(),
            "Delete sandbox without a checkpoint"
        );
        assert_eq!(ResumeFreshBudget.label(), "Resume with a fresh budget");
    }
}
