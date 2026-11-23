//! A failed push of the node's branch at a step boundary is shown on the node
//! (shepherd gap 71): it was only logged, so nobody learned the branch was not
//! on GitHub until a later step or Reset environment failed.

use super::lost_work::{git_in, workspace_is_a_clone};
use super::*;
use crate::autopilot::PushRetrier;
use crate::autopilot::local::{Hook, LocalEvent, PAUSED, PUSH_RETRY, Request, push_at_run_end, push_retry_as_wait};
use std::sync::atomic::{AtomicBool, AtomicU8};

/// The node's worktree is the clone, so `push_node_branch` has a branch to push.
pub(super) fn node_works_in(fx: &Fixture, workspace: &std::path::Path) {
    fx.fleet
        .enqueue(tod_store::fleet::FleetMutation::RecordFilesLocation {
            location: tod_store::fleet::FilesLocation {
                node_id: fx.node.to_string(),
                source_node_id: fx.node.to_string(),
                recipe: "test".into(),
                repo: Some(workspace.display().to_string()),
                container: None,
                worktree_path: Some(workspace.display().to_string()),
                worktree_lease_id: None,
                worktree_lease_holder: None,
                sandbox: None,
                created_at: 1,
            },
        })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    fx.fleet.reload_if_stale().unwrap();
}

fn push_waits(fx: &Fixture) -> Vec<tod_store::waits::Wait> {
    fx.fleet
        .read(|c| tod_store::waits::WaitRepo::new(c).list_pending_for_node(fx.node))
        .unwrap()
        .into_iter()
        .filter(|w| w.match_spec == PUSH_WAIT_SPEC)
        .collect()
}

/// The remote refuses every push to a `task/` branch, as a protected branch
/// or a declining hook does.
fn remote_refuses(fx: &Fixture, refuse: bool) {
    let hook = fx.root.join("origin.git/hooks/pre-receive");
    if refuse {
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\necho 'GH006: Protected branch update failed' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    } else {
        std::fs::remove_file(&hook).unwrap();
    }
}

#[test]
fn a_push_the_remote_refuses_stops_the_run_failed_with_git_s_words_and_the_repository() {
    let fx = setup();
    let workspace = workspace_is_a_clone(&fx);
    node_works_in(&fx, &workspace);
    git_in(&workspace, &["commit", "-q", "--allow-empty", "-m", "the fix"]);
    remote_refuses(&fx, true);

    let outcome = crate::autopilot::local::push_branch(&fx.fleet, fx.node).unwrap().expect("a failure is an outcome");
    let Outcome::Stopped { reason } = &outcome else { panic!("{outcome:?}") };
    assert!(reason.starts_with(crate::autopilot::local::FAILED_PREFIX), "{reason}");
    assert!(reason.contains("Protected branch update failed"), "git's own words: {reason}");
    assert!(reason.contains("repository") && reason.contains("workspace"), "names the repository: {reason}");

    // On the node, counted, and the user's: a refusal is the same every time.
    let waits = push_waits(&fx);
    assert_eq!(waits.len(), 1, "{waits:?}");
    assert!(waits[0].exhausted(), "{:?}", waits[0]);
    assert!(waits[0].last_error.as_deref().is_some_and(|e| e.contains("Protected branch")));

    // Once the remote takes it, the failure ends.
    remote_refuses(&fx, false);
    assert!(crate::autopilot::local::push_branch(&fx.fleet, fx.node).unwrap().is_none());
    assert!(push_waits(&fx).is_empty(), "a push that succeeds clears the failure");
}

/// The runner's hook, as `LocalRun` builds it.
fn hook<'a>(fx: &Fixture, sink: &'a mut dyn FnMut(LocalEvent)) -> Hook<'a, &'a mut dyn FnMut(LocalEvent)> {
    Hook::new(
        fx.node,
        fx.fleet.clone(),
        Arc::new(AtomicU8::new(Request::Run as u8)),
        Arc::new(AtomicBool::new(false)),
        Box::leak(Box::new(sink)),
    )
}

/// A checkout with a commit to push and a remote that can be made to fail
/// the way the network does (`origin` pointing nowhere) and recover.
struct Outage {
    workspace: PathBuf,
    origin: PathBuf,
}

impl Outage {
    fn new(fx: &Fixture) -> Self {
        let workspace = workspace_is_a_clone(fx);
        node_works_in(fx, &workspace);
        git_in(&workspace, &["commit", "-q", "--allow-empty", "-m", "the fix"]);
        Self { workspace, origin: fx.root.join("origin.git") }
    }
    fn down(&self) {
        git_in(&self.workspace, &["remote", "set-url", "origin", self.workspace.join("nowhere.git").to_str().unwrap()]);
    }
    fn up(&self) {
        git_in(&self.workspace, &["remote", "set-url", "origin", self.origin.to_str().unwrap()]);
    }
}

fn run_at_step(hook: &mut Hook<'_, impl FnMut(LocalEvent)>, fx: &Fixture) -> Option<String> {
    use crate::autopilot::StepHook;
    hook.at(&fx.fleet, Boundary::Step).unwrap()
}

fn log_of(dir: &std::path::Path, branch: &str) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(["log", "--format=%s", branch]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn an_outage_within_the_budget_never_fails_the_node_and_counts_the_real_attempts() {
    let fx = setup();
    let outage = Outage::new(&fx);
    outage.down();
    let mut sink = |_: LocalEvent| {};
    let mut hook = hook(&fx, &mut sink);
    let max = tod_store::waits::DEFAULT_MAX_ATTEMPTS as usize;
    // The longest outage that is still within budget: one attempt short of it.
    for attempt in 1..max {
        let stopped = run_at_step(&mut hook, &fx);
        assert_eq!(stopped.as_deref(), Some(PUSH_RETRY), "attempt {attempt} waits, never fails");
        // The run ends as the wait, the way `LocalRun` ends it.
        let (what, due) = hook.push_retry.take().expect("what to say and when");
        assert!(what.contains(&format!("({attempt} of {max})")), "{what}");
        assert!(due > chrono::Utc::now().timestamp_millis());
        let ended = push_retry_as_wait(
            fx.fleet.paths().root(),
            fx.node,
            Outcome::Stopped { reason: PUSH_RETRY.into() },
            Some((what, due)),
        )
        .unwrap();
        assert!(matches!(ended, Outcome::Waiting { .. }), "{ended:?}");
        let waits = push_waits(&fx);
        assert_eq!((waits.len(), waits[0].attempt as usize), (1, attempt), "{waits:?}");
        assert!(!waits[0].exhausted());
        // The run that ended pushes nothing more at its end: this outage is
        // one attempt, not two.
        let after = push_at_run_end(&fx.fleet, fx.fleet.paths().root(), fx.node, ended.clone(), hook.push_failed).unwrap();
        assert_eq!(after, ended);
        assert_eq!(push_waits(&fx)[0].attempt as usize, attempt, "the run-end push did not count again");
    }
    // The network is back: the next boundary push clears it.
    outage.up();
    assert_eq!(run_at_step(&mut hook, &fx), None);
    assert!(push_waits(&fx).is_empty(), "a push that succeeds clears the failure");
    assert!(log_of(&outage.origin, "task/x").contains("the fix"), "the branch is on the remote");
}

#[test]
fn an_outage_beyond_the_budget_ends_failed_with_the_last_error_and_the_count() {
    let fx = setup();
    let outage = Outage::new(&fx);
    outage.down();
    let mut sink = |_: LocalEvent| {};
    let mut hook = hook(&fx, &mut sink);
    let max = tod_store::waits::DEFAULT_MAX_ATTEMPTS as usize;
    for _ in 1..max {
        assert_eq!(run_at_step(&mut hook, &fx).as_deref(), Some(PUSH_RETRY));
    }
    let last = run_at_step(&mut hook, &fx).expect("the last attempt stops the run");
    assert!(last.starts_with(crate::autopilot::local::FAILED_PREFIX), "{last}");
    assert!(last.contains(&format!("after {max} of {max} tries")) && last.contains("nowhere.git"), "{last}");
    assert!(push_waits(&fx)[0].exhausted());
}

#[test]
fn the_run_end_push_after_a_failed_boundary_push_is_not_a_second_attempt() {
    let fx = setup();
    let outage = Outage::new(&fx);
    outage.down();
    let mut sink = |_: LocalEvent| {};
    let mut boundary = hook(&fx, &mut sink);
    assert_eq!(run_at_step(&mut boundary, &fx).as_deref(), Some(PUSH_RETRY));
    assert!(boundary.push_failed);
    let ended = Outcome::Stopped { reason: PUSH_RETRY.into() };
    assert_eq!(push_at_run_end(&fx.fleet, fx.fleet.paths().root(), fx.node, ended.clone(), boundary.push_failed).unwrap(), ended);
    assert_eq!(push_waits(&fx)[0].attempt, 1);
    // A run whose boundary pushes were fine does push at its end, and counts it once.
    let after = push_at_run_end(&fx.fleet, fx.fleet.paths().root(), fx.node, Outcome::Done, false).unwrap();
    let Outcome::Waiting { what, .. } = &after else { panic!("{after:?}") };
    assert!(what.contains("(2 of"), "the second real attempt: {what}");
    assert_eq!(push_waits(&fx)[0].attempt, 2);
}

#[test]
fn a_transient_run_end_failure_leaves_no_wait_nothing_retries() {
    let fx = setup();
    let outage = Outage::new(&fx);
    outage.down();
    let now = chrono::Utc::now().timestamp_millis();
    // A run that ends for a reason that does not resume by itself.
    let paused = Outcome::Stopped { reason: PAUSED.into() };
    let after = push_at_run_end(&fx.fleet, fx.fleet.paths().root(), fx.node, paused.clone(), false).unwrap();
    assert_eq!(after, paused, "the run's own end is not changed");
    let waits = push_waits(&fx);
    assert_eq!(waits.len(), 1, "{waits:?}");
    assert!(waits[0].last_error.as_deref().is_some_and(|e| e.contains("nowhere.git")), "the error is on the node");
    assert_eq!(waits[0].attempt, 1);
    // Nothing is due soon, so it never goes past due as "nothing is looking at it".
    assert!(waits[0].due_at > now + 24 * 60 * 60 * 1000, "{:?}", waits[0]);
    assert!(waits[0].expired_by(now + 3 * 60 * 60 * 1000, false).is_none());
    // When the run goes on, its next push clears it.
    outage.up();
    assert!(crate::autopilot::local::push_branch(&fx.fleet, fx.node).unwrap().is_none());
    assert!(push_waits(&fx).is_empty());

    // A run that ends waiting is resumed by the daemon: it ends as the wait
    // on the push (the earlier time), which the daemon serves.
    outage.down();
    git_in(&outage.workspace, &["commit", "-q", "--allow-empty", "-m", "more"]);
    let later = now + 3_600_000;
    let waiting = Outcome::Waiting { what: "a review".into(), due_at_ms: later };
    let after = push_at_run_end(&fx.fleet, fx.fleet.paths().root(), fx.node, waiting, false).unwrap();
    let Outcome::Waiting { what, due_at_ms } = &after else { panic!("{after:?}") };
    assert!(what.contains("GitHub did not take the branch") && *due_at_ms < later, "{what}");
    let saved = crate::autopilot::AutopilotState::load(fx.fleet.paths().root(), fx.node).unwrap();
    assert_eq!(saved.outcome, Some(after.clone()), "saved, so the daemon resumes the run");
    assert!(push_waits(&fx)[0].due_at <= *due_at_ms + 1, "the wait is due when its retrier is");
}

fn local_wake(fx: &Fixture, user_go_on: bool) -> Outcome {
    let agent: tod_agent::SharedAgent =
        Arc::new(std::sync::Mutex::new(Box::new(FakeAgent::new(&fx.fleet))));
    let (tx, rx) = std::sync::mpsc::channel();
    let run = crate::autopilot::local::LocalRun::start_with(
        fx.fleet.clone(),
        agent,
        config(fx),
        fx.node,
        Budget { max_sessions: 0, ..Budget::default() },
        false,
        user_go_on,
        Some(Duration::ZERO),
        move |event| {
            if let LocalEvent::Finished(result) = event {
                tx.send(result).unwrap();
            }
        },
    )
    .unwrap();
    let outcome = rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    run.join();
    outcome
}

#[test]
fn a_branch_retry_saves_the_commit_and_restores_the_provider_wait_without_a_turn() {
    for (what, user_go_on) in [(Outcome::ACP_QUOTA_WAIT, false), (Outcome::ACP_QUOTA_DAILY_WAIT, true)] {
        let fx = setup();
        let outage = Outage::new(&fx);
        outage.down();
        let root = fx.fleet.paths().root();
        let quota_at = state::now_ms() + 24 * 60 * 60 * 1000;
        let quota = Outcome::Waiting { what: what.into(), due_at_ms: quota_at };
        let first = push_at_run_end(&fx.fleet, root, fx.node, quota.clone(), false).unwrap();
        assert!(matches!(&first, Outcome::Waiting { due_at_ms, .. } if *due_at_ms < quota_at));
        let saved = AutopilotState::load(root, fx.node).unwrap();
        assert_eq!(saved.outcome, Some(first));
        assert_eq!(saved.quota_after_push.as_ref(), Some(&quota));
        assert_eq!(push_waits(&fx)[0].attempt, 1);

        // The new runner reloads the checkpoint at each simulated wake.
        // Another failed push counts once and leaves the same quota deadline.
        let retry = local_wake(&fx, false);
        assert!(matches!(&retry, Outcome::Waiting { due_at_ms, .. } if *due_at_ms < quota_at));
        assert_eq!(push_waits(&fx)[0].attempt, 2);
        assert_eq!(AutopilotState::load(root, fx.node).unwrap().quota_after_push.as_ref(), Some(&quota));

        // Recovery saves the branch at its own wake. Entering the normal
        // autopilot here would exhaust the zero-session budget.
        outage.up();
        assert_eq!(local_wake(&fx, false), quota);
        assert!(log_of(&outage.origin, "task/x").contains("the fix"));
        assert!(push_waits(&fx).is_empty());
        let mut saved = AutopilotState::load(root, fx.node).unwrap();
        assert_eq!(saved.outcome.as_ref(), Some(&quota));
        assert!(saved.quota_after_push.is_none(), "the quota is the primary wake again");

        if !user_go_on {
            // Advance the saved clock without sleeping. An automatic quota
            // wake now takes the normal path, as does an explicit Resume.
            saved.outcome = Some(Outcome::Waiting { what: what.into(), due_at_ms: 0 });
            saved.save(root, fx.node).unwrap();
        }
        assert!(matches!(local_wake(&fx, user_go_on), Outcome::BudgetExhausted { .. }));
        assert!(AutopilotState::load(root, fx.node).unwrap().quota_after_push.is_none());
    }
}

#[test]
fn a_remote_that_moved_between_the_look_and_the_push_is_refetched_and_decided_again() {
    let fx = setup();
    let workspace = workspace_is_a_clone(&fx);
    node_works_in(&fx, &workspace);
    git_in(&workspace, &["commit", "-q", "--allow-empty", "-m", "ours"]);
    // Someone else takes our commit and adds one, in the instant between
    // the fetch that decided "push" and the push itself.
    let origin = fx.root.join("origin.git");
    let other = fx.root.join("other");
    git_in(&fx.root, &["clone", "-q", origin.to_str().unwrap(), other.to_str().unwrap()]);
    let mark = fx.root.join("moved");
    let script = format!(
        "#!/bin/sh\n[ -e '{mark}' ] && exit 0\ntouch '{mark}'\nunset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE\ncd '{other}' || exit 1\n\
         git fetch -q '{ws}' task/x || exit 1\ngit -c user.name=t -c user.email=t@t checkout -q -B task/x FETCH_HEAD || exit 1\n\
         git -c user.name=t -c user.email=t@t commit -q --allow-empty -m theirs || exit 1\ngit push -q origin task/x || exit 1\n",
        mark = mark.display(),
        other = other.display(),
        ws = workspace.display(),
    );
    let hook_path = workspace.join(".git/hooks/pre-push");
    std::fs::write(&hook_path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut sink = |_: LocalEvent| {};
    let mut hook = hook(&fx, &mut sink);
    assert_eq!(run_at_step(&mut hook, &fx), None, "the remote has our commit and more: nothing of ours to send, no failure");
    assert!(mark.exists(), "the remote did move during the push");
    assert!(push_waits(&fx).is_empty());
    assert!(!hook.push_failed);
    assert!(log_of(&origin, "task/x").starts_with("theirs"), "never forced");
}

#[test]
fn a_diverged_branch_stops_the_run_failed_saying_how_to_resolve_it_and_is_never_forced() {
    let fx = setup();
    let workspace = workspace_is_a_clone(&fx);
    node_works_in(&fx, &workspace);
    git_in(&workspace, &["commit", "-q", "--allow-empty", "-m", "ours"]);
    git_in(&workspace, &["push", "-q", "origin", "task/x"]);
    let other = fx.root.join("other");
    git_in(&fx.root, &["clone", "-q", "-b", "task/x", fx.root.join("origin.git").to_str().unwrap(), other.to_str().unwrap()]);
    git_in(&other, &["commit", "-q", "--allow-empty", "-m", "theirs"]);
    git_in(&other, &["push", "-q", "origin", "task/x"]);
    // The agent rebased on purpose: its history no longer has what was pushed.
    git_in(&workspace, &["reset", "-q", "--hard", "HEAD~1"]);
    git_in(&workspace, &["commit", "-q", "--allow-empty", "-m", "ours, rewritten"]);
    git_in(&workspace, &["update-ref", "-d", "refs/remotes/origin/task/x"]);

    let mut sink = |_: LocalEvent| {};
    let mut hook = hook(&fx, &mut sink);
    let reason = run_at_step(&mut hook, &fx).expect("a divergence stops the run");
    assert!(reason.starts_with(crate::autopilot::local::FAILED_PREFIX), "{reason}");
    for needle in [
        "repository",
        "task/x has diverged from origin/task/x",
        "1 commit(s) that origin does not, and origin has 2",
        "git rebase origin/task/x",
        "git merge origin/task/x",
        "git log --oneline --left-right HEAD...origin/task/x",
        "git push --force-with-lease origin HEAD:refs/heads/task/x",
        "never force-pushes",
    ] {
        assert!(reason.contains(needle), "{needle}: {reason}");
    }
    assert!(hook.push_retry.is_none(), "a divergence is not retried");
    let waits = push_waits(&fx);
    assert!(waits[0].exhausted(), "{waits:?}");
    assert!(log_of(&fx.root.join("origin.git"), "task/x").starts_with("theirs"), "never forced");
}

#[test]
fn a_network_failure_waits_with_the_error_and_a_retry_count_and_gives_up_after_the_last() {
    let fx = setup();
    let error = "repository /w: push task/x: git push: fatal: unable to access 'https://github.com/o/r.git/': Could not resolve host: github.com";
    let outcome = crate::autopilot::branch_push_failed(&fx.fleet, fx.node, error, PushRetrier::Run).unwrap();
    let Outcome::Waiting { what, due_at_ms } = &outcome else { panic!("{outcome:?}") };
    assert!(what.contains("1 of") && what.contains("Could not resolve host"), "{what}");
    assert!(*due_at_ms > chrono::Utc::now().timestamp_millis());
    let mut last = outcome;
    for _ in 0..30 {
        last = crate::autopilot::branch_push_failed(&fx.fleet, fx.node, error, PushRetrier::Run).unwrap();
        if !matches!(last, Outcome::Waiting { .. }) {
            break;
        }
    }
    let Outcome::Stopped { reason } = &last else { panic!("retries are bounded: {last:?}") };
    assert!(reason.starts_with(crate::autopilot::local::FAILED_PREFIX) && reason.contains("Could not resolve host"), "{reason}");
    assert!(push_waits(&fx)[0].exhausted());
}

#[test]
fn an_error_nobody_has_classified_is_retried_not_failed() {
    let fx = setup();
    let error = "repository /w: push task/x: git push: kex_exchange_identification: Connection closed by remote host";
    let outcome = crate::autopilot::branch_push_failed(&fx.fleet, fx.node, error, PushRetrier::Run).unwrap();
    assert!(matches!(outcome, Outcome::Waiting { .. }), "{outcome:?}");
}

#[test]
fn nothing_to_send_and_no_checkout_of_its_own_fail_nothing() {
    let fx = setup();
    // No files location of its own: it shares the workspace.
    assert!(crate::autopilot::local::push_branch(&fx.fleet, fx.node).unwrap().is_none());
    // On the remote already: never attempted, so a refusing remote is not asked.
    let workspace = workspace_is_a_clone(&fx);
    node_works_in(&fx, &workspace);
    assert!(crate::autopilot::local::push_branch(&fx.fleet, fx.node).unwrap().is_none());
    remote_refuses(&fx, true);
    assert!(crate::autopilot::local::push_branch(&fx.fleet, fx.node).unwrap().is_none());
    assert!(push_waits(&fx).is_empty());
}

/// What a re-check of an overdue wait shows (gap 79): the node's run state
/// reads `waiting` and a run resumed to look again does no work, so it must not
/// write `working` in between (every write is a change to watchers, and the
/// push look can take minutes). `since` moves on every write, so an unchanged
/// `since` proves there was none.
fn wait_row(fx: &Fixture) -> tod_store::run_state::RunState {
    fx.fleet.read(|conn| tod_store::run_state::get(conn, fx.node)).unwrap().unwrap()
}

#[test]
fn a_re_check_of_a_wait_with_nothing_to_push_keeps_its_waiting_row_and_never_writes_working() {
    use crate::autopilot::StepHook;
    let fx = setup();
    let workspace = workspace_is_a_clone(&fx);
    node_works_in(&fx, &workspace);
    // The remote already has everything.
    git_in(&workspace, &["push", "-q", "origin", "HEAD"]);
    crate::run_state::record(
        &fx.fleet,
        fx.node,
        crate::run_state::waiting_outside("pull request #1 to be marked ready for review"),
        "test",
    )
    .unwrap();
    let before = wait_row(&fx);

    let mut sink = |_: LocalEvent| {};
    let mut hook = hook(&fx, &mut sink);
    hook.resume(&fx.fleet).unwrap();
    // The run's re-check: a step boundary, the gate's "not working" hand-offs,
    // then the wait again, and a boundary of the sleep.
    assert_eq!(run_at_step(&mut hook, &fx), None);
    hook.waiting(None, None);
    assert_eq!(run_at_step(&mut hook, &fx), None);
    hook.waiting(Some("pull request #1 to be marked ready for review"), Some(1));
    assert_eq!(hook.at(&fx.fleet, Boundary::Wait).unwrap(), None);

    let after = wait_row(&fx);
    assert_eq!(after.since, before.since, "no write in between: {after:?}");
    assert_eq!(after.reason_kind, tod_core_kind_waiting());
}

fn tod_core_kind_waiting() -> String {
    crate::run_state::kind::WAITING_OUTSIDE.to_string()
}

#[test]
fn a_waiting_run_says_working_once_a_push_is_needed() {
    let fx = setup();
    let outage = Outage::new(&fx);
    outage.up();
    crate::run_state::record(&fx.fleet, fx.node, crate::run_state::waiting_outside("a release"), "test").unwrap();
    let mut sink = |_: LocalEvent| {};
    let mut hook = hook(&fx, &mut sink);
    hook.resume(&fx.fleet).unwrap();
    assert_eq!(wait_row(&fx).reason_kind, tod_core_kind_waiting());
    assert_eq!(run_at_step(&mut hook, &fx), None);
    let row = wait_row(&fx);
    assert_eq!(row.reason_kind, crate::run_state::kind::WORKING, "a real push is work: {row:?}");
}

/// A change the pull request's agent committed for tod's review is not
/// pushed by the runner (ENG-3558: the step boundary before the review sent
/// it to the pull request unreviewed): not at a step boundary, not when the
/// run ends. Once the change is no longer held, the next boundary pushes as
/// before, and a change saved in an earlier stay holds nothing.
#[test]
fn a_change_held_for_tod_s_review_is_not_pushed_by_the_runner() {
    let fx = setup();
    let outage = Outage::new(&fx);
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "pr_draft").unwrap();
    let data_root = fx.fleet.paths().root().to_path_buf();
    let mut state = AutopilotState::load(&data_root, fx.node).unwrap();
    state.pr_change = Some(crate::autopilot::state::PrChange {
        started_ms: state::now_ms(),
        before: String::new(),
        made_at_ms: Some(state::now_ms()),
        changed: true,
        conflict: false,
        held: false,
    });
    state.save(&data_root, fx.node).unwrap();
    assert!(crate::autopilot::holds_change(&fx.fleet, fx.node).unwrap());

    let mut sink = |_: LocalEvent| {};
    let mut hook = hook(&fx, &mut sink);
    assert_eq!(run_at_step(&mut hook, &fx), None);
    let outcome = push_at_run_end(&fx.fleet, &data_root, fx.node, Outcome::Done, false).unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert!(!log_of(&outage.origin, "task/x").contains("the fix"), "the held change stays off the remote");

    // In another state the saved change holds nothing (it was an earlier stay's).
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "pr_review").unwrap();
    std::thread::sleep(Duration::from_millis(5));
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "pr_draft").unwrap();
    assert!(!crate::autopilot::holds_change(&fx.fleet, fx.node).unwrap(), "a change from before this stay holds nothing");
    assert_eq!(run_at_step(&mut hook, &fx), None);
    assert!(log_of(&outage.origin, "task/x").contains("the fix"), "pushed once nothing holds it");
}
