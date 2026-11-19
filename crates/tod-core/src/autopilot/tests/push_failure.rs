//! A failed push of the node's branch at a step boundary is shown on the node
//! (shepherd gap 71): it was only logged, so nobody learned the branch was not
//! on GitHub until a later step or Reset environment failed.

use super::lost_work::{git_in, workspace_is_a_clone};
use super::*;

/// The node's worktree is the clone, so `push_node_branch` has a branch to push.
fn node_works_in(fx: &Fixture, workspace: &std::path::Path) {
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

#[test]
fn a_diverged_branch_names_the_repository_and_is_never_forced() {
    let fx = setup();
    let workspace = workspace_is_a_clone(&fx);
    node_works_in(&fx, &workspace);
    git_in(&workspace, &["commit", "-q", "--allow-empty", "-m", "ours"]);
    git_in(&workspace, &["push", "-q", "origin", "task/x"]);
    let other = fx.root.join("other");
    git_in(&fx.root, &["clone", "-q", "-b", "task/x", fx.root.join("origin.git").to_str().unwrap(), other.to_str().unwrap()]);
    git_in(&other, &["commit", "-q", "--allow-empty", "-m", "theirs"]);
    git_in(&other, &["push", "-q", "origin", "task/x"]);
    git_in(&workspace, &["commit", "-q", "--allow-empty", "-m", "ours 2"]);
    git_in(&workspace, &["update-ref", "-d", "refs/remotes/origin/task/x"]);

    let outcome = crate::autopilot::local::push_branch(&fx.fleet, fx.node).unwrap().expect("refused");
    let Outcome::Stopped { reason } = &outcome else { panic!("{outcome:?}") };
    assert!(reason.starts_with(crate::autopilot::local::FAILED_PREFIX) && reason.contains("repository"), "{reason}");
    assert!(reason.contains("rejected") || reason.contains("non-fast-forward"), "{reason}");
}

#[test]
fn a_network_failure_waits_with_the_error_and_a_retry_count_and_gives_up_after_the_last() {
    let fx = setup();
    let error = "repository /w: push task/x: git push: fatal: unable to access 'https://github.com/o/r.git/': Could not resolve host: github.com";
    let outcome = crate::autopilot::branch_push_failed(&fx.fleet, fx.node, error).unwrap();
    let Outcome::Waiting { what, due_at_ms } = &outcome else { panic!("{outcome:?}") };
    assert!(what.contains("1 of") && what.contains("Could not resolve host"), "{what}");
    assert!(*due_at_ms > chrono::Utc::now().timestamp_millis());
    let mut last = outcome;
    for _ in 0..30 {
        last = crate::autopilot::branch_push_failed(&fx.fleet, fx.node, error).unwrap();
        if !matches!(last, Outcome::Waiting { .. }) {
            break;
        }
    }
    let Outcome::Stopped { reason } = &last else { panic!("retries are bounded: {last:?}") };
    assert!(reason.starts_with(crate::autopilot::local::FAILED_PREFIX) && reason.contains("Could not resolve host"), "{reason}");
    assert!(push_waits(&fx)[0].exhausted());
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
