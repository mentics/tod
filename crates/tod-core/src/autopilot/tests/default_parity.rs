//! Compare observable outcomes at the same seeded boundary. Each side gets
//! its own task and the same deterministic external observations.

mod certificates;
mod pr_bots;

use super::*;
use crate::lifecycle::workflow::{
    Definition, Observation, Source, Value,
    builtin::{MANUAL, automated_default, default_template},
    catalog::Catalog,
    execute,
    improvement::captured_context,
    retry::GithubOperation,
    runner::{Run, RunStatus, Runner},
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;

struct Pair {
    legacy: Fixture,
    workflow: Fixture,
    legacy_agent: FakeAgent,
    workflow_agent: FakeAgent,
    run_id: Uuid,
}

impl Pair {
    fn new(definition: Definition, phase: &str, prs: &[&str]) -> Self {
        Self::with_tasks(definition, phase, prs, bare)
    }

    fn with_tasks(
        definition: Definition,
        phase: &str,
        prs: &[&str],
        task: fn() -> Fixture,
    ) -> Self {
        let legacy = task();
        let workflow = task();
        let task_phase = definition
            .task_phases
            .get(phase)
            .map_or(phase, String::as_str);
        for task in [&legacy, &workflow] {
            retire_outside_criteria(task);
            if prs.is_empty() {
                lifecycle::set_lifecycle(&task.fleet, task.node, task_phase).unwrap();
            } else {
                linked_to_prs(task, task_phase, prs);
            }
        }
        let catalog = Catalog::new(&workflow.fleet);
        let draft = catalog
            .create("Default parity fixture", starting_at(definition, phase))
            .unwrap();
        let published = catalog
            .publish(&draft.definition.revision.id, draft.generation)
            .unwrap();
        let run_id = Runner::new(&workflow.fleet)
            .assign(workflow.node, &published.definition.revision, 100)
            .unwrap()
            .id;
        let legacy_agent = FakeAgent::new(&legacy.fleet);
        let workflow_agent = FakeAgent::new(&workflow.fleet);
        Self {
            legacy,
            workflow,
            legacy_agent,
            workflow_agent,
            run_id,
        }
    }

    fn run_legacy(&mut self) -> Outcome {
        autopilot(&self.legacy, Budget::default())
            .run(&self.legacy.fleet, &mut self.legacy_agent)
            .unwrap()
    }

    fn run_workflow(&mut self) -> Outcome {
        autopilot(&self.workflow, Budget::default())
            .run(&self.workflow.fleet, &mut self.workflow_agent)
            .unwrap()
    }

    fn graph_run(&self) -> Run {
        Runner::new(&self.workflow.fleet)
            .get(self.run_id)
            .unwrap()
            .unwrap()
    }

    fn legacy_phase(&self) -> String {
        lifecycle::current_state(&self.legacy.fleet, self.legacy.node).unwrap()
    }

    fn scheduled_graph_wait(&self) -> Option<tod_store::workflow_run::ScheduledWait> {
        self.workflow
            .fleet
            .read(|conn| tod_store::workflow_run::scheduled_wait(conn, self.workflow.node))
            .unwrap()
    }

    fn decision_reasons(task: &Fixture) -> Vec<String> {
        task.fleet
            .read(|conn| {
                Ok(tod_store::decisions::DecisionRepo::new(conn).list_open_for_node(task.node)?)
            })
            .unwrap()
            .into_iter()
            .map(|question| question.reason)
            .collect()
    }
}

/// Starting after Proposed uses the reachable part of the published Default
/// graph so both engines enter at the same point. The retained nodes, actions,
/// routes, and provider bindings keep their production definitions.
fn starting_at(mut definition: Definition, phase: &str) -> Definition {
    let mut reachable = BTreeSet::new();
    let mut pending = vec![phase];
    while let Some(node) = pending.pop() {
        if reachable.insert(node) {
            pending.extend(
                definition.nodes[node]
                    .routes
                    .iter()
                    .map(|route| route.to.as_str()),
            );
        }
    }
    let reachable: BTreeSet<String> = reachable.into_iter().map(str::to_owned).collect();
    definition.entry = phase.into();
    definition.nodes.retain(|name, _| reachable.contains(name));
    definition
        .task_phases
        .retain(|name, _| reachable.contains(name));
    definition
        .adoption_points
        .retain(|_, name| reachable.contains(name));
    definition.fields.retain(|_, field| match &field.source {
        Source::NodeOutput { node, .. } => reachable.contains(node),
        Source::Provider { .. } => true,
    });
    definition
}

fn require_timed_wait(outcome: Outcome, label: &str, now_ms: i64) {
    assert!(
        matches!(&outcome, Outcome::Waiting { what, due_at_ms }
            if what.contains(label) && *due_at_ms > now_ms),
        "expected scheduled {label} wait, got {outcome:?}"
    );
}

#[test]
fn missing_proposal_intent_exposes_the_operator_default_gap() {
    let mut pair = Pair::new(default_template(), "proposed", &[]);

    assert_eq!(
        pair.run_legacy(),
        Outcome::NeedsHuman {
            reason: NeedsHuman::Decision { pending: 1 }
        }
    );
    assert_eq!(pair.legacy_phase(), "proposed");
    assert_eq!(
        Pair::decision_reasons(&pair.legacy),
        [tod_store::decisions::REASON_INTENT]
    );
    assert!(pair.legacy_agent.turns > 0);

    assert!(
        matches!(pair.run_workflow(), Outcome::Stopped { reason } if reason.contains("needs an action result"))
    );
    let run = pair.graph_run();
    assert_eq!(run.node, "proposed");
    let visit = Runner::new(&pair.workflow.fleet)
        .current_visit(&run)
        .unwrap();
    assert_eq!(visit.dispatch.unwrap().action.id, MANUAL);
    assert!(visit.output.is_none());
    assert!(Pair::decision_reasons(&pair.workflow).is_empty());
    assert_eq!(pair.workflow_agent.turns, 0);
}

#[test]
fn merge_and_release_gates_respond_to_the_same_github_evidence() {
    let mut pair = Pair::new(
        default_template(),
        "approved",
        &["https://github.com/acme/app/pull/42"],
    );
    let github = AfterReview::new();
    crate::pr_readiness::set_feed_override(Some(github.clone()));

    for _ in 0..2 {
        let now = chrono::Utc::now().timestamp_millis();
        require_timed_wait(pair.run_legacy(), "merge", now);
        require_timed_wait(pair.run_workflow(), "merge", now);
        assert_eq!(pair.legacy_phase(), "approved");
        assert_eq!(pair.graph_run().node, "approved");
        assert_eq!(pair.graph_run().status, RunStatus::Waiting);
        assert_eq!(pending_specs(&pair.legacy), ["github:pr:42:merged"]);
    }
    assert_eq!(pair.legacy_agent.turns, 0);
    assert_eq!(pair.workflow_agent.turns, 0);

    github.merged.store(true, Ordering::SeqCst);
    let now = chrono::Utc::now().timestamp_millis();
    require_timed_wait(pair.run_legacy(), "release", now);
    assert_eq!(pair.legacy_phase(), "merged");
    assert!(
        matches!(pair.run_workflow(), Outcome::Stopped { reason } if reason.contains("needs an action result"))
    );
    let run = pair.graph_run();
    assert_eq!(run.node, "merged");
    let visit = Runner::new(&pair.workflow.fleet)
        .current_visit(&run)
        .unwrap();
    assert!(visit.output.is_none());
    let request = visit.dispatch.unwrap();
    assert_eq!(request.action.id, MANUAL);
    assert_eq!(pair.workflow_agent.turns, 0);

    // The operator's acknowledgement is an explicit fixture input. The
    // assertions above capture the unassisted execution gap. Everything
    // below compares release evidence after that boundary is satisfied.
    execute::record_manual(&pair.workflow.fleet, &request, BTreeMap::new(), Vec::new()).unwrap();
    *github.notes.lock().unwrap() = Some("* Unrelated change in #7".into());
    let now = chrono::Utc::now().timestamp_millis();
    require_timed_wait(pair.run_legacy(), "release", now);
    require_timed_wait(pair.run_workflow(), "release", now);
    assert_eq!(pair.legacy_phase(), "merged");
    assert_eq!(pair.graph_run().node, "merged");
    assert_eq!(pending_specs(&pair.legacy), ["github:release:published"]);
    assert_eq!(pair.legacy_agent.turns, 0);
    assert_eq!(pair.workflow_agent.turns, 0);

    *github.notes.lock().unwrap() = Some("* Add the widget by @ann in #42".into());
    assert_eq!(pair.run_legacy(), Outcome::Done);
    assert_eq!(pair.legacy_phase(), "done");
    assert!(pair.legacy_agent.turns > 0);
    assert!(pending_specs(&pair.legacy).is_empty());
    assert!(
        matches!(pair.run_workflow(), Outcome::Stopped { reason } if reason.contains("needs an action result"))
    );
    assert_eq!(pair.graph_run().node, "released");
    assert_eq!(pair.workflow_agent.turns, 0);
    let trace = Runner::new(&pair.workflow.fleet)
        .traces(pair.run_id)
        .unwrap();
    assert!(trace.iter().any(|visit| matches!(
        visit.trace.observations.get("all_linked_prs_released"),
        Some(Observation::Ready { value: Value::Boolean(true), evidence })
            if evidence == "https://github.com/acme/app/releases/tag/v1.0.0"
    )));
    crate::pr_readiness::set_feed_override(None);
}

#[test]
fn github_read_failure_retries_and_exhausts_on_both_paths() {
    let mut pair = Pair::new(
        default_template(),
        "approved",
        &["https://github.com/acme/app/pull/42"],
    );
    crate::pr_readiness::set_feed_override(Some(FailingGithub::new(
        "GitHub is unavailable (HTTP 503)",
    )));

    let max = tod_store::waits::DEFAULT_MAX_ATTEMPTS;
    for attempt in 1..=max {
        let now = chrono::Utc::now().timestamp_millis();
        let legacy = pair.run_legacy();
        let workflow = pair.run_workflow();
        let legacy_wait = read_wait(&pair.legacy).unwrap();
        let graph = pair.graph_run();
        assert_eq!(pair.legacy_phase(), "approved");
        assert_eq!(graph.node, "approved");
        assert_eq!(legacy_wait.attempt, attempt);

        let runner = Runner::new(&pair.workflow.fleet);
        let visit = runner.traces(graph.id).unwrap().pop().unwrap();
        let retry = &visit.trace.github_retries[&GithubOperation::PullRequests];
        assert_eq!(retry.attempt, attempt);
        assert!(retry.message.contains("503"));
        assert!(!retry.parked && !retry.rate_limited);
        let observation = &visit.trace.observations["all_linked_prs_merged"];
        let graph_wait = pair.scheduled_graph_wait();
        if attempt < max {
            let label = format!("{attempt} of {max}");
            require_timed_wait(legacy, &label, now);
            require_timed_wait(workflow, &label, now);
            assert!(!legacy_wait.exhausted());
            assert_eq!(graph.status, RunStatus::Waiting);
            assert!(
                matches!(observation, Observation::Waiting { reason } if reason.contains("503"))
            );
            assert_eq!(
                retry.due_at_ms,
                Some(graph_wait.unwrap().schedule.due_at_ms)
            );
        } else {
            const FAILURE: &str = "GitHub is unavailable (HTTP 503)";
            assert!(
                matches!(&legacy, Outcome::Stopped { reason } if reason.contains(FAILURE) && reason.contains("acme/app#42"))
            );
            assert_eq!(workflow, legacy);
            assert!(legacy_wait.exhausted());
            assert_eq!(graph.status, RunStatus::Error);
            assert!(matches!(observation, Observation::Error { reason } if reason.contains("503")));
            assert!(retry.due_at_ms.is_none() && graph_wait.is_none());
        }
    }
    assert_eq!(pair.legacy_agent.turns, 0);
    assert_eq!(pair.workflow_agent.turns, 0);
    crate::pr_readiness::set_feed_override(None);
}

#[test]
fn review_and_fix_exposes_the_operator_default_dispatch_gap() {
    let mut pair = Pair::new(default_template(), "review", &[]);

    let legacy = pair.run_legacy();
    let reviews = pair
        .legacy
        .fleet
        .read(|conn| {
            ConversationRepo::new(conn).conversations_for_focus(Focus::Node(pair.legacy.node))
        })
        .unwrap();
    for reviewer in tod_store::review::Reviewer::ALL {
        assert!(
            reviews.iter().any(|conversation| {
                conversation.protocol == ProtocolKind::Review
                    && conversation.reviewer == Some(reviewer)
            }),
            "No {reviewer:?} reviewer. Legacy stopped at {legacy:?}."
        );
    }
    assert!(!openings(&pair.legacy, ProtocolKind::Fix).is_empty());
    assert!(!review_findings_of(&pair.legacy).is_empty());

    assert!(
        matches!(pair.run_workflow(), Outcome::Stopped { reason } if reason.contains("needs an action result"))
    );
    let run = pair.graph_run();
    assert_eq!(run.node, "review");
    let visit = Runner::new(&pair.workflow.fleet)
        .current_visit(&run)
        .unwrap();
    assert_eq!(visit.dispatch.unwrap().action.id, MANUAL);
    assert!(visit.output.is_none());
    assert!(review_findings_of(&pair.workflow).is_empty());
    assert_eq!(pair.workflow_agent.turns, 0);
}

#[test]
fn cloud_tasks_use_legacy_execution_even_with_a_saved_workflow() {
    use crate::lifecycle::workflow::migration::{detach, preview_adoption, preview_detach};
    use crate::runner_place::test_support::{sandbox, set_files};

    let mut pair = Pair::new(automated_default(), "proposed", &[]);
    let saved = pair.graph_run();
    let fleet = &pair.workflow.fleet;
    set_files(fleet, pair.workflow.node, sandbox(), false);
    assert!(execute::resume(fleet, saved.clone(), &BTreeMap::new()).is_err());
    assert!(Runner::new(fleet).dispatch(&saved).is_err());

    lifecycle::set_lifecycle(&pair.workflow.fleet, pair.workflow.node, "done").unwrap();
    assert_eq!(pair.run_workflow(), Outcome::Done);
    assert_eq!(pair.workflow_agent.turns, 0);
    assert_eq!(pair.graph_run(), saved);

    let fleet = &pair.workflow.fleet;
    detach(fleet, &preview_detach(fleet, saved.id).unwrap()).unwrap();
    assert!(preview_adoption(fleet, pair.workflow.node, &saved.revision).is_err());
    assert!(
        Runner::new(fleet)
            .assign(pair.workflow.node, &saved.revision, 100)
            .is_err()
    );
}

#[test]
fn automated_default_asks_for_the_same_missing_intent_as_legacy() {
    let mut pair = Pair::new(automated_default(), "proposed", &[]);
    let legacy = pair.run_legacy();
    let workflow = pair.run_workflow();
    assert_eq!(
        legacy,
        Outcome::NeedsHuman {
            reason: NeedsHuman::Decision { pending: 1 }
        }
    );
    assert_eq!(workflow, legacy);
    assert_eq!(pair.legacy_phase(), "proposed");
    assert_eq!(pair.graph_run().node, "proposed");
    assert_eq!(
        Pair::decision_reasons(&pair.workflow),
        Pair::decision_reasons(&pair.legacy)
    );
    assert_eq!(
        Pair::decision_reasons(&pair.workflow),
        [tod_store::decisions::REASON_INTENT]
    );
    assert_eq!(pair.workflow_agent.turns, pair.legacy_agent.turns);
    assert!(pair.workflow_agent.turns > 0);
}

#[test]
fn automated_default_waits_for_merge_and_release_without_manual_completion() {
    let mut pair = Pair::new(
        automated_default(),
        "approved",
        &["https://github.com/acme/app/pull/42"],
    );
    let github = AfterReview::new();
    crate::pr_readiness::set_feed_override(Some(github.clone()));

    let now = chrono::Utc::now().timestamp_millis();
    require_timed_wait(pair.run_legacy(), "merge", now);
    require_timed_wait(pair.run_workflow(), "merge", now);
    let merge_wait = pair.scheduled_graph_wait().unwrap();
    assert!(merge_wait.schedule.what.contains("merge"));
    assert!(merge_wait.schedule.due_at_ms > now);
    assert_eq!(pair.legacy_phase(), "approved");
    assert_eq!(pair.graph_run().node, "approved");

    github.merged.store(true, Ordering::SeqCst);
    *github.notes.lock().unwrap() = Some("* Unrelated change in #7".into());
    let now = chrono::Utc::now().timestamp_millis();
    require_timed_wait(pair.run_legacy(), "release", now);
    require_timed_wait(pair.run_workflow(), "release", now);
    let stored = pair.scheduled_graph_wait().unwrap();
    assert_ne!(stored.id, merge_wait.id);
    assert!(stored.schedule.what.contains("release"));
    assert!(stored.schedule.due_at_ms > now);
    assert_eq!(pair.legacy_phase(), "merged");
    let release_wait = pair.graph_run();
    assert_eq!(release_wait.node, "release_evidence");
    assert_eq!(release_wait.status, RunStatus::Waiting);
    assert_eq!(
        lifecycle::current_state(&pair.workflow.fleet, pair.workflow.node).unwrap(),
        "merged"
    );
    assert_eq!(pair.legacy_agent.turns, 0);
    assert_eq!(pair.workflow_agent.turns, 0);

    *github.notes.lock().unwrap() = Some("* Add the widget by @ann in #42".into());
    assert_eq!(pair.run_legacy(), Outcome::Done);
    assert_eq!(pair.run_workflow(), Outcome::Done);
    assert_eq!(pair.legacy_phase(), "done");
    assert_eq!(pair.graph_run().status, RunStatus::Complete);
    assert!(pair.scheduled_graph_wait().is_none());
    assert_eq!(
        lifecycle::current_state(&pair.workflow.fleet, pair.workflow.node).unwrap(),
        "done"
    );
    assert!(pair.legacy_agent.turns > 0);
    assert!(pair.workflow_agent.turns > 0);
    let runner = Runner::new(&pair.workflow.fleet);
    assert!(runner.visits(pair.run_id).unwrap().iter().all(|visit| {
        visit
            .dispatch
            .as_ref()
            .is_none_or(|request| request.action.id != MANUAL)
    }));
    crate::pr_readiness::set_feed_override(None);
}

fn configured_task() -> Fixture {
    let task = setup();
    task.outline(OutlineMutation::EnableCapabilities {
        node_id: task.node,
        capabilities: vec![Capability::Agent],
    });
    task.fleet.writer().flush().unwrap();
    task.outline(OutlineMutation::SetNodeAgent {
        node_id: task.node,
        platform: Some("claude".into()),
        model: None,
        effort: None,
    });
    task.fleet.writer().flush().unwrap();
    task
}

#[test]
fn automated_default_runs_a_configured_task_through_native_protocols() {
    let mut pair = Pair::with_tasks(automated_default(), "proposed", &[], configured_task);
    assert_eq!(pair.run_legacy(), Outcome::Done);
    let workflow = pair.run_workflow();
    let run = pair.graph_run();
    assert_eq!(workflow, Outcome::Done, "graph stopped at {}", run.node);
    assert_eq!(pair.legacy_phase(), "done");
    assert_eq!(
        lifecycle::current_state(&pair.workflow.fleet, pair.workflow.node).unwrap(),
        "done"
    );
    assert_eq!(
        (run.node.as_str(), run.status),
        ("done", RunStatus::Complete)
    );

    for protocol in [
        ProtocolKind::Implementation,
        ProtocolKind::Verification,
        ProtocolKind::Review,
        ProtocolKind::Fix,
        ProtocolKind::Pr,
    ] {
        let legacy = openings(&pair.legacy, protocol).len();
        let workflow = openings(&pair.workflow, protocol).len();
        assert!(legacy > 0, "fixture should exercise {protocol:?}");
        assert_eq!(workflow, legacy, "session count for {protocol:?}");
    }
    let phase_events = |task: &Fixture, phase: &str| {
        let mut history = task
            .fleet
            .read(|conn| tod_store::phase::PhaseRepo::new(conn).list_for_node(task.node))
            .unwrap()
            .into_iter()
            .filter(|event| event.state == phase)
            .map(|event| (event.kind, event.certifier))
            .collect::<Vec<_>>();
        history.sort();
        history
    };
    let legacy_pr = phase_events(&pair.legacy, "pr");
    assert!(
        legacy_pr
            .iter()
            .any(|(kind, _)| kind == tod_store::phase::PHASE_CERTIFY)
    );
    assert_eq!(phase_events(&pair.workflow, "pr"), legacy_pr);
    let linked_prs = |task: &Fixture| {
        task.fleet
            .read(|conn| tod_store::github::NodePrRepo::new(conn).read(task.node))
            .unwrap()
            .prs
            .into_iter()
            .map(|pr| pr.url)
            .collect::<Vec<_>>()
    };
    let legacy_links = linked_prs(&pair.legacy);
    assert_eq!(
        legacy_links.len(),
        1,
        "fixture should open one pull request"
    );
    assert_eq!(linked_prs(&pair.workflow), legacy_links);
    let runner = Runner::new(&pair.workflow.fleet);
    let visits = runner.visits(run.id).unwrap();
    assert!(visits.iter().all(|visit| {
        visit
            .dispatch
            .as_ref()
            .is_none_or(|request| request.action.id != MANUAL)
    }));
    let review = visits.iter().find(|visit| visit.node == "review").unwrap();
    let captures = captured_context(&pair.workflow.fleet, run.id, review.number).unwrap();
    let openings = pair
        .workflow
        .fleet
        .read(|conn| {
            let repo = ConversationRepo::new(conn);
            let mut openings = HashMap::new();
            for capture in captures {
                if let Some((shared, _)) = capture.text.split_once("# Your assignment")
                    && let Some(row) = repo.get(capture.conversation)?
                    && let Some(reviewer) = row.reviewer
                {
                    openings
                        .entry(reviewer)
                        .or_insert_with(|| shared.to_owned());
                }
            }
            Ok(openings)
        })
        .unwrap();
    use tod_store::review::Reviewer;
    assert!(!openings[&Reviewer::Main].is_empty());
    assert_eq!(openings[&Reviewer::Main], openings[&Reviewer::Risk]);
}

#[test]
fn automated_default_stops_like_legacy_when_obligation_verification_cannot_converge() {
    let mut pair = Pair::with_tasks(automated_default(), "proposed", &[], configured_task);
    let script: VerifyScript = Arc::new(|_, due| due.to_vec());
    pair.legacy_agent.verify_script = Some(script.clone());
    pair.workflow_agent.verify_script = Some(script);
    let legacy = pair.run_legacy();
    let workflow = pair.run_workflow();
    assert!(matches!(&legacy, Outcome::NeedsHuman {
        reason: NeedsHuman::Looping { what, times }
    } if what.contains("It works.") && *times == CYCLE_LIMIT));
    assert_eq!(workflow, legacy);
    assert_eq!(pair.legacy_phase(), "verifying");
    assert_eq!(pair.graph_run().node, "verifying");
    for agent in [&pair.legacy_agent, &pair.workflow_agent] {
        assert_eq!(
            agent.verify_rounds.load(Ordering::SeqCst),
            CYCLE_LIMIT as usize
        );
    }
    let legacy_state = AutopilotState::load(&pair.legacy.root, pair.legacy.node).unwrap();
    let workflow_state = AutopilotState::load(&pair.workflow.root, pair.workflow.node).unwrap();
    assert_eq!(legacy_state.loops.fix_cycles, CYCLE_LIMIT - 1);
    assert_eq!(
        workflow_state.loops.fix_cycles,
        legacy_state.loops.fix_cycles
    );
}

#[test]
fn automated_default_escalates_a_repeated_review_root_cause_like_legacy() {
    struct StopAtDesign(Uuid);
    impl StepHook for StopAtDesign {
        fn at(&mut self, fleet: &FleetStore, boundary: Boundary) -> Result<Option<String>> {
            if boundary != Boundary::Step {
                return Ok(None);
            }
            Ok(fleet
                .read(|conn| tod_store::send_back::SendBackRepo::new(conn).list_for_node(self.0))?
                .iter()
                .any(|move_| {
                    move_.from_state == "review" && move_.to_state == "design" && move_.moved()
                })
                .then(|| "Observed the Review to Design send-back.".into()))
        }
    }

    let mut pair = Pair::with_tasks(automated_default(), "proposed", &[], configured_task);
    let root_cause = "The cache is trusted after a write";
    for agent in [&mut pair.legacy_agent, &mut pair.workflow_agent] {
        agent.review_never_clean = true;
        agent.review_root_cause = Some(root_cause.into());
    }
    let run = |task: &Fixture, agent: &mut FakeAgent| {
        autopilot(task, Budget::default())
            .run_with(&task.fleet, agent, &mut StopAtDesign(task.node))
            .unwrap()
    };
    let legacy = run(&pair.legacy, &mut pair.legacy_agent);
    let workflow = run(&pair.workflow, &mut pair.workflow_agent);
    assert_eq!(
        legacy,
        Outcome::Stopped {
            reason: "Observed the Review to Design send-back.".into()
        }
    );
    assert_eq!(workflow, legacy);

    let find_send_back = |task: &Fixture| {
        task.fleet
            .read(|conn| tod_store::send_back::SendBackRepo::new(conn).list_for_node(task.node))
            .unwrap()
            .into_iter()
            .find(|move_| move_.from_state == "review" && move_.to_state == "design")
            .unwrap()
    };
    let legacy = find_send_back(&pair.legacy);
    let workflow = find_send_back(&pair.workflow);
    assert!(legacy.moved() && workflow.moved());
    assert!(legacy.reason.contains(&format!(
        "root cause \"{root_cause}\" across 3 review rounds"
    )));
    let reason_with_normalized_ids = |task: &Fixture, event: &tod_store::send_back::SendBack| {
        assert_eq!(event.evidence.len(), 3);
        let findings = review_findings_of(task);
        let mut reason = event.reason.clone();
        for item in &event.evidence {
            assert_eq!(item.kind, "finding");
            assert!(findings.iter().any(|finding| finding.id == item.id));
            let short = &item.id.to_string()[..8];
            assert!(reason.contains(&format!("[{short}]")));
            reason = reason.replace(&format!("[{short}]"), "[finding]");
        }
        reason
    };
    assert_eq!(
        reason_with_normalized_ids(&pair.workflow, &workflow),
        reason_with_normalized_ids(&pair.legacy, &legacy),
    );
    assert_eq!(pair.legacy_phase(), "design");
    assert_eq!(pair.graph_run().node, "design");
    assert_eq!(
        lifecycle::current_state(&pair.workflow.fleet, pair.workflow.node).unwrap(),
        "design"
    );
}

mod pr;
