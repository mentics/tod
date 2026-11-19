//! The lost-work rule in the autopilot (`crate::lost_work`).

use super::*;

fn git_in(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// Makes the node's workspace a clone of a repository with one commit on
/// `main`, on the node's branch with nothing of its own.
fn workspace_is_a_clone(fx: &Fixture) -> PathBuf {
    let origin = fx.root.join("origin.git");
    let seed = fx.root.join("seed");
    let workspace = fx.root.join("workspace");
    std::fs::create_dir_all(&origin).unwrap();
    git_in(&origin, &["init", "-q", "--bare", "-b", "main"]);
    git_in(&fx.root, &["clone", "-q", origin.to_str().unwrap(), seed.to_str().unwrap()]);
    std::fs::write(seed.join("a"), "a").unwrap();
    git_in(&seed, &["add", "."]);
    git_in(&seed, &["commit", "-qm", "base"]);
    git_in(&seed, &["push", "-q", "origin", "HEAD:main"]);
    std::fs::remove_dir_all(&workspace).unwrap();
    git_in(&fx.root, &["clone", "-q", origin.to_str().unwrap(), workspace.to_str().unwrap()]);
    git_in(&workspace, &["checkout", "-q", "-b", "task/x"]);
    // What making the checkout records: the node's branch, and where it was born.
    fx.fleet
        .enqueue(tod_store::fleet::FleetMutation::UpdateTaskBranch { id: fx.node.to_string(), branch: Some("task/x".into()) })
        .unwrap();
    let starts = tod_store::fleet::checkout_start::capture(&tod_store::fleet::Workdir::host(&workspace), "task/x").unwrap();
    assert!(starts.contains_key("."), "{starts:?}");
    fx.fleet
        .enqueue(tod_store::fleet::FleetMutation::RecordBranchStarts { node_id: fx.node.to_string(), branch: "task/x".into(), starts })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    fx.fleet.reload_if_stale().unwrap();
    workspace
}

/// The node in `review` with its plan steps recorded as done.
fn in_review_with_done_steps(fx: &Fixture) {
    fx.fleet
        .enqueue_outline(OutlineMutation::SetLifecycle { node_id: fx.node, state: "review".into() })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
    conn.execute("UPDATE node_plan_steps SET status = 'verified'", []).unwrap();
    drop(conn);
    fx.fleet.reload_if_stale().ok();
}

fn ask_broken(fx: &Fixture, protocol: &str) {
    fx.fleet
        .interview(
            ACTOR_USER,
            InterviewCommand::AskDecision {
                node_id: fx.node,
                conversation_id: None,
                protocol: Some(protocol.into()),
                decision: tod_store::decisions::NewDecision {
                    question: "Restore this task's code to the review checkout".into(),
                    options: vec!["Fixed: try again".into()],
                    evidence: Vec::new(),
                    reason: tod_store::decisions::REASON_BROKEN.into(),
                    moves: Vec::new(),
                    ..Default::default()
                },
            },
        )
        .unwrap();
}

#[test]
fn a_node_whose_done_work_is_not_on_its_branch_is_reopened_not_asked_about() {
    let fx = setup();
    retire_outside_criteria(&fx);
    workspace_is_a_clone(&fx);
    in_review_with_done_steps(&fx);
    // What a reviewer asks when it finds the checkout holding only main.
    ask_broken(&fx, "review");
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, Budget::default());
    let outcome = pilot.run(&fx.fleet, &mut agent).unwrap();
    let steps = &pilot.state().steps;
    // Before any agent turn: back to active, no question to the user.
    assert_eq!(
        (steps[0].step.as_str(), steps[0].from.as_str(), steps[0].to.as_str()),
        ("lost_work", "review", "active"),
        "{steps:#?}"
    );
    assert_eq!(steps[1].step, "implementation", "{steps:#?}");
    let all = fx.fleet.read(|c| Ok(tod_store::decisions::DecisionRepo::new(c).list_for_node(fx.node)?)).unwrap();
    assert!(!all.is_empty() && all.iter().all(|d| d.status == "withdrawn"), "{all:?}");
    // It says why, on the node's activity.
    let timeline = fx.fleet.read(|c| tod_store::node_timeline::TimelineRepo::new(c).list(fx.node)).unwrap();
    let note = timeline.iter().find(|e| e.kind == tod_store::node_timeline::KIND_NOTE).expect("a note");
    assert_eq!(note.title, crate::lost_work::NOTE_TITLE);
    assert!(note.detail.contains("no commits of its own"), "{}", note.detail);
    // The agent here commits nothing, so the rebuilt work is lost again: the
    // run stops for the user rather than going round.
    assert!(matches!(&outcome, Outcome::NeedsHuman { reason: NeedsHuman::LostWork { .. } }), "{outcome:?} {steps:#?}");
    assert_eq!(steps.iter().filter(|s| s.step == "lost_work").count(), 1, "{steps:#?}");
}

#[test]
fn the_steps_and_verdicts_a_lost_node_had_are_open_work_again() {
    let fx = setup();
    workspace_is_a_clone(&fx);
    in_review_with_done_steps(&fx);
    let cwd = tod_store::fleet::Workdir::host(fx.root.join("workspace"));
    let starts = fx.fleet.read(|c| tod_store::branch_starts::get(c, &fx.node.to_string(), "task/x")).unwrap();
    let repos = crate::lost_work::inspect(&cwd, Some("task/x"), &starts).unwrap();
    assert!(crate::lost_work::is_lost(&repos), "{repos:?}");
    let proof = crate::lost_work::check(&fx.fleet, fx.node, "review").unwrap().lost().expect("lost");
    assert_eq!(proof.done_steps, 2);
    crate::lost_work::reopen(&fx.fleet, fx.node, "review", &proof).unwrap();
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "active");
    let statuses: Vec<String> = fx
        .fleet
        .read(|c| Ok(PlanStepRepo::new(c).list_for_node(fx.node)?.into_iter().map(|s| s.status).collect()))
        .unwrap();
    assert!(statuses.iter().all(|s| s == "ready" || s == "pending"), "{statuses:?}");
    // Nothing more to find once the steps are open.
    assert!(crate::lost_work::check(&fx.fleet, fx.node, "verifying").unwrap().lost().is_none());
}

#[test]
fn a_branch_with_commits_of_its_own_is_never_reopened() {
    let fx = setup();
    let workspace = workspace_is_a_clone(&fx);
    std::fs::write(workspace.join("work"), "w").unwrap();
    git_in(&workspace, &["add", "."]);
    git_in(&workspace, &["commit", "-qm", "the node's work"]);
    in_review_with_done_steps(&fx);
    assert!(crate::lost_work::check(&fx.fleet, fx.node, "review").unwrap().lost().is_none());
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "review");
}

#[test]
fn a_node_with_no_done_steps_is_not_lost_work() {
    let fx = setup();
    workspace_is_a_clone(&fx);
    assert!(crate::lost_work::check(&fx.fleet, fx.node, "review").unwrap().lost().is_none());
}

#[test]
fn lost_work_is_only_looked_for_where_implementation_is_claimed_done() {
    let fx = setup();
    workspace_is_a_clone(&fx);
    in_review_with_done_steps(&fx);
    for state in ["design", "planning", "active", "merged", "pr_draft"] {
        assert!(crate::lost_work::check(&fx.fleet, fx.node, state).unwrap().lost().is_none(), "{state}");
    }
}

#[test]
fn only_the_broken_questions_of_the_checking_agents_are_withdrawn() {
    let fx = setup();
    workspace_is_a_clone(&fx);
    in_review_with_done_steps(&fx);
    ask_broken(&fx, "review");
    ask_broken(&fx, "implementation");
    // A tool failure from the same agent is not about the missing work.
    fx.fleet
        .interview(
            ACTOR_USER,
            InterviewCommand::AskDecision {
                node_id: fx.node,
                conversation_id: None,
                protocol: Some("review".into()),
                decision: tod_store::decisions::NewDecision {
                    question: "The Linear token was rejected (401)".into(),
                    options: vec!["Fixed".into()],
                    evidence: Vec::new(),
                    reason: tod_store::decisions::REASON_BROKEN.into(),
                    moves: Vec::new(),
                    ..Default::default()
                },
            },
        )
        .unwrap();
    fx.fleet
        .interview(
            ACTOR_USER,
            InterviewCommand::AskDecision {
                node_id: fx.node,
                conversation_id: None,
                protocol: Some("review".into()),
                decision: tod_store::decisions::NewDecision {
                    question: "Which API?".into(),
                    options: vec!["A".into(), "B".into()],
                    evidence: Vec::new(),
                    ..Default::default()
                },
            },
        )
        .unwrap();
    let proof = crate::lost_work::check(&fx.fleet, fx.node, "review").unwrap().lost().expect("lost");
    crate::lost_work::reopen(&fx.fleet, fx.node, "review", &proof).unwrap();
    let pending: Vec<String> = fx
        .fleet
        .read(|c| Ok(tod_store::decisions::DecisionRepo::new(c).list_pending_for_node(fx.node)?))
        .unwrap()
        .into_iter()
        .map(|d| d.question)
        .collect();
    assert_eq!(pending.len(), 3, "{pending:?}");
    assert!(pending.iter().any(|q| q.contains("401")), "{pending:?}");
    assert!(pending.iter().any(|q| q == "Which API?"));
}

#[test]
fn a_stale_files_location_is_an_error_not_silence() {
    let fx = setup();
    in_review_with_done_steps(&fx);
    // The fixture's workspace is not a repository; remove it so Files is missing.
    std::fs::remove_dir_all(fx.root.join("workspace")).unwrap();
    assert!(crate::lost_work::check(&fx.fleet, fx.node, "review").is_err());
}

/// A branch fast-forward merged into main and deleted, in a node whose branch
/// start was never recorded (a node made before tod recorded it): nothing is
/// proven, so nothing is reopened, and the node's activity says so once.
#[test]
fn a_merged_branch_with_no_recorded_start_is_not_proven_lost_and_not_reopened() {
    let fx = setup();
    retire_outside_criteria(&fx);
    let workspace = workspace_is_a_clone(&fx);
    let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
    conn.execute("DELETE FROM node_branch_starts", []).unwrap();
    drop(conn);
    std::fs::write(workspace.join("work"), "w").unwrap();
    git_in(&workspace, &["add", "."]);
    git_in(&workspace, &["commit", "-qm", "the node's work"]);
    git_in(&workspace, &["checkout", "-q", "main"]);
    git_in(&workspace, &["merge", "-q", "--ff-only", "task/x"]);
    git_in(&workspace, &["push", "-q", "origin", "main"]);
    git_in(&workspace, &["branch", "-q", "-D", "task/x"]);
    git_in(&workspace, &["fetch", "-q", "--prune", "origin"]);
    in_review_with_done_steps(&fx);
    let finding = crate::lost_work::check(&fx.fleet, fx.node, "review").unwrap();
    assert!(matches!(finding, crate::lost_work::Finding::Unproven(_)), "{finding:?}");
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, Budget::default());
    let _ = pilot.run(&fx.fleet, &mut agent).unwrap();
    assert!(pilot.state().steps.iter().all(|s| s.step != "lost_work"), "{:#?}", pilot.state().steps);
    assert_ne!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "active");
    let timeline = fx.fleet.read(|c| tod_store::node_timeline::TimelineRepo::new(c).list(fx.node)).unwrap();
    assert_eq!(timeline.iter().filter(|e| e.title == "Lost work: not proven either way" && e.phase == "review").count(), 1, "{timeline:#?}");
}

/// The sandbox is replaced: a new checkout makes the node's branch fresh at
/// origin's main, and `make_location` records that birth through the store.
fn replace_checkout(fx: &Fixture, workspace: &std::path::Path) {
    git_in(workspace, &["fetch", "-q", "--prune", "origin"]);
    git_in(workspace, &["checkout", "-q", "-B", "task/x", "origin/main"]);
    let starts = tod_store::fleet::checkout_start::capture(&tod_store::fleet::Workdir::host(workspace), "task/x").unwrap();
    assert!(starts.contains_key("."), "{starts:?}");
    fx.fleet
        .enqueue(tod_store::fleet::FleetMutation::RecordBranchStarts { node_id: fx.node.to_string(), branch: "task/x".into(), starts })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    fx.fleet.reload_if_stale().unwrap();
}

fn assert_unproven_not_reopened(fx: &Fixture) {
    in_review_with_done_steps(fx);
    let finding = crate::lost_work::reopen_if_lost(&fx.fleet, fx.node).unwrap();
    let crate::lost_work::Finding::Unproven(reasons) = finding else { panic!("{finding:?}") };
    assert!(reasons[0].contains("recreated at a newer base") && reasons[0].contains("cannot be confirmed"), "{reasons:?}");
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "review");
}

/// P1: a branch never pushed, replaced at a base that moved on, is neither
/// called merged nor lost: nothing proves where its work went.
#[test]
fn an_unpushed_stub_replaced_at_a_moved_base_is_unproven() {
    let fx = setup();
    let workspace = workspace_is_a_clone(&fx);
    git_in(&workspace, &["checkout", "-q", "main"]);
    std::fs::write(workspace.join("later"), "l").unwrap();
    git_in(&workspace, &["add", "."]);
    git_in(&workspace, &["commit", "-qm", "main moves"]);
    git_in(&workspace, &["push", "-q", "origin", "main"]);
    replace_checkout(&fx, &workspace);
    assert_unproven_not_reopened(&fx);
}

/// P2/P3: work pushed and merged (fast-forward, or squashed), the branch
/// deleted locally and on origin, then the sandbox replaced: not lost.
#[test]
fn merged_and_deleted_then_replaced_is_never_lost() {
    for squash in [false, true] {
        let fx = setup();
        let workspace = workspace_is_a_clone(&fx);
        std::fs::write(workspace.join("work"), "w").unwrap();
        git_in(&workspace, &["add", "."]);
        git_in(&workspace, &["commit", "-qm", "the node's work"]);
        git_in(&workspace, &["push", "-q", "origin", "HEAD:refs/heads/task/x"]);
        git_in(&workspace, &["checkout", "-q", "main"]);
        if squash {
            std::fs::write(workspace.join("work"), "w").unwrap();
            git_in(&workspace, &["add", "."]);
            git_in(&workspace, &["commit", "-qm", "squashed"]);
        } else {
            git_in(&workspace, &["merge", "-q", "--ff-only", "task/x"]);
        }
        git_in(&workspace, &["push", "-q", "origin", "main"]);
        git_in(&workspace, &["branch", "-q", "-D", "task/x"]);
        git_in(&workspace, &["push", "-q", "origin", ":refs/heads/task/x"]);
        replace_checkout(&fx, &workspace);
        assert_unproven_not_reopened(&fx);
    }
}

/// The first record is kept: a replacement that finds the branch fresh at the
/// same commit changes nothing, a different one is kept apart.
#[test]
fn the_first_birth_is_kept_and_a_later_different_one_is_kept_apart() {
    let fx = setup();
    let workspace = workspace_is_a_clone(&fx);
    let first = fx.fleet.read(|c| tod_store::branch_starts::get(c, &fx.node.to_string(), "task/x")).unwrap();
    assert!(first.replaced.is_empty());
    git_in(&workspace, &["checkout", "-q", "main"]);
    std::fs::write(workspace.join("later"), "l").unwrap();
    git_in(&workspace, &["add", "."]);
    git_in(&workspace, &["commit", "-qm", "main moves"]);
    git_in(&workspace, &["push", "-q", "origin", "main"]);
    replace_checkout(&fx, &workspace);
    let after = fx.fleet.read(|c| tod_store::branch_starts::get(c, &fx.node.to_string(), "task/x")).unwrap();
    assert_eq!(after.born, first.born);
    assert_ne!(after.replaced.get("."), first.born.get("."));
    assert!(after.replaced.contains_key("."));
}

/// A node made before the record existed (no row), its work done, merged and
/// the branch deleted everywhere, then its sandbox replaced at main's tip: the
/// birth seen now is not the node's first, so it is never a stub to trust.
fn pre_existing_node_replaced(squash: bool, state: &str, skipped_first_capture: bool) {
    let fx = setup();
    retire_outside_criteria(&fx);
    let workspace = workspace_is_a_clone(&fx);
    // No row: made before the table existed, or the first capture skipped
    // because origin already had the branch.
    let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
    conn.execute("DELETE FROM node_branch_starts", []).unwrap();
    drop(conn);
    if skipped_first_capture {
        git_in(&workspace, &["push", "-q", "origin", "HEAD:refs/heads/task/x"]);
        git_in(&workspace, &["fetch", "-q", "origin"]);
        let skipped = tod_store::fleet::checkout_start::capture(&tod_store::fleet::Workdir::host(&workspace), "task/x").unwrap();
        assert!(!skipped.contains_key("."), "{skipped:?}");
        git_in(&workspace, &["push", "-q", "origin", ":refs/heads/task/x"]);
    }
    std::fs::write(workspace.join("work"), "w").unwrap();
    git_in(&workspace, &["add", "."]);
    git_in(&workspace, &["commit", "-qm", "the node's work"]);
    git_in(&workspace, &["push", "-q", "origin", "HEAD:refs/heads/task/x"]);
    git_in(&workspace, &["checkout", "-q", "main"]);
    if squash {
        std::fs::write(workspace.join("work"), "w").unwrap();
        git_in(&workspace, &["add", "."]);
        git_in(&workspace, &["commit", "-qm", "squashed"]);
    } else {
        git_in(&workspace, &["merge", "-q", "--ff-only", "task/x"]);
    }
    git_in(&workspace, &["push", "-q", "origin", "main"]);
    git_in(&workspace, &["branch", "-q", "-D", "task/x"]);
    git_in(&workspace, &["push", "-q", "origin", ":refs/heads/task/x"]);
    in_review_with_done_steps(&fx);
    if state != "review" {
        fx.fleet
            .enqueue_outline(OutlineMutation::SetLifecycle { node_id: fx.node, state: state.into() })
            .unwrap();
        fx.fleet.writer().flush().unwrap();
        fx.fleet.reload_if_stale().unwrap();
    }
    replace_checkout(&fx, &workspace);
    let finding = crate::lost_work::reopen_if_lost(&fx.fleet, fx.node).unwrap();
    assert!(matches!(finding, crate::lost_work::Finding::Unproven(_)), "{finding:?}");
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), state);
    let starts = fx.fleet.read(|c| tod_store::branch_starts::get(c, &fx.node.to_string(), "task/x")).unwrap();
    assert!(starts.born.is_empty(), "{starts:?}");
}

#[test]
fn a_node_with_no_record_and_done_work_merged_and_replaced_is_never_lost() {
    for squash in [false, true] {
        pre_existing_node_replaced(squash, "review", false);
    }
}

#[test]
fn a_skipped_first_capture_then_merged_and_replaced_is_never_lost() {
    pre_existing_node_replaced(false, "review", true);
}

#[test]
fn a_node_in_pr_with_no_record_merged_and_replaced_is_never_lost() {
    pre_existing_node_replaced(false, "pr", false);
}

fn set_every_step(fx: &Fixture, status: &str) {
    let steps: Vec<_> = fx
        .fleet
        .read(|c| Ok(tod_store::outline::repos::PlanStepRepo::new(c).list_for_node(fx.node)?))
        .unwrap();
    assert!(!steps.is_empty());
    for step in steps {
        fx.fleet
            .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                step_id: step.id,
                status: status.into(),
                note: None,
                reason: None,
            })
            .unwrap();
    }
    fx.fleet.writer().flush().unwrap();
    fx.fleet.reload_if_stale().unwrap();
}

/// S1: a node with no implementation conversation whose steps were done through
/// `update_status`, merged and deleted, the steps reset to pending, then the
/// sandbox replaced at main's tip, then the steps marked verified again with no
/// new commit. The reset must not make the birth look like the node's first.
#[test]
fn steps_reset_to_pending_before_a_replacement_do_not_make_it_a_first_checkout() {
    let fx = setup();
    retire_outside_criteria(&fx);
    let workspace = workspace_is_a_clone(&fx);
    let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
    conn.execute("DELETE FROM node_branch_starts", []).unwrap();
    drop(conn);
    set_every_step(&fx, "implemented");
    std::fs::write(workspace.join("work"), "w").unwrap();
    git_in(&workspace, &["add", "."]);
    git_in(&workspace, &["commit", "-qm", "the node's work"]);
    git_in(&workspace, &["push", "-q", "origin", "HEAD:refs/heads/task/x"]);
    git_in(&workspace, &["checkout", "-q", "main"]);
    git_in(&workspace, &["merge", "-q", "--ff-only", "task/x"]);
    git_in(&workspace, &["push", "-q", "origin", "main"]);
    git_in(&workspace, &["branch", "-q", "-D", "task/x"]);
    git_in(&workspace, &["push", "-q", "origin", ":refs/heads/task/x"]);
    set_every_step(&fx, "pending");
    fx.fleet
        .enqueue_outline(OutlineMutation::SetLifecycle { node_id: fx.node, state: "active".into() })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    replace_checkout(&fx, &workspace);
    set_every_step(&fx, "verified");
    fx.fleet
        .enqueue_outline(OutlineMutation::SetLifecycle { node_id: fx.node, state: "review".into() })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    fx.fleet.reload_if_stale().unwrap();
    let starts = fx.fleet.read(|c| tod_store::branch_starts::get(c, &fx.node.to_string(), "task/x")).unwrap();
    assert!(starts.born.is_empty(), "{starts:?}");
    let finding = crate::lost_work::reopen_if_lost(&fx.fleet, fx.node).unwrap();
    assert!(matches!(finding, crate::lost_work::Finding::Unproven(_)), "{finding:?}");
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "review");
}

/// S1 with nothing else to protect it: the node stays `proposed` (no activity,
/// no lifecycle state past `active`) until after the replacement, so only the
/// sticky mark says it worked. A node with no implementation conversation whose steps were done through
/// `update_status`, merged and deleted, the steps reset to pending, then the
/// sandbox replaced at main's tip, then the steps marked verified again with no
/// new commit. The reset must not make the birth look like the node's first.
#[test]
fn steps_reset_to_pending_on_a_proposed_node_are_remembered_by_the_sticky_mark_alone() {
    let fx = setup();
    retire_outside_criteria(&fx);
    let workspace = workspace_is_a_clone(&fx);
    let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
    conn.execute("DELETE FROM node_branch_starts", []).unwrap();
    drop(conn);
    set_every_step(&fx, "implemented");
    std::fs::write(workspace.join("work"), "w").unwrap();
    git_in(&workspace, &["add", "."]);
    git_in(&workspace, &["commit", "-qm", "the node's work"]);
    git_in(&workspace, &["push", "-q", "origin", "HEAD:refs/heads/task/x"]);
    git_in(&workspace, &["checkout", "-q", "main"]);
    git_in(&workspace, &["merge", "-q", "--ff-only", "task/x"]);
    git_in(&workspace, &["push", "-q", "origin", "main"]);
    git_in(&workspace, &["branch", "-q", "-D", "task/x"]);
    git_in(&workspace, &["push", "-q", "origin", ":refs/heads/task/x"]);
    set_every_step(&fx, "pending");
    let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
    conn.execute("DELETE FROM node_timeline", []).unwrap();
    conn.execute("UPDATE node_lifecycle SET state = 'proposed'", []).unwrap();
    drop(conn);
    fx.fleet.reload_if_stale().unwrap();
    replace_checkout(&fx, &workspace);
    set_every_step(&fx, "verified");
    fx.fleet
        .enqueue_outline(OutlineMutation::SetLifecycle { node_id: fx.node, state: "review".into() })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    fx.fleet.reload_if_stale().unwrap();
    let starts = fx.fleet.read(|c| tod_store::branch_starts::get(c, &fx.node.to_string(), "task/x")).unwrap();
    assert!(starts.born.is_empty(), "{starts:?}");
    let finding = crate::lost_work::reopen_if_lost(&fx.fleet, fx.node).unwrap();
    assert!(matches!(finding, crate::lost_work::Finding::Unproven(_)), "{finding:?}");
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "review");
}

/// A replaced sandbox leaves a note per repository on the node, so lost
/// submodule work is counted and not found only when an agent notices.
#[test]
fn a_replaced_sandbox_records_what_its_checkout_holds_of_the_nodes_work() {
    let fx = setup();
    workspace_is_a_clone(&fx);
    in_review_with_done_steps(&fx);
    let lost = crate::lost_work::record_replacement(&fx.fleet, fx.node, "old-box", "new-box").unwrap();
    assert!(lost.is_empty(), "no submodules here: {lost:?}");
    let timeline = fx.fleet.read(|c| tod_store::node_timeline::TimelineRepo::new(c).list(fx.node)).unwrap();
    let note = timeline.iter().find(|e| e.title == crate::lost_work::REPLACED_TITLE).expect("a note");
    assert!(note.detail.contains("old-box") && note.detail.contains("new-box") && note.detail.contains(".: Untouched"), "{}", note.detail);
    assert!(!note.failed);
}

#[test]
fn a_submodule_with_no_commit_of_the_nodes_own_is_lost_work_only_when_steps_were_done() {
    use crate::lost_work::{RepoState, lost_submodules};
    let repos = vec![
        (".".to_string(), RepoState::OwnCommits),
        ("kb".to_string(), RepoState::Untouched),
        ("ui".to_string(), RepoState::OwnCommits),
        ("infra".to_string(), RepoState::Unknown("no origin".into())),
    ];
    assert_eq!(lost_submodules(&repos, 2), vec!["kb".to_string()]);
    assert!(lost_submodules(&repos, 0).is_empty(), "nothing was done, nothing was lost");
    let only_root = vec![(".".to_string(), RepoState::Untouched)];
    assert!(lost_submodules(&only_root, 2).is_empty(), "the checkout itself is not a submodule");
}

fn in_state_with_done_steps(fx: &Fixture, state: &str) {
    fx.fleet.enqueue_outline(OutlineMutation::SetLifecycle { node_id: fx.node, state: state.into() }).unwrap();
    fx.fleet.writer().flush().unwrap();
    let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
    conn.execute("UPDATE node_plan_steps SET status = 'verified'", []).unwrap();
    drop(conn);
    fx.fleet.reload_if_stale().ok();
}

/// Gap 57 (ENG-3311): steps verified, the checkout holds none of the work, and
/// nothing records where the branch started (a node moved to a new checkout).
/// The branch proves nothing in general, but in `verifying` no pull request can
/// have merged it, so it is lost; in `review` it stays unproven.
#[test]
fn verifying_with_no_work_on_a_branch_with_no_birth_record_is_lost() {
    let fx = setup();
    workspace_is_a_clone(&fx);
    let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
    conn.execute("DELETE FROM node_branch_starts", []).unwrap();
    drop(conn);
    in_state_with_done_steps(&fx, "verifying");
    let finding = crate::lost_work::check(&fx.fleet, fx.node, "verifying").unwrap();
    assert!(matches!(finding, crate::lost_work::Finding::Lost(_)), "{finding:?}");
    let finding = crate::lost_work::check(&fx.fleet, fx.node, "review").unwrap();
    assert!(matches!(finding, crate::lost_work::Finding::Unproven(_)), "{finding:?}");
}

#[test]
fn the_work_gate_criterion_fails_on_a_branch_without_the_work_and_passes_with_it() {
    let fx = setup();
    let workspace = workspace_is_a_clone(&fx);
    let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
    conn.execute("DELETE FROM node_branch_starts", []).unwrap();
    drop(conn);
    in_state_with_done_steps(&fx, "verifying");
    assert!(crate::lost_work::code_missing(&fx.fleet, fx.node).unwrap().is_some());
    let c = crate::lost_work::gate_criterion(&fx.fleet, fx.node, "verifying", "review").unwrap().expect("a criterion");
    assert_eq!(c.outcome, tod_store::outline::OUTCOME_FAIL);
    assert!(crate::lost_work::is_work_criterion(&c.criterion));
    // Not looked at where the claim is not made.
    assert!(crate::lost_work::gate_criterion(&fx.fleet, fx.node, "review", "pr").unwrap().is_none());
    // Uncommitted work counts: it is in the branch's checkout.
    std::fs::write(workspace.join("work"), "w").unwrap();
    assert!(crate::lost_work::code_missing(&fx.fleet, fx.node).unwrap().is_none());
    let c = crate::lost_work::gate_criterion(&fx.fleet, fx.node, "verifying", "review").unwrap().expect("a criterion");
    assert_eq!(c.outcome, tod_store::outline::OUTCOME_PASS);
}

#[test]
fn a_plan_that_says_no_code_change_needs_no_work_on_the_branch() {
    let fx = setup();
    workspace_is_a_clone(&fx);
    in_state_with_done_steps(&fx, "verifying");
    let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
    conn.execute("UPDATE node_plan_steps SET body = 'Confirm the setting. No code change.' WHERE rowid = (SELECT MIN(rowid) FROM node_plan_steps)", []).unwrap();
    drop(conn);
    fx.fleet.reload_if_stale().ok();
    assert!(crate::lost_work::code_missing(&fx.fleet, fx.node).unwrap().is_none());
}

/// Gap 57: the implement protocol is done when the steps are closed and the
/// tests green, but not while the branch holds none of the work.
#[test]
fn implementation_is_not_done_while_the_branch_holds_none_of_the_work() {
    use crate::conversation::implement::{ImplementationProtocol, TestRun};
    use crate::conversation::protocol::{Next, Protocol, ProtocolEnv, Stop, TurnContext};
    let fx = setup();
    let workspace = workspace_is_a_clone(&fx);
    in_state_with_done_steps(&fx, "active");
    let media = crate::media::MediaPaths::discover().expect("media paths");
    let run = TestRun { command: "t".into(), passed: 3, failed: 0, errors: 0 };
    let report = serde_json::to_value(&run).unwrap();
    let decide = || {
        let env = ProtocolEnv {
            fleet: &fx.fleet,
            media: &media,
            data_root: &fx.root,
            conversation_id: uuid::Uuid::new_v4(),
            focus: tod_store::conversation::Focus::Node(fx.node),
        };
        ImplementationProtocol
            .next(&TurnContext { env: &env, report: Some(&report), continuations: 0, progressed: true })
            .unwrap()
    };
    assert!(matches!(decide(), Next::Continue { .. }));
    std::fs::write(workspace.join("work"), "w").unwrap();
    assert!(matches!(decide(), Next::Done(Stop::Complete)));
}
