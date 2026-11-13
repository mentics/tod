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
    let repos = crate::lost_work::inspect(&cwd, None).unwrap();
    assert!(crate::lost_work::is_lost(&repos), "{repos:?}");
    let proof = crate::lost_work::check(&fx.fleet, fx.node, "review").unwrap().expect("lost");
    assert_eq!(proof.done_steps, 2);
    crate::lost_work::reopen(&fx.fleet, fx.node, "review", &proof).unwrap();
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "active");
    let statuses: Vec<String> = fx
        .fleet
        .read(|c| Ok(PlanStepRepo::new(c).list_for_node(fx.node)?.into_iter().map(|s| s.status).collect()))
        .unwrap();
    assert!(statuses.iter().all(|s| s == "ready" || s == "pending"), "{statuses:?}");
    // Nothing more to find once the steps are open.
    assert!(crate::lost_work::check(&fx.fleet, fx.node, "verifying").unwrap().is_none());
}

#[test]
fn a_branch_with_commits_of_its_own_is_never_reopened() {
    let fx = setup();
    let workspace = workspace_is_a_clone(&fx);
    std::fs::write(workspace.join("work"), "w").unwrap();
    git_in(&workspace, &["add", "."]);
    git_in(&workspace, &["commit", "-qm", "the node's work"]);
    in_review_with_done_steps(&fx);
    assert!(crate::lost_work::check(&fx.fleet, fx.node, "review").unwrap().is_none());
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "review");
}

#[test]
fn a_node_with_no_done_steps_is_not_lost_work() {
    let fx = setup();
    workspace_is_a_clone(&fx);
    assert!(crate::lost_work::check(&fx.fleet, fx.node, "review").unwrap().is_none());
}

#[test]
fn lost_work_is_only_looked_for_where_implementation_is_claimed_done() {
    let fx = setup();
    workspace_is_a_clone(&fx);
    in_review_with_done_steps(&fx);
    for state in ["design", "planning", "active", "merged", "pr_draft"] {
        assert!(crate::lost_work::check(&fx.fleet, fx.node, state).unwrap().is_none(), "{state}");
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
    let proof = crate::lost_work::check(&fx.fleet, fx.node, "review").unwrap().expect("lost");
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
