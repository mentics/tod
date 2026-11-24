use super::*;
use tod_store::phase::{CertificateStatus, PHASE_CERTIFY, PhaseRepo};

struct StopIn(&'static str, Uuid);

impl StepHook for StopIn {
    fn at(&mut self, fleet: &FleetStore, boundary: Boundary) -> Result<Option<String>> {
        Ok(
            (boundary == Boundary::Step && lifecycle::current_state(fleet, self.1)? == self.0)
                .then(|| format!("Fixture stopped in {}", self.0)),
        )
    }
}

fn advance_until(pair: &mut Pair, phase: &'static str) {
    let mut outcomes = Vec::new();
    for (task, agent) in [
        (&pair.legacy, &mut pair.legacy_agent),
        (&pair.workflow, &mut pair.workflow_agent),
    ] {
        let result = autopilot(task, Budget::default())
            .run_with(&task.fleet, agent, &mut StopIn(phase, task.node))
            .unwrap();
        assert_eq!(
            lifecycle::current_state(&task.fleet, task.node).unwrap(),
            phase
        );
        outcomes.push(result);
    }
    assert_eq!(outcomes[0], outcomes[1]);
    assert_eq!(
        outcomes[0],
        Outcome::Stopped {
            reason: format!("Fixture stopped in {phase}")
        }
    );
}

fn design_certificates(task: &Fixture) -> Vec<tod_store::phase::PhaseEvent> {
    task.fleet
        .read(|conn| PhaseRepo::new(conn).list_for_node(task.node))
        .unwrap()
        .into_iter()
        .filter(|event| event.state == "design" && event.kind == PHASE_CERTIFY)
        .collect()
}

fn set_design(task: &Fixture, body: &str) {
    task.outline(OutlineMutation::SetExtraContent {
        node_id: task.node,
        content_type: "design".into(),
        body: body.into(),
    });
    task.fleet.writer().flush().unwrap();
}

#[test]
fn edited_design_invalidates_the_same_stay_certificate_on_both_paths() {
    let mut pair = Pair::with_tasks(automated_default(), "design", &[], configured_task);
    for task in [&pair.legacy, &pair.workflow] {
        set_design(task, "Use a table for the initial design.");
        let obligation = task
            .fleet
            .read(|conn| {
                Ok(tod_store::outline::repos::ObligationRepo::new(conn)
                    .list_for_node(task.node)?
                    .remove(0))
            })
            .unwrap();
        let mockup = task.root.join("design-mockup.txt");
        std::fs::write(&mockup, "Initial mockup").unwrap();
        task.outline(OutlineMutation::UpdateObligationVisualDesign {
            obligation_id: obligation.id,
            path: Some(mockup.display().to_string()),
        });
        task.fleet.writer().flush().unwrap();
        task.fleet
            .interview(
                ACTOR_USER,
                InterviewCommand::PhaseCertify {
                    node_id: task.node,
                    state: "design".into(),
                    note: "Initial design accepted.".into(),
                },
            )
            .unwrap();
        let status = || {
            task.fleet
                .read(|conn| PhaseRepo::new(conn).certificate_status(task.node, "design"))
                .unwrap()
        };
        let set_obligation = |body: &str| {
            task.outline(OutlineMutation::UpdateObligationBody {
                obligation_id: obligation.id,
                body: body.into(),
            });
            task.fleet.writer().flush().unwrap();
        };
        set_obligation("Reword the requirement.");
        assert!(matches!(status(), CertificateStatus::Stale { changed, .. }
            if changed.iter().any(|cause| cause.contains("obligation"))));
        set_obligation(&obligation.body);
        assert!(matches!(status(), CertificateStatus::Current(_)));

        std::fs::write(&mockup, "Changed mockup").unwrap();
        assert!(matches!(status(), CertificateStatus::Stale { changed, .. }
            if changed.iter().any(|cause| cause.contains("mockup"))));
        std::fs::write(&mockup, "Initial mockup").unwrap();
        assert!(matches!(status(), CertificateStatus::Current(_)));

        set_design(task, "Use a list for the revised design.");
        assert!(matches!(status(), CertificateStatus::Stale { changed, .. }
            if changed.iter().any(|cause| cause.contains("design"))));
    }

    advance_until(&mut pair, "planning");
    assert!(pair.legacy_agent.turns > 0);
    assert_eq!(pair.legacy_agent.turns, pair.workflow_agent.turns);
    for task in [&pair.legacy, &pair.workflow] {
        let certificates = design_certificates(task);
        assert_eq!(certificates.len(), 2);
        assert_ne!(certificates[0].digest, certificates[1].digest);
        assert_eq!(
            certificates[1].certifier,
            tod_store::phase::CERTIFIER_INDEPENDENT
        );
    }
}

#[test]
fn confirming_a_move_back_reenters_design_and_restores_unchanged_evidence() {
    let mut pair = Pair::with_tasks(automated_default(), "design", &[], configured_task);
    advance_until(&mut pair, "planning");
    let stale = pair.graph_run();
    assert_eq!(stale.node, "planning");
    let completed = Runner::new(&pair.workflow.fleet).visits(stale.id).unwrap();
    let turns = [pair.legacy_agent.turns, pair.workflow_agent.turns];
    for task in [&pair.legacy, &pair.workflow] {
        lifecycle::set_lifecycle_by_user_because(
            &task.fleet,
            task.node,
            "design",
            Some("Reconsider the design."),
        )
        .unwrap();
    }
    let resumed = pair.graph_run();
    assert_eq!(resumed.node, "design");
    assert!(resumed.visit > stale.visit && resumed.generation > stale.generation);
    let runner = Runner::new(&pair.workflow.fleet);
    let history = runner.visits(resumed.id).unwrap();
    assert_eq!(history[0].output, completed[0].output);
    assert!(runner.dispatch(&stale).is_err());
    let traces = runner.traces(resumed.id).unwrap();
    assert!(traces.iter().any(|entry| matches!(&entry.trace.decision,
        crate::lifecycle::workflow::Decision::OperatorMove { to, reason }
            if to == "design" && reason.as_deref() == Some("Reconsider the design.")
    )));

    advance_until(&mut pair, "planning");
    assert_eq!([pair.legacy_agent.turns, pair.workflow_agent.turns], turns);
    for task in [&pair.legacy, &pair.workflow] {
        let certificates = design_certificates(task);
        assert_eq!(certificates.len(), 2);
        assert_eq!(certificates[0].digest, certificates[1].digest);
        assert!(certificates[1].body.starts_with("Restored:"));
    }
}

#[test]
fn an_open_intent_request_resumes_after_adoption_and_allows_rollback() {
    use crate::lifecycle::workflow::migration::{adopt, detach, preview_adoption, preview_detach};
    use tod_store::{decisions::DecisionRepo, outline::repos::ObligationRepo};

    let mut pair = Pair::new(automated_default(), "proposed", &[]);
    let revision = pair.graph_run().revision;
    let fleet = &pair.workflow.fleet;
    detach(fleet, &preview_detach(fleet, pair.run_id).unwrap()).unwrap();
    let pending = pair.run_legacy();
    assert_eq!(
        pending,
        Outcome::NeedsHuman {
            reason: NeedsHuman::Decision { pending: 1 }
        }
    );
    assert_eq!(pair.run_workflow(), pending);

    let fleet = &pair.workflow.fleet;
    let preview = preview_adoption(fleet, pair.workflow.node, &revision).unwrap();
    pair.run_id = adopt(fleet, &preview, 100).unwrap().id;
    for task in [&pair.legacy, &pair.workflow] {
        let [question] = task
            .fleet
            .read(|conn| DecisionRepo::new(conn).list_open_for_node(task.node))
            .unwrap()
            .try_into()
            .unwrap();
        task.user(InterviewCommand::AnswerDecision {
            decision_id: question.id,
            option: None,
            text: Some("Show the weather.".into()),
        });
    }

    advance_until(&mut pair, "design");
    for task in [&pair.legacy, &pair.workflow] {
        assert!(
            task.fleet
                .read(|conn| ObligationRepo::new(conn).list_for_node(task.node))
                .unwrap()
                .iter()
                .any(|obligation| obligation.body == "Show the weather.")
        );
    }
    let fleet = &pair.workflow.fleet;
    detach(fleet, &preview_detach(fleet, pair.run_id).unwrap()).unwrap();
    advance_until(&mut pair, "planning");
}

#[test]
fn reopening_a_completed_task_retains_the_completed_run_and_resumes_work() {
    let mut pair = Pair::with_tasks(automated_default(), "design", &[], configured_task);
    for task in [&pair.legacy, &pair.workflow] {
        lifecycle::set_lifecycle_by_user_because(
            &task.fleet,
            task.node,
            "done",
            Some("Mark the task complete."),
        )
        .unwrap();
    }
    assert_eq!(pair.run_legacy(), Outcome::Done);
    assert_eq!(pair.run_workflow(), Outcome::Done);
    let completed = pair.graph_run();

    for task in [&pair.legacy, &pair.workflow] {
        lifecycle::set_lifecycle_by_user_because(
            &task.fleet,
            task.node,
            "design",
            Some("Reopen the design."),
        )
        .unwrap();
    }
    let runner = Runner::new(&pair.workflow.fleet);
    let reopened = runner.current(pair.workflow.node).unwrap().unwrap();
    assert_ne!(reopened.id, completed.id);
    assert_eq!(reopened.revision, completed.revision);
    assert_eq!(reopened.node, "design");
    assert_eq!(runner.get(completed.id).unwrap().unwrap(), completed);
    advance_until(&mut pair, "planning");
}

#[test]
fn a_worktree_lock_conflict_schedules_the_same_retry_on_both_paths() {
    struct BusyFiles;
    impl StepHook for BusyFiles {
        fn at(&mut self, _: &FleetStore, _: Boundary) -> Result<Option<String>> {
            Err(tod_store::fleet::worktree::WorktreeLockBusy {
                repo: "/fixtures/worktree".into(),
                holder: "another task is preparing the worktree".into(),
                waited: Duration::from_secs(1),
            }
            .into())
        }
    }
    let mut pair = Pair::with_tasks(automated_default(), "design", &[], configured_task);
    let mut reasons = Vec::new();
    for (task, agent) in [
        (&pair.legacy, &mut pair.legacy_agent),
        (&pair.workflow, &mut pair.workflow_agent),
    ] {
        let started = state::now_ms();
        let result = autopilot(task, Budget::default())
            .run_with(&task.fleet, agent, &mut BusyFiles)
            .unwrap();
        let Outcome::Waiting { what, due_at_ms } = result else {
            panic!("Expected a timed worktree wait, got {result:?}");
        };
        assert!(due_at_ms >= started + 60_000, "{due_at_ms}");
        let waits = task
            .fleet
            .read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(task.node))
            .unwrap();
        assert!(waits.iter().any(|wait| wait.due_at == due_at_ms));
        assert_eq!(agent.turns, 0);
        reasons.push(what);
    }
    assert_eq!(reasons[0], reasons[1]);
}

#[test]
fn changed_plan_coverage_is_recertified_before_both_paths_record_a_baseline() {
    use tod_store::outline::repos::{ObligationRepo, PlanStepRepo};
    let mut pair = Pair::with_tasks(automated_default(), "planning", &[], configured_task);
    for task in [&pair.legacy, &pair.workflow] {
        let (obligation, step) = task
            .fleet
            .read(|conn| {
                Ok((
                    ObligationRepo::new(conn)
                        .list_for_node(task.node)?
                        .remove(0),
                    PlanStepRepo::new(conn).list_for_node(task.node)?.remove(0),
                ))
            })
            .unwrap();
        task.outline(OutlineMutation::LinkPlanStepObligation {
            step_id: step.id,
            obligation_id: obligation.id,
        });
        task.fleet.writer().flush().unwrap();
        task.user(InterviewCommand::PhaseCertify {
            node_id: task.node,
            state: "planning".into(),
            note: "The plan links to the requirement.".into(),
        });
        task.outline(OutlineMutation::UnlinkPlanStepObligation {
            step_id: step.id,
            obligation_id: obligation.id,
        });
        task.fleet.writer().flush().unwrap();
        let status = task
            .fleet
            .read(|conn| PhaseRepo::new(conn).certificate_status(task.node, "planning"))
            .unwrap();
        assert!(matches!(status, CertificateStatus::Stale { changed, .. }
            if changed.iter().any(|cause| cause.contains("satisfies"))));
    }
    advance_until(&mut pair, "ready");
    assert!(pair.legacy_agent.turns > 0);
    assert_eq!(pair.legacy_agent.turns, pair.workflow_agent.turns);
    let snapshot = |task: &Fixture| {
        let baseline = task
            .fleet
            .read(|conn| tod_store::lifecycle_baseline::BaselineRepo::new(conn).get(task.node))
            .unwrap()
            .expect("a baseline captured on entry to Ready");
        (
            baseline
                .obligations
                .into_iter()
                .map(|o| (o.kind, o.body, o.acts_in))
                .collect::<Vec<_>>(),
            baseline
                .plan_steps
                .into_iter()
                .map(|s| (s.body, s.phase))
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(snapshot(&pair.legacy), snapshot(&pair.workflow));
}

#[test]
fn both_paths_repair_a_configured_check_before_moving_to_verification() {
    let mut pair = Pair::with_tasks(automated_default(), "active", &[], configured_task);
    for (task, agent) in [
        (&pair.legacy, &mut pair.legacy_agent),
        (&pair.workflow, &mut pair.workflow_agent),
    ] {
        let workspace = with_check(task, "implement", "ok file", OK_FILE_CHECK);
        agent.fix_writes = Some((workspace.join("ok.txt"), "fixed".into()));
    }
    advance_until(&mut pair, "verifying");
    for task in [&pair.legacy, &pair.workflow] {
        let latest = task
            .fleet
            .read(|conn| {
                tod_store::check_runs::CheckRunRepo::new(conn).latest(
                    task.node,
                    "implement",
                    "",
                    OK_FILE_CHECK,
                )
            })
            .unwrap()
            .expect("the configured check ran");
        assert_eq!(latest.verdict, tod_store::check_runs::Verdict::Passed);
        let fixes = openings(task, ProtocolKind::Fix);
        assert_eq!(fixes.len(), 1);
        assert!(fixes[0].1.contains("Check `ok file` fails"));
    }
}

#[test]
fn a_review_check_can_quick_fix_the_checkout_before_the_reviewers_open() {
    let mut pair = Pair::with_tasks(automated_default(), "capture_review", &[], configured_task);
    for (task, agent) in [
        (&pair.legacy, &mut pair.legacy_agent),
        (&pair.workflow, &mut pair.workflow_agent),
    ] {
        let workspace = with_check(task, "review", "ok file", OK_FILE_CHECK);
        agent.quick_fix_writes = Some((workspace.join("ok.txt"), "fixed".into()));
    }
    advance_until(&mut pair, "pr");
    for task in [&pair.legacy, &pair.workflow] {
        let fixes = quick_fixes_of(task);
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].outcome, tod_store::quick_fixes::OUTCOME_SUCCEEDED);
    }
    assert_eq!(
        openings(&pair.legacy, ProtocolKind::Review).len(),
        openings(&pair.workflow, ProtocolKind::Review).len()
    );
    let capture = Runner::new(&pair.workflow.fleet)
        .visits(pair.run_id)
        .unwrap()
        .remove(0)
        .output
        .expect("the first review input was captured");
    let repaired_head = quick_fixes_of(&pair.workflow).remove(0).head;
    assert_eq!(capture.values["revision"], Value::Text(repaired_head));
}
