//! Run native PR actions against the same simulated GitHub states as legacy.

use super::*;
use std::sync::Mutex;
use tod_store::github::{BaseProof, NodePr, PrSnapshot, PrStatus};

const APP_PR: &str = "https://github.com/acme/app/pull/42";
const INFRA_PR: &str = "https://github.com/acme/infra/pull/7";

// Pair normally retires criteria for unrelated phases in the shared fixture.
// The two PR phases under test must evaluate the real exit gate.
fn restore_pr_gate(task: &Fixture, phase: &str) {
    let conn = rusqlite::Connection::open(task.fleet.paths().db()).unwrap();
    let changed = conn
        .execute(
            "UPDATE gate_criteria SET active = 1 WHERE from_state = ?1",
            [phase],
        )
        .unwrap();
    assert!(changed > 0, "expected seeded criteria for {phase}");
    task.fleet.reload_if_stale().unwrap();
}

#[test]
fn pr_feedback_is_reviewed_fixed_and_pushed_in_both_engines() {
    for phase in ["pr_draft", "pr_review"] {
        let pair = Pair::with_tasks(automated_default(), phase, &[APP_PR], configured_task);
        let exercise = |task: &Fixture| {
            let dir = git_workspace(task);
            with_pr_draft_review(task);
            restore_pr_gate(task, phase);
            let feed = BranchFeed::new(
                &dir,
                vec![review_thread("PRRT_1", "src/lib.rs", 2, "Rename b.")],
                false,
            );
            crate::pr_readiness::set_feed_override(Some(feed.clone()));
            let mut agent = branch_agent(task, &dir, &feed, edit_and_commit);
            let outcome = run_babysitter(task, &mut agent);
            let steps = steps_of(&turn_labels(&agent));
            let head = git_in(&dir, &["rev-parse", "HEAD"]);
            assert_eq!(
                *feed.pushed.lock().unwrap(),
                head,
                "{phase}: {outcome:?} {steps:?}"
            );
            assert!(
                feed.threads
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|thread| thread.resolved)
            );
            assert_eq!(feed.posts.load(Ordering::SeqCst), 0);
            let heads = [("acme/app#42".to_string(), head)].into();
            assert!(
                task.fleet
                    .read(|conn| crate::pr_draft_review::gate(conn, task.node, &heads))
                    .unwrap()
                    .is_ok()
            );
            let pushes = pr_pushes(task);
            assert!(pushes.iter().all(|(verdict, _)| verdict != "unreviewed"));
            assert!(
                review_findings_of(task)
                    .iter()
                    .all(|finding| !finding.is_open())
            );
            (steps, pushes, pending_specs(task), outcome)
        };
        let (legacy_steps, legacy_pushes, legacy_waits, legacy_outcome) = exercise(&pair.legacy);
        let (workflow_steps, workflow_pushes, workflow_waits, workflow_outcome) =
            exercise(&pair.workflow);
        let repair = [
            "pr:change",
            "guidance",
            "review",
            "fix",
            "review",
            "pr:push",
        ];
        assert!(
            legacy_steps
                .windows(repair.len())
                .any(|part| part.iter().map(String::as_str).eq(repair)),
            "{phase}: {legacy_steps:?}"
        );
        assert_eq!(workflow_steps, legacy_steps, "{phase}");
        assert_eq!(workflow_pushes, legacy_pushes, "{phase}");
        assert_eq!(
            workflow_waits, legacy_waits,
            "{phase}: {legacy_outcome:?} vs {workflow_outcome:?}"
        );
        assert_eq!(pair.graph_run().node, phase);
    }
    crate::pr_readiness::set_feed_override(None);
}

#[test]
fn native_pr_draft_waits_for_ready_and_for_a_reviewer() {
    struct ReadyFeed(Mutex<PrSnapshot>);
    impl crate::pr_readiness::PrFeed for ReadyFeed {
        fn viewer_login(&self) -> Result<String, String> {
            Ok("tod-test-user".into())
        }
        fn snapshot(&self, _: &NodePr) -> Result<PrSnapshot, String> {
            Ok(self.0.lock().unwrap().clone())
        }
    }
    let pair = Pair::new(automated_default(), "pr_draft", &[APP_PR]);
    let exercise = |task: &Fixture| {
        restore_pr_gate(task, "pr_draft");
        let feed = Arc::new(ReadyFeed(Mutex::new(PrSnapshot {
            status: PrStatus {
                mergeable: Some(true),
                mergeable_state: Some("blocked".into()),
                behind_by: Some(0),
                checks: Some("success".into()),
                head_sha: Some("abcdef1".into()),
                open: true,
                draft: true,
                ..Default::default()
            },
            threads: vec![],
            comments: vec![],
            reviews: vec![],
        })));
        crate::pr_readiness::set_feed_override(Some(feed.clone()));
        let mut agent = FakeAgent::new(&task.fleet);
        let now = chrono::Utc::now().timestamp_millis();
        require_timed_wait(
            run_babysitter(task, &mut agent),
            "marked ready for review",
            now,
        );
        assert_eq!(pending_specs(task), ["github:pr:42:ready_for_review"]);
        feed.0.lock().unwrap().status.draft = false;
        let now = chrono::Utc::now().timestamp_millis();
        require_timed_wait(run_babysitter(task, &mut agent), "reviewer", now);
        assert_eq!(pending_specs(task), ["github:pr:42:review_requested"]);
        assert_eq!(
            lifecycle::current_state(&task.fleet, task.node).unwrap(),
            "pr_draft"
        );
        feed.0.lock().unwrap().status.reviewers =
            vec!["dependabot[bot]".into(), "tod-test-user".into()];
        let now = chrono::Utc::now().timestamp_millis();
        require_timed_wait(run_babysitter(task, &mut agent), "reviewer", now);
        assert_eq!(pending_specs(task), ["github:pr:42:review_requested"]);
        feed.0.lock().unwrap().status.reviewers = vec!["maria".into()];
        let now = chrono::Utc::now().timestamp_millis();
        require_timed_wait(run_babysitter(task, &mut agent), "a review of", now);
        assert_eq!(
            lifecycle::current_state(&task.fleet, task.node).unwrap(),
            "pr_review"
        );
        assert_eq!(pending_specs(task), ["github:pr:42:review:maria"]);
        agent.turns
    };
    assert_eq!(exercise(&pair.workflow), exercise(&pair.legacy));
    assert_eq!(pair.graph_run().node, "pr_review");
    crate::pr_readiness::set_feed_override(None);
}

#[test]
fn native_pr_review_polls_the_second_pr_before_waiting_for_a_person() {
    let pair = Pair::new(automated_default(), "pr_review", &[APP_PR, INFRA_PR]);
    let exercise = |task: &Fixture| {
        restore_pr_gate(task, "pr_review");
        let feed = Arc::new(SplitPrReadinessFeed {
            reads_of_7: Default::default(),
        });
        crate::pr_readiness::set_feed_override(Some(feed.clone()));
        let mut agent = FakeAgent::new(&task.fleet);
        let now = chrono::Utc::now().timestamp_millis();
        require_timed_wait(
            run_babysitter(task, &mut agent),
            "a review of https://github.com/acme/app/pull/42",
            now,
        );
        let specs = pending_specs(task);
        assert_eq!(specs.len(), 1, "{specs:?}");
        assert!(specs[0].starts_with("github:pr:42:review"));
        assert!(feed.reads_of_7.load(Ordering::SeqCst) >= 3);
        assert_eq!(agent.turns, 0);
        specs
    };
    assert_eq!(exercise(&pair.legacy), exercise(&pair.workflow));
    assert_eq!(pair.graph_run().node, "pr_review");
    crate::pr_readiness::set_feed_override(None);
}

#[test]
fn a_new_pr_head_during_a_review_wait_preserves_the_wait_until_it_conflicts() {
    struct MovingHead {
        current: Mutex<PrSnapshot>,
        last_read: Mutex<Option<String>>,
    }
    impl crate::pr_readiness::PrFeed for MovingHead {
        fn viewer_login(&self) -> Result<String, String> {
            Ok("tod-test-user".into())
        }

        fn snapshot(&self, _: &NodePr) -> Result<PrSnapshot, String> {
            let snapshot = self.current.lock().unwrap().clone();
            *self.last_read.lock().unwrap() = snapshot.status.head_sha.clone();
            Ok(snapshot)
        }
    }

    let pair = Pair::new(automated_default(), "pr_review", &[APP_PR]);
    let exercise = |task: &Fixture| {
        restore_pr_gate(task, "pr_review");
        let pr = NodePr::new("acme", "app", 42);
        let mut initial = crate::pr_readiness::PrFeed::snapshot(&AwaitingReview, &pr).unwrap();
        initial.status.reviewers = vec!["maria".into()];
        let feed = Arc::new(MovingHead {
            current: Mutex::new(initial),
            last_read: Mutex::new(None),
        });
        crate::pr_readiness::set_feed_override(Some(feed.clone()));
        let mut agent = FakeAgent::new(&task.fleet);
        let wait = |agent: &mut FakeAgent| {
            let now = chrono::Utc::now().timestamp_millis();
            require_timed_wait(run_babysitter(task, agent), "a review of", now);
            assert_eq!(pending_specs(task), ["github:pr:42:review:maria"]);
            assert_eq!(
                lifecycle::current_state(&task.fleet, task.node).unwrap(),
                "pr_review"
            );
        };

        wait(&mut agent);
        assert_eq!(agent.turns, 0);
        {
            let mut pr = feed.current.lock().unwrap();
            pr.status.head_sha = Some("def5678".into());
            pr.status.head_committed_at = Some("2020-01-02T00:00:00Z".into());
            pr.status.behind_by = Some(4);
        }
        crate::pr_readiness::forget_snapshot(&pr);
        wait(&mut agent);
        assert_eq!(feed.last_read.lock().unwrap().as_deref(), Some("def5678"));
        assert_eq!(
            agent.turns, 0,
            "falling behind alone leaves the review wait in place"
        );

        {
            let mut pr = feed.current.lock().unwrap();
            pr.status.mergeable = Some(false);
            pr.status.mergeable_state = Some("dirty".into());
        }
        crate::pr_readiness::forget_snapshot(&pr);
        agent.pr_hook = Some(Arc::new(move |_| {
            let mut pr = feed.current.lock().unwrap();
            assert_eq!(pr.status.head_sha.as_deref(), Some("def5678"));
            pr.status.head_sha = Some("fedcba9".into());
            pr.status.head_committed_at = Some("2020-01-03T00:00:00Z".into());
            pr.status.mergeable = Some(true);
            pr.status.mergeable_state = Some("blocked".into());
            pr.status.behind_by = Some(0);
        }));
        wait(&mut agent);
        assert_eq!(agent.turns, 1, "only the conflict needs an agent turn");
        turn_labels(&agent)
    };
    assert_eq!(exercise(&pair.legacy), exercise(&pair.workflow));
    assert_eq!(pair.graph_run().node, "pr_review");
    assert_eq!(pair.graph_run().visit, 1);
    crate::pr_readiness::set_feed_override(None);
}

#[test]
fn native_pr_review_waives_a_failure_also_on_the_base_without_agent_or_duplicate_request() {
    let pair = Pair::new(automated_default(), "pr_review", &[APP_PR]);
    let exercise = |task: &Fixture| {
        let (feed, mut agent, _) = failing_check_on(task, false);
        restore_pr_gate(task, "pr_review");
        feed.fails_as("0123456789abcdef");
        *feed.base_failure.lock().unwrap() = Some(base_proof_for_terraform(BaseProof::SameCheck));
        for _ in 0..2 {
            let now = chrono::Utc::now().timestamp_millis();
            require_timed_wait(run_babysitter(task, &mut agent), "merge", now);
            assert_eq!(agent.turns, 0);
            assert!(pending_requests(task).is_empty());
        }
        let decisions = unrelated_decisions(task);
        assert_eq!(decisions.len(), 1);
        let decision = &decisions[0];
        assert_eq!(decision.status, "answered");
        let answer = task
            .fleet
            .read(|conn| {
                Ok(DecisionRepo::new(conn)
                    .get_with_answers(decision.id)?
                    .unwrap()
                    .answers
                    .remove(0))
            })
            .unwrap();
        assert_eq!(answer.decided_by, tod_store::decisions::DECIDED_BY_TOD);
        assert_eq!(answer.option, Some(1));
        let settings =
            crate::pr_readiness::settings_for_node(&task.fleet, task.node, &task.root).unwrap();
        assert!(crate::check_unrelated::all_waived(
            &settings,
            "acme/app#42",
            "abc1234",
            &["terraform".into()],
            &[]
        ));
        assert!(!crate::check_unrelated::all_waived(
            &settings,
            "acme/app#42",
            "def5678",
            &["terraform".into()],
            &[]
        ));
        assert_eq!(
            lifecycle::current_state(&task.fleet, task.node).unwrap(),
            "approved"
        );
        answer.rationale
    };
    assert_eq!(exercise(&pair.legacy), exercise(&pair.workflow));
    assert_eq!(pair.graph_run().node, "approved");
    crate::pr_readiness::set_feed_override(None);
}

fn adopt_default(task: &Fixture) -> Run {
    let catalog = Catalog::new(&task.fleet);
    let draft = catalog
        .create("Automated Default", automated_default())
        .unwrap();
    let published = catalog
        .publish(&draft.definition.revision.id, draft.generation)
        .unwrap();
    adopt_revision(task, &published.definition.revision)
}

fn adopt_revision(task: &Fixture, revision: &crate::lifecycle::workflow::Revision) -> Run {
    use crate::lifecycle::workflow::migration;
    let preview = migration::preview_adoption(&task.fleet, task.node, revision).unwrap();
    migration::adopt(&task.fleet, &preview, 100).unwrap()
}

#[test]
fn an_open_draft_is_recovered_by_branch_and_certified_again_in_a_new_stay() {
    use std::sync::atomic::AtomicUsize;
    use tod_store::github::{PullState, PullSummary};
    struct ExistingDraft(AtomicUsize);
    impl crate::pr_readiness::PrFeed for ExistingDraft {
        fn viewer_login(&self) -> Result<String, String> {
            Ok("tod-test-user".into())
        }
        fn snapshot(&self, pr: &NodePr) -> Result<PrSnapshot, String> {
            assert_eq!(
                (pr.owner.as_str(), pr.repo.as_str(), pr.pr_number),
                ("acme", "app", 42)
            );
            Ok(PrSnapshot {
                status: PrStatus {
                    mergeable: Some(true),
                    mergeable_state: Some("draft".into()),
                    behind_by: Some(0),
                    checks: Some("success".into()),
                    head_sha: Some("abcdef1".into()),
                    draft: true,
                    open: true,
                    ..Default::default()
                },
                threads: vec![],
                comments: vec![],
                reviews: vec![],
            })
        }
        fn branch_prs(
            &self,
            owner: &str,
            repo: &str,
            branch: &str,
        ) -> Result<Vec<PullSummary>, String> {
            assert_eq!((owner, repo, branch), ("acme", "app", "task/x"));
            self.0.fetch_add(1, Ordering::SeqCst);
            let pull = |number, state| PullSummary {
                number,
                title: "Existing task change".into(),
                url: format!("https://github.com/acme/app/pull/{number}"),
                state,
                author: None,
                head: branch.into(),
                base: "main".into(),
                updated_at: "2026-11-21T10:00:00Z".into(),
            };
            Ok(vec![pull(3, PullState::Closed), pull(42, PullState::Draft)])
        }
    }
    let exercise = |workflow: bool| {
        let task = configured_task();
        retire_outside_criteria(&task);
        linked_to_prs(&task, "pr", &[]);
        task.fleet
            .enqueue(tod_store::fleet::FleetMutation::UpdateTaskBranch {
                id: task.node.to_string(),
                branch: Some("task/x".into()),
            })
            .unwrap();
        task.fleet.writer().flush().unwrap();
        {
            let conn = rusqlite::Connection::open(task.fleet.paths().db()).unwrap();
            tod_store::cloud_nodes::upsert(&conn, task.node, "tod-x-abc123", "alice", 1).unwrap();
            tod_store::cloud_nodes::set_ownership(
                &conn,
                task.node,
                &tod_store::cloud_nodes::Ownership {
                    workspace: None,
                    owner_client: None,
                    repo: Some("acme/app".into()),
                },
            )
            .unwrap();
        }
        task.fleet.reload_if_stale().unwrap();
        restore_pr_gate(&task, "pr");
        let feed = Arc::new(ExistingDraft(AtomicUsize::new(0)));
        crate::pr_readiness::set_feed_override(Some(feed.clone()));
        let mut run = workflow.then(|| adopt_default(&task));
        let mut agent = FakeAgent::new(&task.fleet);
        let read_links = || {
            task.fleet
                .read(|conn| tod_store::github::NodePrRepo::new(conn).read(task.node))
                .unwrap()
        };
        let certificates = || {
            task.fleet
                .read(|conn| PhaseRepo::new(conn).list_for_node(task.node))
                .unwrap()
                .into_iter()
                .filter(|event| {
                    event.state == "pr" && event.kind == tod_store::phase::PHASE_CERTIFY
                })
                .collect::<Vec<_>>()
        };
        assert!(read_links().prs.is_empty());
        for stay in 1..=2 {
            let turns_before = agent.turns;
            assert_eq!(
                task.fleet
                    .read(|conn| PhaseRepo::new(conn).certificate_status(task.node, "pr"))
                    .unwrap(),
                tod_store::phase::CertificateStatus::None
            );
            let now = chrono::Utc::now().timestamp_millis();
            require_timed_wait(
                run_babysitter(&task, &mut agent),
                "marked ready for review",
                now,
            );
            assert_eq!(
                lifecycle::current_state(&task.fleet, task.node).unwrap(),
                "pr_draft"
            );
            assert_eq!(pending_specs(&task), ["github:pr:42:ready_for_review"]);
            assert_eq!(
                read_links()
                    .prs
                    .iter()
                    .map(|pr| pr.url.as_str())
                    .collect::<Vec<_>>(),
                [APP_PR]
            );
            assert_eq!(certificates().len(), stay);
            assert!(
                agent.turns > turns_before,
                "the task certifies its draft in each stay"
            );
            if stay == 1 {
                if let Some(prior) = run.take() {
                    use crate::lifecycle::workflow::migration;
                    let detach = migration::preview_detach(&task.fleet, prior.id).unwrap();
                    migration::detach(&task.fleet, &detach).unwrap();
                    lifecycle::set_lifecycle_by_user(&task.fleet, task.node, "pr").unwrap();
                    run = Some(adopt_revision(&task, &prior.revision));
                } else {
                    lifecycle::set_lifecycle_by_user(&task.fleet, task.node, "pr").unwrap();
                }
            }
        }
        assert_eq!(feed.0.load(Ordering::SeqCst), 1);
        let notes = task
            .fleet
            .read(|conn| tod_store::node_timeline::TimelineRepo::new(conn).list(task.node))
            .unwrap();
        assert_eq!(
            notes
                .iter()
                .filter(|entry| entry.title == format!("Linked {APP_PR} from GitHub"))
                .count(),
            1
        );
        agent.turns
    };
    assert_eq!(exercise(false), exercise(true));
    crate::pr_readiness::set_feed_override(None);
}

#[test]
fn adopting_merged_without_a_pr_runs_the_same_release_phase() {
    let exercise = |workflow: bool| {
        let task = bare();
        retire_outside_criteria(&task);
        lifecycle::set_lifecycle(&task.fleet, task.node, "merged").unwrap();
        let run = workflow.then(|| adopt_default(&task));
        let mut agent = FakeAgent::new(&task.fleet);
        assert_eq!(run_babysitter(&task, &mut agent), Outcome::Done);
        assert_eq!(
            lifecycle::current_state(&task.fleet, task.node).unwrap(),
            "done"
        );
        assert!(pending_specs(&task).is_empty());
        if let Some(run) = run {
            assert_eq!(run.node, "release_evidence");
            assert_eq!(
                Runner::new(&task.fleet)
                    .get(run.id)
                    .unwrap()
                    .unwrap()
                    .status,
                RunStatus::Complete
            );
        }
        let history = task
            .fleet
            .read(|conn| PhaseRepo::new(conn).list_for_node(task.node))
            .unwrap();
        let phases = history
            .into_iter()
            .filter(|event| event.kind == tod_store::phase::PHASE_CERTIFY)
            .map(|event| (event.state, event.certifier))
            .collect::<BTreeSet<_>>();
        assert!(phases.iter().any(|(phase, _)| phase == "merged"));
        (agent.turns, phases)
    };
    assert_eq!(exercise(false), exercise(true));
    crate::pr_readiness::set_feed_override(None);
}

#[test]
fn approved_requires_a_link_and_explains_the_same_gate_failure() {
    let mut pair = Pair::new(automated_default(), "approved", &[]);
    for task in [&pair.legacy, &pair.workflow] {
        restore_pr_gate(task, "approved");
    }
    let legacy = pair.run_legacy();
    assert!(
        matches!(&legacy, Outcome::NeedsHuman { reason: NeedsHuman::FailingCriteria { criteria } }
        if criteria.iter().any(|item| item.contains("No pull request is linked")))
    );
    assert_eq!(pair.run_workflow(), legacy);
    assert_eq!(pair.legacy_phase(), "approved");
    assert_eq!(pair.graph_run().status, RunStatus::Waiting);
    assert!(pair.scheduled_graph_wait().is_none());
    assert_eq!(pair.legacy_agent.turns + pair.workflow_agent.turns, 0);
}

#[test]
fn malformed_links_preserve_merged_certification_and_cannot_certify_approval() {
    const INVALID: &str = "missing-repo/pull/not-a-number";
    let feed = AfterReview::new();
    feed.merged.store(true, Ordering::SeqCst);
    for workflow in [false, true] {
        let task = bare();
        retire_outside_criteria(&task);
        linked_to_prs(&task, "merged", &[APP_PR]);
        crate::pr_readiness::set_feed_override(Some(feed.clone()));
        if workflow {
            adopt_default(&task);
        }
        let now = chrono::Utc::now().timestamp_millis();
        let mut agent = FakeAgent::new(&task.fleet);
        require_timed_wait(run_babysitter(&task, &mut agent), "release", now);
        assert_eq!(agent.turns, 0);
    }
    for links in [&[INVALID][..], &[APP_PR, INVALID][..]] {
        let exercise = |phase: &str, workflow: bool| {
            let task = bare();
            retire_outside_criteria(&task);
            linked_to_prs(&task, phase, links);
            crate::pr_readiness::set_feed_override(Some(feed.clone()));
            if phase == "approved" {
                restore_pr_gate(&task, phase);
            }
            let run = workflow.then(|| adopt_default(&task));
            let mut agent = FakeAgent::new(&task.fleet);
            let outcome = run_babysitter(&task, &mut agent);
            let state = lifecycle::current_state(&task.fleet, task.node).unwrap();
            if phase == "approved" {
                assert_eq!(state, "approved");
                assert_eq!(agent.turns, 0);
                assert!(
                    matches!(&outcome, Outcome::NeedsHuman { reason: NeedsHuman::FailingCriteria { criteria } } if criteria.iter().any(|item| item.contains(INVALID)))
                );
                if let Some(run) = run {
                    let runner = Runner::new(&task.fleet);
                    assert_eq!(
                        runner.get(run.id).unwrap().unwrap().status,
                        RunStatus::Error
                    );
                    let trace = runner.traces(run.id).unwrap().pop().unwrap();
                    assert!(
                        matches!(&trace.trace.observations["all_linked_prs_merged"], Observation::Error { reason } if reason.contains("could not be read as GitHub URLs"))
                    );
                }
            } else {
                assert_eq!(outcome, Outcome::Done);
                assert_eq!(state, "done");
                assert!(agent.turns > 0);
                let events = task
                    .fleet
                    .read(|conn| PhaseRepo::new(conn).list_for_node(task.node))
                    .unwrap();
                assert!(events.iter().any(|event| event.state == "merged"
                    && event.kind == tod_store::phase::PHASE_CERTIFY));
                if let Some(run) = run {
                    let runner = Runner::new(&task.fleet);
                    assert_eq!(
                        runner.get(run.id).unwrap().unwrap().status,
                        RunStatus::Complete
                    );
                    let traces = runner.traces(run.id).unwrap();
                    let release = traces
                        .iter()
                        .find(|record| record.trace.observations.contains_key("has_valid_pr_links"))
                        .unwrap();
                    assert!(
                        matches!(&release.trace.observations["has_valid_pr_links"], Observation::Ready { value: Value::Boolean(false), evidence } if evidence.contains("could not be read as GitHub URLs"))
                    );
                }
            }
            (outcome, agent.turns)
        };
        assert_eq!(exercise("approved", false), exercise("approved", true));
        assert_eq!(exercise("merged", false), exercise("merged", true));
    }
    crate::pr_readiness::set_feed_override(None);
}

#[test]
fn a_merge_only_repository_does_not_hold_the_release_on_either_path() {
    let exercise = |workflow: bool| {
        let task = bare();
        retire_outside_criteria(&task);
        linked_to_prs(&task, "merged", &[APP_PR, INFRA_PR]);
        let mut settings = tod_store::TodSettings::default();
        settings.lifecycle.repo_profiles = vec![tod_store::RepoProfileSetting {
            repo: "acme/infra".into(),
            profile: tod_store::RepoProfile::Merge,
        }];
        settings.save(&tod_store::TodPaths::at(&task.root)).unwrap();
        let feed = AfterReview::new();
        feed.merged.store(true, Ordering::SeqCst);
        crate::pr_readiness::set_feed_override(Some(feed.clone()));
        let run = workflow.then(|| adopt_default(&task));
        let mut agent = FakeAgent::new(&task.fleet);
        let now = chrono::Utc::now().timestamp_millis();
        let outcome = run_babysitter(&task, &mut agent);
        let Outcome::Waiting { what, due_at_ms } = outcome else {
            panic!("expected release wait, got {outcome:?}");
        };
        assert!(due_at_ms > now);
        assert!(what.contains("acme/app#42 merged, no release names it yet"));
        assert!(what.contains("acme/infra#7 merged (this repository does not release)"));
        assert_eq!(agent.turns, 0);
        *feed.notes.lock().unwrap() = Some("* Add the widget by @ann in #42".into());
        assert_eq!(run_babysitter(&task, &mut agent), Outcome::Done);
        assert_eq!(
            lifecycle::current_state(&task.fleet, task.node).unwrap(),
            "done"
        );
        if let Some(run) = run {
            let trace = Runner::new(&task.fleet).traces(run.id).unwrap();
            assert!(trace.iter().any(|record| matches!(record.trace.observations.get("all_linked_prs_released"),
                Some(Observation::Ready { value: Value::Boolean(true), evidence }) if evidence.contains("/releases/tag/v1.0.0") && evidence.contains(INFRA_PR))));
        }
        (agent.turns, what)
    };
    assert_eq!(exercise(false), exercise(true));
    crate::pr_readiness::set_feed_override(None);
}
