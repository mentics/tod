//! The autopilot against the mock agents (`conversation::mock`, and the
//! phase agent and evaluator in `conversation::phase`), played synchronously
//! against the fixture's store.

use super::*;
use crate::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
use crate::conversation::mock::Direct;
use crate::interview::test_support::{Fixture, fixture};
use crate::media::MediaPaths;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tod_agent::agent_traffic::InterviewAgentCounts;
use tod_agent::{
    AgentLaunchOptions, AgentPlatform, AgentProvider, AgentRunHandle, AgentRunState, RunId,
    SessionTurn,
};
use tod_store::interview::{ACTOR_ENV, ACTOR_USER, InterviewCommand};
use tod_store::outline::repos::PlanStepRepo;
use tod_store::outline::repos::plan_steps::STATUS_BLOCKED;
use tod_store::outline::{Capability, OutlineMutation};
use tod_store::settings::InterviewContextSettings;

/// Plays every protocol's mock agent; each run finishes at once.
struct FakeAgent {
    fleet: Arc<FleetStore>,
    runs: HashMap<RunId, AgentRunState>,
    sessions: HashMap<String, String>,
    turns: usize,
    /// Every turn played, counted where a fake pull request can see it.
    shared_turns: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

impl FakeAgent {
    fn new(fleet: &Arc<FleetStore>) -> Self {
        Self {
            fleet: fleet.clone(),
            runs: HashMap::new(),
            sessions: HashMap::new(),
            turns: 0,
            shared_turns: None,
        }
    }

    fn play(&self, turn: &SessionTurn) -> anyhow::Result<String> {
        let env = |name: &str| {
            turn.env
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        if let (Some(node), Some(conversation)) =
            (env(IMPLEMENT_NODE_ENV), env(IMPLEMENT_CONVERSATION_ENV))
        {
            let user = Direct {
                fleet: &self.fleet,
                actor: env(ACTOR_ENV).unwrap_or_else(|| ACTOR_USER.to_string()),
            };
            let place = crate::conversation::mock::Place {
                cwd: Some(&turn.cwd),
                env: &turn.env,
            };
            return crate::conversation::mock::plan_turn(&user, node.parse()?, conversation.parse()?, place);
        }
        match env(ACTOR_ENV) {
            Some(actor) => {
                let client = Direct {
                    fleet: &self.fleet,
                    actor,
                };
                Ok(crate::conversation::mock::reply(&client, &turn.prompt_blocks())?.text)
            }
            None => Ok(String::new()),
        }
    }
}

impl AgentProvider for FakeAgent {
    fn start_fleet_agent(
        &mut self,
        _: &str,
        _: PathBuf,
        _: String,
        _: AgentLaunchOptions,
        _: String,
        _: tod_agent::AgentEnvironment,
    ) -> anyhow::Result<AgentRunHandle> {
        anyhow::bail!("not used")
    }

    fn send_session_turn(&mut self, turn: SessionTurn) -> anyhow::Result<AgentRunHandle> {
        let id = RunId::new();
        self.sessions
            .entry(turn.key.clone())
            .or_insert_with(|| format!("agent-side-{}", Uuid::new_v4()));
        let state = match self.play(&turn) {
            Ok(text) => AgentRunState::Success(Some(text)),
            Err(err) => AgentRunState::Failure(format!("{err:#}")),
        };
        self.turns += 1;
        if let Some(shared) = &self.shared_turns {
            shared.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        self.runs.insert(id, state);
        Ok(AgentRunHandle { id })
    }

    fn session_id(&self, key: &str) -> Option<String> {
        self.sessions.get(key).cloned()
    }

    fn fleet_run_session_id(&self, _: RunId) -> Option<String> {
        None
    }

    fn session_context_chars(&self, _: &str) -> Option<u64> {
        None
    }

    fn close_session(&mut self, key: &str) {
        self.sessions.remove(key);
    }

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState> {
        self.runs.get(&id).cloned()
    }

    fn respond_to_permission(&mut self, _: RunId, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("not used")
    }

    fn cancel_run(&mut self, _: RunId) -> anyhow::Result<()> {
        Ok(())
    }

    fn interview_status_counts(&self) -> InterviewAgentCounts {
        InterviewAgentCounts::default()
    }
}

fn config(fx: &Fixture) -> ConversationConfig {
    ConversationConfig {
        data_root: fx.root.clone(),
        media: MediaPaths::from_media_root(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("tod")
                .join("media"),
        )
        .unwrap(),
        launch: AgentLaunchOptions::for_platform(AgentPlatform::Claude),
        settings_path: None,
        context: InterviewContextSettings {
            context_budget_tokens: 1_000_000,
            ..Default::default()
        },
    }
}

/// A node with a workspace, one requirement, and a two-step plan.
fn setup() -> Fixture {
    let fx = bare();
    fx.fleet
        .enqueue_outline(OutlineMutation::CreateObligation {
            obligation_id: None,
            node_id: fx.node,
            kind: tod_store::outline::KIND_REQUIREMENT.into(),
            after_id: None,
            before: false,
            section: None,
            body: "It works.".into(),
            phase: tod_store::interview::PHASE_REQUIREMENTS.into(),
        })
        .unwrap();
    for n in 0..2 {
        fx.fleet
            .enqueue_outline(OutlineMutation::CreatePlanStep {
                step_id: None,
                node_id: fx.node,
                after_id: None,
                before: false,
                body: format!("Step {n}"),
            })
            .unwrap();
    }
    fx.fleet.writer().flush().unwrap();
    fx
}

/// What a pull request that is merged, and named in a release, reads as:
/// GitHub as most tests need it, where the pull request is no obstacle.
fn finished_merge() -> tod_store::github::PrMerge {
    tod_store::github::PrMerge {
        title: "Mock change".into(),
        merged: true,
        merged_at: Some("2026-03-01T00:00:00Z".into()),
    }
}

fn finished_releases() -> Vec<tod_store::github::Release> {
    vec![tod_store::github::Release {
        tag: "v1.0.0".into(),
        name: None,
        body: "Mock change".into(),
        url: "https://github.com/mock-owner/mock-repo/releases/tag/v1.0.0".into(),
        published_at: Some("2026-03-02T00:00:00Z".into()),
    }]
}

/// GitHub as a test has it: every pull request merged and released, so
/// nothing about it holds the node up. (An unreadable GitHub is an error the
/// run reports, never something left to the gate: see
/// `an_unreadable_pull_request_fails_the_run_visibly`.)
struct FinishedGithub;

impl crate::pr_readiness::PrFeed for FinishedGithub {
    fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrSnapshot, String> {
        use tod_store::github::{PrSnapshot, PrStatus};
        Ok(PrSnapshot {
            status: PrStatus {
                mergeable: Some(true),
                mergeable_state: Some("clean".into()),
                merged: true,
                checks: Some("success".into()),
                head_sha: Some("abcdef1".into()),
                head_committed_at: Some("2020-01-01T00:00:00Z".into()),
                draft: false,
                base_ref: Some("main".into()),
                body: None,
                ..Default::default()
            },
            threads: vec![],
            comments: vec![],
            reviews: vec![],
        })
    }

    fn comment(&self, _: &tod_store::github::NodePr, _: &str) -> Result<(), String> {
        Ok(())
    }

    fn merge(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrMerge, String> {
        Ok(finished_merge())
    }

    fn releases(&self, _: &tod_store::github::NodePr) -> Result<Vec<tod_store::github::Release>, String> {
        Ok(finished_releases())
    }
}

/// GitHub that fails every look with `error`.
struct FailingGithub {
    error: std::sync::Mutex<String>,
}

impl FailingGithub {
    fn new(error: &str) -> Arc<Self> {
        Arc::new(Self { error: std::sync::Mutex::new(error.to_string()) })
    }
}

impl crate::pr_readiness::PrFeed for FailingGithub {
    fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrSnapshot, String> {
        Err(self.error.lock().unwrap().clone())
    }

    fn comment(&self, _: &tod_store::github::NodePr, _: &str) -> Result<(), String> {
        Err(self.error.lock().unwrap().clone())
    }

    fn merge(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrMerge, String> {
        Err(self.error.lock().unwrap().clone())
    }

    fn releases(&self, _: &tod_store::github::NodePr) -> Result<Vec<tod_store::github::Release>, String> {
        Err(self.error.lock().unwrap().clone())
    }
}

/// A node with a workspace and nothing else: no requirements, no plan.
fn bare() -> Fixture {
    crate::pr_readiness::set_feed_override(Some(Arc::new(FinishedGithub)));
    let fx = fixture();
    let workspace = fx.root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    fx.fleet
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: fx.node,
            capabilities: vec![Capability::Files, Capability::Lifecycle],
        })
        .unwrap();
    fx.fleet
        .enqueue_outline(OutlineMutation::SetLifecycle {
            node_id: fx.node,
            state: "proposed".into(),
        })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    // The workspace itself: a worktree of its own would need a data root.
    tod_store::fleet::places::pick_matching(
        &fx.fleet,
        &fx.node.to_string(),
        &tod_store::fleet::FilesPlace {
            repo: Some(workspace.display().to_string()),
            use_worktree: false,
            ..Default::default()
        },
    )
    .unwrap();
    fx.fleet.writer().flush().unwrap();
    fx
}

/// Retire the gate criteria that need what a test has not got: an agent
/// configuration, and GitHub.
fn retire_outside_criteria(fx: &Fixture) {
    let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
    conn.execute(
        "UPDATE gate_criteria SET active = 0
         WHERE slug = ?1 OR from_state IN ('pr', 'pr_review', 'approved')",
        [tod_store::outline::repos::gate::READY_ACTIVE_ACTION_CONFIG_SLUG],
    )
    .unwrap();
    drop(conn);
    fx.fleet.reload_if_stale().ok();
}

fn autopilot(fx: &Fixture, budget: Budget) -> Autopilot {
    Autopilot::new(config(fx), fx.node, budget)
        .unwrap()
        .with_poll_interval(Duration::ZERO)
}

#[test]
fn takes_a_node_from_proposed_to_done() {
    let fx = setup();
    retire_outside_criteria(&fx);
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, Budget::default());
    let outcome = pilot.run(&fx.fleet, &mut agent).unwrap();
    let steps: Vec<_> = pilot
        .state()
        .steps
        .iter()
        .map(|s| format!("{} {}->{}", s.step, s.from, s.to))
        .collect();
    assert_eq!(outcome, Outcome::Done, "{steps:#?}");
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "done");
    for step in [
        "phase",
        "evaluate",
        "implementation",
        "verification",
        "review",
        "pr",
        "advance",
    ] {
        assert!(steps.iter().any(|s| s.starts_with(step)), "{step}: {steps:#?}");
    }
    // Saved: a new autopilot reads the same run back.
    let saved = AutopilotState::load(&fx.root, fx.node).unwrap();
    assert_eq!(saved.outcome, Some(Outcome::Done));
    assert_eq!(&saved, pilot.state());
    assert_eq!(saved.current, None);
    // Every finished step's session was ended, not left holding an agent.
    assert!(agent.sessions.is_empty(), "{:?}", agent.sessions.keys().collect::<Vec<_>>());
}

#[test]
fn stops_at_a_failing_criterion_it_cannot_fix() {
    // No agent configured: `ready` → `active` fails, and nothing earlier is
    // owed that could fix it.
    let fx = setup();
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default())
        .run(&fx.fleet, &mut agent)
        .unwrap();
    assert!(
        matches!(
            &outcome,
            Outcome::NeedsHuman {
                reason: NeedsHuman::FailingCriteria { criteria }
            } if !criteria.is_empty()
        ),
        "{outcome:?}"
    );
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "ready");
}

#[test]
fn stops_for_a_blocked_step() {
    let fx = setup();
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "active").unwrap();
    let steps = fx
        .fleet
        .read(|conn| Ok(PlanStepRepo::new(conn).list_for_node(fx.node)?))
        .unwrap();
    for step in steps {
        fx.fleet
            .enqueue_outline(OutlineMutation::UpdatePlanStepStatus {
                step_id: step.id,
                status: STATUS_BLOCKED.to_string(),
                note: None,
                reason: None,
            })
            .unwrap();
    }
    fx.fleet.writer().flush().unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default())
        .run(&fx.fleet, &mut agent)
        .unwrap();
    assert_eq!(
        outcome,
        Outcome::NeedsHuman {
            reason: NeedsHuman::BlockedSteps { count: 2 }
        }
    );
    assert_eq!(agent.turns, 0);
}

#[test]
fn stops_for_a_pending_decision() {
    let fx = setup();
    fx.fleet
        .interview(
            ACTOR_USER,
            InterviewCommand::AskDecision {
                node_id: fx.node,
                conversation_id: None,
                protocol: None,
                decision: tod_store::decisions::NewDecision {
                    question: "Which database?".into(),
                    options: vec!["SQLite".into(), "Postgres".into()],
                    evidence: Vec::new(),
                    ..Default::default()
                },
            },
        )
        .unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default())
        .run(&fx.fleet, &mut agent)
        .unwrap();
    assert_eq!(
        outcome,
        Outcome::NeedsHuman {
            reason: NeedsHuman::Decision { pending: 1 }
        }
    );
}

#[test]
fn stops_when_the_session_budget_runs_out_and_a_restart_keeps_count() {
    let fx = setup();
    retire_outside_criteria(&fx);
    let budget = Budget {
        max_sessions: 2,
        ..Budget::default()
    };
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, budget).run(&fx.fleet, &mut agent).unwrap();
    assert_eq!(
        outcome,
        Outcome::BudgetExhausted {
            limit: BudgetLimit::Sessions { used: 2 }
        }
    );

    // A restart reads the spent budget back and stops at once.
    let mut again = autopilot(&fx, budget);
    assert_eq!(again.state().sessions, 2);
    let turns = agent.turns;
    assert_eq!(again.run(&fx.fleet, &mut agent).unwrap(), outcome);
    assert_eq!(agent.turns, turns);

    // With a larger budget it continues from where it stopped.
    let mut more = autopilot(
        &fx,
        Budget {
            max_sessions: 100,
            ..budget
        },
    );
    assert_eq!(more.run(&fx.fleet, &mut agent).unwrap(), Outcome::Done);
    assert_eq!(more.state().steps.first(), again.state().steps.first());
}

#[test]
fn a_restart_reopens_the_conversation_in_progress() {
    let fx = setup();
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "active").unwrap();
    // A run that stopped mid-implementation: its conversation is saved as
    // current.
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver = ConversationDriver::new(
        config(&fx),
        Focus::Node(fx.node),
        ProtocolKind::Implementation,
    );
    driver.send(&fx.fleet, &mut agent, "Implement the plan.").unwrap();
    let id = driver.conversation_id().unwrap();
    let mut state = AutopilotState::fresh();
    state.current = Some(CurrentStep {
        protocol: "implementation".into(),
        lifecycle: "active".into(),
        conversation_id: id,
    });
    state.save(&fx.root, fx.node).unwrap();

    let mut pilot = autopilot(&fx, Budget::default());
    pilot.run(&fx.fleet, &mut agent).unwrap();
    let first = pilot.state().steps.first().unwrap();
    assert_eq!(first.step, "implementation");
    assert_eq!(first.conversation_id, Some(id));
    let turns = fx
        .fleet
        .read(|conn| ConversationRepo::new(conn).turns(id))
        .unwrap();
    assert!(
        turns.iter().any(|t| t.body == RESUME_MESSAGE),
        "{:?}",
        turns.iter().map(|t| &t.body).collect::<Vec<_>>()
    );
}

#[test]
fn a_requested_fresh_session_starts_a_new_conversation_instead_of_reopening() {
    let fx = setup();
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "active").unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver = ConversationDriver::new(
        config(&fx),
        Focus::Node(fx.node),
        ProtocolKind::Implementation,
    );
    driver.send(&fx.fleet, &mut agent, "Implement the plan.").unwrap();
    let id = driver.conversation_id().unwrap();
    let mut state = AutopilotState::fresh();
    state.current = Some(CurrentStep {
        protocol: "implementation".into(),
        lifecycle: "active".into(),
        conversation_id: id,
    });
    state.save(&fx.root, fx.node).unwrap();
    AutopilotState::request_fresh_session(&fx.root, fx.node).unwrap();

    let mut pilot = autopilot(&fx, Budget::default());
    // The request was taken as the run was set up, so a later run does not
    // start a new session again.
    assert!(!AutopilotState::load(&fx.root, fx.node).unwrap().fresh_session);
    pilot.run(&fx.fleet, &mut agent).unwrap();
    let first = pilot.state().steps.first().unwrap();
    assert_eq!(first.step, "implementation");
    assert_ne!(first.conversation_id, Some(id), "the earlier conversation was not reopened");
    let turns = fx.fleet.read(|conn| ConversationRepo::new(conn).turns(id)).unwrap();
    assert!(!turns.iter().any(|t| t.body == RESUME_MESSAGE));
}

/// Stops the run from `watch`, the first time a turn is in flight.
struct StopMidTurn(bool);

impl StepHook for StopMidTurn {
    fn at(&mut self, _: &FleetStore, _: Boundary) -> Result<Option<String>> {
        Ok(None)
    }

    fn watch(&mut self, _: Turn<'_>) -> Option<String> {
        (!std::mem::replace(&mut self.0, true)).then(|| "stop".to_string())
    }
}

#[test]
fn stopping_mid_turn_keeps_the_conversation_for_the_next_run() {
    let fx = setup();
    retire_outside_criteria(&fx);
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "active").unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, Budget::default());
    let outcome = pilot.run_with(&fx.fleet, &mut agent, &mut StopMidTurn(false)).unwrap();
    assert_eq!(outcome, Outcome::Stopped { reason: "stop".into() });
    let current = pilot.state().current.clone().expect("the conversation stays current");
    assert_eq!(current.protocol, "implementation");

    // The next run reopens it and carries on to the end.
    let mut pilot = autopilot(&fx, Budget::default());
    assert_eq!(pilot.run(&fx.fleet, &mut agent).unwrap(), Outcome::Done);
    let first = &pilot.state().steps[0];
    assert_eq!((first.step.as_str(), first.conversation_id), (current.protocol.as_str(), Some(current.conversation_id)));
}

#[test]
fn a_renewed_budget_keeps_the_steps() {
    let fx = setup();
    retire_outside_criteria(&fx);
    let mut agent = FakeAgent::new(&fx.fleet);
    let budget = Budget {
        max_sessions: 1,
        ..Budget::default()
    };
    let mut pilot = autopilot(&fx, budget);
    assert!(matches!(
        pilot.run(&fx.fleet, &mut agent).unwrap(),
        Outcome::BudgetExhausted { .. }
    ));
    let steps = pilot.state().steps.len();
    assert!(steps > 0);
    pilot.renew_budget().unwrap();
    let saved = AutopilotState::load(&fx.root, fx.node).unwrap();
    assert_eq!(saved.sessions, 0);
    assert_eq!(saved.steps.len(), steps);
}

#[test]
fn a_local_run_takes_the_node_to_done_on_its_own_thread() {
    use super::local::{LocalEvent, LocalRun};
    let fx = setup();
    retire_outside_criteria(&fx);
    let agent: tod_agent::SharedAgent =
        Arc::new(std::sync::Mutex::new(Box::new(FakeAgent::new(&fx.fleet))));
    let (tx, rx) = std::sync::mpsc::channel();
    let run = LocalRun::start_with(
        fx.fleet.clone(),
        agent,
        config(&fx),
        fx.node,
        Budget::default(),
        false,
        Some(Duration::ZERO),
        move |event| {
            let _ = tx.send(event);
        },
    )
    .unwrap();
    let events: Vec<LocalEvent> = rx.iter().collect();
    run.join();
    assert!(
        events.iter().any(|e| matches!(e, LocalEvent::Live(live) if live.protocol == Some(ProtocolKind::Implementation))),
        "{events:#?}"
    );
    assert_eq!(events.last(), Some(&LocalEvent::Finished(Ok(Outcome::Done))));
    // The run held a lease while it worked and let it go when it ended: the
    // node is not left claiming to be processed.
    fx.fleet.writer().flush().unwrap();
    let leases = fx
        .fleet
        .read(|conn| tod_store::activity::ActivityRepo::new(conn).list_for_nodes(&[fx.node]))
        .unwrap();
    assert!(leases.is_empty(), "{leases:?}");
}

#[test]
fn a_local_run_asked_to_pause_stops_at_the_first_boundary() {
    use super::local::{LocalEvent, LocalRun, PAUSED, stopped_by_user};
    let fx = setup();
    retire_outside_criteria(&fx);
    let agent: tod_agent::SharedAgent =
        Arc::new(std::sync::Mutex::new(Box::new(FakeAgent::new(&fx.fleet))));
    let (tx, rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    // Held on the first event until the pause is asked for.
    let mut first = Some(go_rx);
    let run = LocalRun::start_with(
        fx.fleet.clone(),
        agent,
        config(&fx),
        fx.node,
        Budget::default(),
        false,
        Some(Duration::ZERO),
        move |event| {
            if let Some(go) = first.take() {
                let _ = go.recv();
            }
            let _ = tx.send(event);
        },
    )
    .unwrap();
    run.pause();
    go_tx.send(()).unwrap();
    let events: Vec<LocalEvent> = rx.iter().collect();
    run.join();
    let outcome = Outcome::Stopped { reason: PAUSED.into() };
    assert!(stopped_by_user(&outcome));
    assert_eq!(events.last(), Some(&LocalEvent::Finished(Ok(outcome))));
    assert_ne!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "done");
}

/// A node with nothing on it that says what it is for: the `proposed` agent
/// asks the user, in free text, and carries on from the answer.
#[test]
fn proposed_with_nothing_to_go_on_asks_the_user_what_it_is_for() {
    let fx = bare();
    retire_outside_criteria(&fx);
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default())
        .run(&fx.fleet, &mut agent)
        .unwrap();
    assert_eq!(
        outcome,
        Outcome::NeedsHuman {
            reason: NeedsHuman::Decision { pending: 1 }
        }
    );
    let asked = fx
        .fleet
        .read(|conn| Ok(tod_store::decisions::DecisionRepo::new(conn).list_for_node(fx.node)?))
        .unwrap();
    let [question] = asked.as_slice() else {
        panic!("{asked:?}");
    };
    assert!(question.options.is_empty(), "free text: {question:?}");
    assert_eq!(question.reason, tod_store::decisions::REASON_INTENT);

    fx.fleet
        .interview(
            ACTOR_USER,
            InterviewCommand::AnswerDecision {
                decision_id: question.id,
                option: None,
                text: Some("Show the weather.".into()),
            },
        )
        .unwrap();
    let outcome = autopilot(&fx, Budget::default())
        .run(&fx.fleet, &mut agent)
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    let requirements = fx
        .fleet
        .read(|conn| Ok(tod_store::outline::repos::ObligationRepo::new(conn).list_for_node(fx.node)?))
        .unwrap();
    assert!(
        requirements.iter().any(|o| o.body == "Show the weather."),
        "{requirements:?}"
    );
}

/// An evaluator that sends the same work back twice, with nothing changed
/// between, stops the run for the user instead of looping.
#[test]
fn an_evaluator_that_keeps_rejecting_the_same_work_stops_the_run() {
    let fx = setup();
    retire_outside_criteria(&fx);
    fx.fleet
        .enqueue_outline(OutlineMutation::UpdateNodeTitle {
            node_id: fx.node,
            title: "Interview node [reject]".into(),
        })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, Budget::default());
    let outcome = pilot.run(&fx.fleet, &mut agent).unwrap();
    assert!(
        matches!(
            &outcome,
            Outcome::NeedsHuman {
                reason: NeedsHuman::EvaluationStuck { fixes }
            } if !fixes.is_empty()
        ),
        "{outcome:?}"
    );
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "proposed");
    let steps: Vec<_> = pilot.state().steps.iter().map(|s| s.step.clone()).collect();
    assert_eq!(steps, ["phase", "evaluate", "phase", "evaluate"], "{steps:?}");
}

/// A pull request that changes as the agent takes turns: it opens with an
/// unanswered review thread; once the agent has taken a turn the thread is
/// answered and the head is new, and the review bot has not looked at it
/// until it is asked and has been polled a couple of times.
struct Sim {
    turns: Arc<std::sync::atomic::AtomicUsize>,
    base: std::sync::Mutex<Option<usize>>,
    posted: std::sync::Mutex<Vec<String>>,
    polls_after_ask: std::sync::atomic::AtomicUsize,
}

impl crate::pr_readiness::PrFeed for Sim {
    fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrSnapshot, String> {
        use std::sync::atomic::Ordering::SeqCst;
        use tod_store::github::{IssueComment, PrSnapshot, PrStatus, ReviewThread};
        let turns = self.turns.load(SeqCst);
        let base = *self.base.lock().unwrap().get_or_insert(turns);
        let round = turns - base;
        let asked = !self.posted.lock().unwrap().is_empty();
        if asked {
            self.polls_after_ask.fetch_add(1, SeqCst);
        }
        let mut comments = Vec::new();
        if asked {
            comments.push(IssueComment {
                id: 1,
                author: Some("someone".into()),
                body: self.posted.lock().unwrap()[0].clone(),
                created_at: "2020-01-02T00:00:00Z".into(),
                updated_at: "2020-01-02T00:00:00Z".into(),
            });
        }
        if self.polls_after_ask.load(SeqCst) >= 3 {
            comments.push(IssueComment {
                id: 2,
                author: Some("greptile-apps".into()),
                body: "Confidence Score: 5/5\n".into(),
                created_at: "2021-01-01T00:00:00Z".into(),
                updated_at: "2021-01-01T00:00:00Z".into(),
            });
        }
        Ok(PrSnapshot {
            status: PrStatus {
                mergeable: Some(true),
                mergeable_state: Some("blocked".into()),
                merged: false,
                checks: Some("success".into()),
                head_sha: Some(format!("{:07}", round.min(1) + 1)),
                head_committed_at: Some("2020-01-01T00:00:00Z".into()),
                draft: false,
                open: true,
                reviewers: Vec::new(),
                base_ref: Some("main".into()),
                body: None,
                ..Default::default()
            },
            threads: if round == 0 {
                vec![ReviewThread {
                    id: "PRRT_1".into(),
                    resolved: false,
                    outdated: false,
                    path: Some("src/a.rs".into()),
                    line: Some(1),
                    comments: vec![],
                }]
            } else {
                Vec::new()
            },
            comments,
            // A person approves once the bot has been through.
            reviews: if self.polls_after_ask.load(SeqCst) >= 3 {
                vec![tod_store::github::PrReview {
                    author: Some("alice".into()),
                    submitted_at: "2021-06-01T00:00:00Z".into(),
                    state: "APPROVED".into(),
                }]
            } else {
                vec![]
            },
        })
    }

    fn comment(&self, _: &tod_store::github::NodePr, body: &str) -> Result<(), String> {
        self.posted.lock().unwrap().push(body.to_string());
        Ok(())
    }

    // Once the review is through, the pull request merges and ships.
    fn merge(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrMerge, String> {
        Ok(finished_merge())
    }

    fn releases(&self, _: &tod_store::github::NodePr) -> Result<Vec<tod_store::github::Release>, String> {
        Ok(finished_releases())
    }
}

#[test]
fn babysits_a_pull_request_through_feedback_and_a_bots_review() {
    let fx = setup();
    retire_outside_criteria(&fx);
    std::fs::write(
        fx.root.join("tod.yml"),
        "pr_readiness:\n  bots:\n    - name: greptile\n",
    )
    .unwrap();
    let turns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sim = Arc::new(Sim {
        turns: turns.clone(),
        base: std::sync::Mutex::new(None),
        posted: std::sync::Mutex::new(Vec::new()),
        polls_after_ask: std::sync::atomic::AtomicUsize::new(0),
    });
    crate::pr_readiness::set_feed_override(Some(sim.clone()));
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "pr").unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    agent.shared_turns = Some(turns);
    let mut pilot = autopilot(&fx, Budget::default()).with_pr_poll_interval(Duration::ZERO);
    let outcome = pilot.run(&fx.fleet, &mut agent).unwrap();
    let steps: Vec<_> = pilot.state().steps.iter().map(|s| format!("{} {}->{}", s.step, s.from, s.to)).collect();
    assert_eq!(outcome, Outcome::Done, "{steps:#?}");
    // The agent opened the PR, then took another turn for the open thread.
    assert!(steps.iter().any(|s| s.starts_with("pr ")), "{steps:#?}");
    assert!(agent.turns >= 2 + 1, "turns: {}", agent.turns);
    // The bot had not reviewed the new head: it was asked, once, and waited for.
    assert_eq!(*sim.posted.lock().unwrap(), vec!["@greptileai review this".to_string()]);
    assert!(sim.polls_after_ask.load(std::sync::atomic::Ordering::SeqCst) >= 3);
}

#[test]
fn pr_review_never_opens_a_pull_request() {
    // Opening the draft is `pr`'s work: a node that reaches `pr_review` with
    // none linked hands back rather than start a second state's job.
    let fx = setup();
    retire_outside_criteria(&fx);
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "pr_review").unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert!(
        matches!(outcome, Outcome::NeedsHuman { reason: NeedsHuman::PrBlocked { .. } }),
        "{outcome:?}"
    );
    assert_eq!(agent.turns, 0);
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "pr_review");
}

#[test]
fn pr_runs_its_agent_on_every_stay_even_with_a_pull_request_and_no_work() {
    // `pr` gets the draft ready for review: a linked pull request that shows
    // no work still gets the agent's turn (the phase's skills apply), once per
    // stay, and the run then moves on to the gate.
    let fx = setup();
    retire_outside_criteria(&fx);
    struct Clean;
    impl crate::pr_readiness::PrFeed for Clean {
        fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrSnapshot, String> {
            use tod_store::github::{PrSnapshot, PrStatus};
            Ok(PrSnapshot {
                status: PrStatus {
                    mergeable: Some(true),
                    mergeable_state: Some("blocked".into()),
                    checks: Some("success".into()),
                    head_sha: Some("abcdef1".into()),
                    head_committed_at: Some("2020-01-01T00:00:00Z".into()),
                    draft: true,
                    open: true,
                    ..Default::default()
                },
                threads: vec![],
                comments: vec![],
                reviews: vec![],
            })
        }
        fn comment(&self, _: &tod_store::github::NodePr, _: &str) -> Result<(), String> {
            Ok(())
        }
    }
    crate::pr_readiness::set_feed_override(Some(Arc::new(Clean)));
    linked_to_prs(&fx, "pr", &["https://github.com/acme/app/pull/42"]);
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut pilot = autopilot(&fx, Budget::default()).with_pr_poll_interval(Duration::ZERO);
    let outcome = pilot.run(&fx.fleet, &mut agent).unwrap();
    assert_eq!(agent.turns, 1, "{outcome:?}");
    assert!(!matches!(outcome, Outcome::NeedsHuman { reason: NeedsHuman::PrBlocked { .. } }), "{outcome:?}");

    // Run again in the same stay: the agent is not sent again.
    let mut again = autopilot(&fx, Budget::default()).with_pr_poll_interval(Duration::ZERO);
    again.run(&fx.fleet, &mut agent).unwrap();
    assert_eq!(agent.turns, 1);

    // A new session was asked for: it goes out again.
    super::AutopilotState::request_fresh_session(&fx.root, fx.node).unwrap();
    let mut fresh = autopilot(&fx, Budget::default()).with_pr_poll_interval(Duration::ZERO);
    fresh.run(&fx.fleet, &mut agent).unwrap();
    assert_eq!(agent.turns, 2);
}

#[test]
fn a_pull_request_waiting_for_a_person_ends_the_run_with_a_timed_wait() {
    let fx = setup();
    retire_outside_criteria(&fx);
    struct Pending;
    impl crate::pr_readiness::PrFeed for Pending {
        fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrSnapshot, String> {
            use tod_store::github::{PrSnapshot, PrStatus};
            Ok(PrSnapshot {
                status: PrStatus {
                    mergeable: Some(true),
                    mergeable_state: Some("blocked".into()),
                    merged: false,
                    checks: Some("success".into()),
                    head_sha: Some("abcdef1".into()),
                    head_committed_at: Some("2020-01-01T00:00:00Z".into()),
                    draft: false,
                    open: true,
                    reviewers: Vec::new(),
                    base_ref: None,
                    body: None,
                    ..Default::default()
                },
                threads: vec![],
                comments: vec![],
                reviews: vec![],
            })
        }
        fn comment(&self, _: &tod_store::github::NodePr, _: &str) -> Result<(), String> {
            Ok(())
        }
    }
    crate::pr_readiness::set_feed_override(Some(Arc::new(Pending)));
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "pr").unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let before = chrono::Utc::now().timestamp_millis();
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    let Outcome::Waiting { due_at_ms, .. } = outcome else {
        panic!("expected Waiting, got {outcome:?}");
    };
    assert!(due_at_ms > before, "the next look is in the future");
    // The wait is recorded, so a scheduler (and a webhook) can wake the node.
    let waits = fx
        .fleet
        .read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(fx.node))
        .unwrap();
    assert_eq!(waits.len(), 1, "{waits:?}");
    assert!(waits[0].match_spec.starts_with("github:pr:"), "{waits:?}");
    assert_eq!(waits[0].due_at, due_at_ms);
}

#[test]
fn the_wait_for_a_person_records_who_was_asked_to_review() {
    let fx = setup();
    retire_outside_criteria(&fx);
    struct Asked;
    impl crate::pr_readiness::PrFeed for Asked {
        fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrSnapshot, String> {
            use tod_store::github::{PrSnapshot, PrStatus};
            Ok(PrSnapshot {
                status: PrStatus {
                    mergeable: Some(true),
                    mergeable_state: Some("blocked".into()),
                    checks: Some("success".into()),
                    head_sha: Some("abcdef1".into()),
                    head_committed_at: Some("2020-01-01T00:00:00Z".into()),
                    reviewers: vec!["maria".into(), "acme/core".into()],
                    open: true,
                    ..Default::default()
                },
                threads: vec![],
                comments: vec![],
                reviews: vec![],
            })
        }
        fn comment(&self, _: &tod_store::github::NodePr, _: &str) -> Result<(), String> {
            Ok(())
        }
    }
    crate::pr_readiness::set_feed_override(Some(Arc::new(Asked)));
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "pr").unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert!(matches!(outcome, Outcome::Waiting { .. }), "{outcome:?}");
    let waits = fx
        .fleet
        .read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(fx.node))
        .unwrap();
    assert_eq!(waits.len(), 1, "{waits:?}");
    assert!(waits[0].match_spec.starts_with("github:pr:"), "{waits:?}");
    assert!(waits[0].match_spec.ends_with(":review:maria,acme/core"), "{waits:?}");
    assert_eq!(crate::next_actor::WaitKind::from_spec(&waits[0].match_spec), crate::next_actor::WaitKind::HumanReview);
}

#[test]
fn a_pull_request_that_never_changes_stops_for_a_person() {
    let fx = setup();
    retire_outside_criteria(&fx);
    struct Stuck;
    impl crate::pr_readiness::PrFeed for Stuck {
        fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrSnapshot, String> {
            use tod_store::github::{PrSnapshot, PrStatus, ReviewThread};
            Ok(PrSnapshot {
                status: PrStatus {
                    mergeable: Some(true),
                    mergeable_state: Some("blocked".into()),
                    merged: false,
                    checks: Some("success".into()),
                    head_sha: Some("abcdef1".into()),
                    head_committed_at: Some("2020-01-01T00:00:00Z".into()),
                    draft: false,
                    open: true,
                    reviewers: Vec::new(),
                    base_ref: None,
                    body: None,
                    ..Default::default()
                },
                threads: vec![ReviewThread {
                    id: "PRRT_1".into(),
                    resolved: false,
                    outdated: false,
                    path: None,
                    line: None,
                    comments: vec![],
                }],
                comments: vec![],
                reviews: vec![],
            })
        }
        fn comment(&self, _: &tod_store::github::NodePr, _: &str) -> Result<(), String> {
            Ok(())
        }
    }
    crate::pr_readiness::set_feed_override(Some(Arc::new(Stuck)));
    lifecycle::set_lifecycle(&fx.fleet, fx.node, "pr").unwrap();
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert!(
        matches!(&outcome, Outcome::NeedsHuman { reason: NeedsHuman::PrStuck { .. } }),
        "{outcome:?}"
    );
}

/// GitHub after the review: whether the pull request is merged yet, and the
/// notes of the one release (if any) published since.
struct AfterReview {
    merged: std::sync::atomic::AtomicBool,
    notes: std::sync::Mutex<Option<String>>,
}

impl AfterReview {
    fn new() -> Arc<Self> {
        Arc::new(Self { merged: Default::default(), notes: Default::default() })
    }
}

impl crate::pr_readiness::PrFeed for AfterReview {
    fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<tod_store::github::PrSnapshot, String> {
        Err("not read after the review".to_string())
    }

    fn comment(&self, _: &tod_store::github::NodePr, _: &str) -> Result<(), String> {
        Ok(())
    }

    fn merge(&self, pr: &tod_store::github::NodePr) -> Result<tod_store::github::PrMerge, String> {
        let merged = self.merged.load(std::sync::atomic::Ordering::SeqCst);
        Ok(tod_store::github::PrMerge {
            title: if pr.pr_number == 7 { "Apply the infra change" } else { "Add the widget" }.into(),
            merged,
            merged_at: merged.then(|| "2026-03-01T00:00:00Z".to_string()),
        })
    }

    fn releases(&self, _: &tod_store::github::NodePr) -> Result<Vec<tod_store::github::Release>, String> {
        Ok(self
            .notes
            .lock()
            .unwrap()
            .iter()
            .map(|body| tod_store::github::Release {
                tag: "v1.0.0".into(),
                name: None,
                body: body.clone(),
                url: "https://github.com/acme/app/releases/tag/v1.0.0".into(),
                published_at: Some("2026-03-02T00:00:00Z".into()),
            })
            .collect())
    }
}

/// A node with the pull request linked, at `lifecycle`.
fn linked_to_pr(fx: &Fixture, lifecycle: &str) {
    linked_to_prs(fx, lifecycle, &["https://github.com/acme/app/pull/42"]);
}

fn linked_to_prs(fx: &Fixture, lifecycle: &str, urls: &[&str]) {
    fx.fleet
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: fx.node,
            capabilities: vec![Capability::Ticket],
        })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    fx.fleet
        .enqueue(tod_store::fleet::FleetMutation::UpdateTaskLinkedPrs {
            id: fx.node.to_string(),
            linked_prs: urls.iter().map(|u| u.to_string()).collect(),
        })
        .unwrap();
    fx.fleet.writer().flush().unwrap();
    fx.fleet.reload_if_stale().ok();
    lifecycle::set_lifecycle(&fx.fleet, fx.node, lifecycle).unwrap();
}

fn pending_specs(fx: &Fixture) -> Vec<String> {
    fx.fleet
        .read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(fx.node))
        .unwrap()
        .into_iter()
        .map(|w| w.match_spec)
        .collect()
}

#[test]
fn an_approved_node_waits_for_the_merge_then_for_the_release() {
    let fx = bare();
    retire_outside_criteria(&fx);
    let github = AfterReview::new();
    crate::pr_readiness::set_feed_override(Some(github.clone()));
    linked_to_pr(&fx, "approved");
    let mut agent = FakeAgent::new(&fx.fleet);

    // Not merged: the run ends with a timed wait a scheduler can wake.
    let before = chrono::Utc::now().timestamp_millis();
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    let Outcome::Waiting { due_at_ms, what } = outcome else {
        panic!("expected Waiting, got {outcome:?}");
    };
    assert!(due_at_ms > before && what.contains("merged"), "{what}");
    assert_eq!(pending_specs(&fx), ["github:pr:42:merged"]);
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "approved");
    assert_eq!(agent.turns, 0, "waiting uses no agent");

    // A wake before the merge waits again, replacing the wait.
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert!(matches!(outcome, Outcome::Waiting { .. }), "{outcome:?}");
    assert_eq!(pending_specs(&fx), ["github:pr:42:merged"]);

    // Merged: it goes on, and now waits for a release.
    github.merged.store(true, std::sync::atomic::Ordering::SeqCst);
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    let Outcome::Waiting { what, .. } = outcome else {
        panic!("expected Waiting, got {outcome:?}");
    };
    assert!(what.contains("release"), "{what}");
    assert_eq!(lifecycle::current_state(&fx.fleet, fx.node).unwrap(), "merged");
    assert_eq!(pending_specs(&fx), ["github:release:published"]);
    assert_eq!(agent.turns, 0, "the release is not driven while it is awaited");

    // A release whose notes do not name the pull request is not its release.
    *github.notes.lock().unwrap() = Some("* Something else by @bob in #7".into());
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert!(matches!(outcome, Outcome::Waiting { .. }), "{outcome:?}");

    // One that does lets the lifecycle go on as normal, and clears the wait.
    *github.notes.lock().unwrap() = Some("* Add the widget by @ann in #42".into());
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert_eq!(outcome, Outcome::Done, "{outcome:?}");
    assert!(pending_specs(&fx).is_empty(), "{:?}", pending_specs(&fx));
}

fn read_wait(fx: &Fixture) -> Option<tod_store::waits::Wait> {
    fx.fleet
        .read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(fx.node))
        .unwrap()
        .into_iter()
        .find(|w| w.match_spec == "github:pr:read")
}

#[test]
fn an_unreadable_pull_request_fails_the_run_visibly() {
    let fx = bare();
    retire_outside_criteria(&fx);
    // An error asking again cannot fix: the run ends failed, naming it, and
    // the failure is on the node (not in a log), where it shows.
    crate::pr_readiness::set_feed_override(Some(FailingGithub::new(
        "GitHub rejected the token (HTTP 401): it is expired, revoked, or mistyped",
    )));
    linked_to_pr(&fx, "approved");
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    let Outcome::Stopped { reason } = &outcome else { panic!("{outcome:?}") };
    assert!(reason.starts_with(crate::autopilot::local::FAILED_PREFIX), "{reason}");
    assert!(reason.contains("rejected the token"), "{reason}");

    let wait = read_wait(&fx).expect("the failure is recorded on the node");
    assert!(wait.exhausted(), "{wait:?}");
    assert_eq!(wait.attempt, wait.max_attempts.unwrap());
    assert!(wait.last_error.as_deref().unwrap().contains("rejected the token"));
}

#[test]
fn a_pull_request_github_could_not_serve_is_retried_then_fails_visibly() {
    let fx = bare();
    retire_outside_criteria(&fx);
    crate::pr_readiness::set_feed_override(Some(FailingGithub::new("GitHub is unavailable (HTTP 503)")));
    linked_to_pr(&fx, "approved");
    let mut agent = FakeAgent::new(&fx.fleet);

    let max = tod_store::waits::DEFAULT_MAX_ATTEMPTS;
    for attempt in 1..max {
        let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
        let Outcome::Waiting { what, due_at_ms } = &outcome else { panic!("attempt {attempt}: {outcome:?}") };
        assert!(what.contains(&format!("{attempt} of {max}")), "{what}");
        assert!(what.contains("503"), "{what}");
        assert!(*due_at_ms > chrono::Utc::now().timestamp_millis());
        let wait = read_wait(&fx).unwrap();
        assert_eq!((wait.attempt, wait.exhausted()), (attempt, false));
    }
    // The last attempt: retries spent, the user is told.
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    let Outcome::Stopped { reason } = &outcome else { panic!("{outcome:?}") };
    assert!(reason.starts_with(crate::autopilot::local::FAILED_PREFIX) && reason.contains("503"), "{reason}");
    assert!(read_wait(&fx).unwrap().exhausted());
}

#[test]
fn a_look_that_reads_github_again_ends_the_recorded_failure() {
    let fx = bare();
    retire_outside_criteria(&fx);
    let github = FailingGithub::new("GitHub is unavailable (HTTP 503)");
    crate::pr_readiness::set_feed_override(Some(github.clone()));
    linked_to_pr(&fx, "approved");
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert!(matches!(outcome, Outcome::Waiting { .. }), "{outcome:?}");
    assert!(read_wait(&fx).is_some());

    crate::pr_readiness::set_feed_override(Some(Arc::new(FinishedGithub)));
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert_eq!(outcome, Outcome::Done, "{outcome:?}");
    assert!(read_wait(&fx).is_none(), "{:?}", pending_specs(&fx));
}

#[test]
fn a_wait_on_several_pull_requests_says_where_each_stands() {
    let fx = bare();
    retire_outside_criteria(&fx);
    let github = AfterReview::new();
    github.merged.store(true, std::sync::atomic::Ordering::SeqCst);
    // Only acme/app#42 is named in a release; acme/infra#7 never is.
    *github.notes.lock().unwrap() = Some("* Add the widget by @ann in #42".into());
    crate::pr_readiness::set_feed_override(Some(github.clone()));
    linked_to_prs(
        &fx,
        "merged",
        &["https://github.com/acme/app/pull/42", "https://github.com/acme/infra/pull/7"],
    );
    let mut agent = FakeAgent::new(&fx.fleet);
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    let Outcome::Waiting { what, .. } = outcome else {
        panic!("expected Waiting, got {outcome:?}");
    };
    assert!(what.contains("acme/app#42 released in v1.0.0"), "{what}");
    assert!(what.contains("acme/infra#7 merged, no release names it yet"), "{what}");

    // What the task panel reads back later, with no run in this app.
    let waits = fx.fleet.read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(fx.node)).unwrap();
    let (shown, _) = crate::runner_status::github_wait(&waits).expect("a scheduled look");
    assert_eq!(shown, what);
}

#[test]
fn a_repository_that_does_not_release_is_not_waited_on_for_one() {
    let fx = bare();
    retire_outside_criteria(&fx);
    let github = AfterReview::new();
    github.merged.store(true, std::sync::atomic::Ordering::SeqCst);
    crate::pr_readiness::set_feed_override(Some(github.clone()));
    let mut settings = tod_store::TodSettings::default();
    settings.lifecycle.repo_profiles = vec![tod_store::RepoProfileSetting {
        repo: "acme/infra".into(),
        profile: tod_store::RepoProfile::Merge,
    }];
    settings.save(&tod_store::TodPaths::at(&fx.root)).unwrap();
    linked_to_prs(
        &fx,
        "merged",
        &["https://github.com/acme/app/pull/42", "https://github.com/acme/infra/pull/7"],
    );
    let mut agent = FakeAgent::new(&fx.fleet);

    // The release for acme/app is still out: that alone holds the node.
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    let Outcome::Waiting { what, .. } = outcome else {
        panic!("expected Waiting, got {outcome:?}");
    };
    assert!(what.contains("acme/app#42"), "{what}");
    assert!(what.contains("acme/infra#7 merged (this repository does not release)"), "{what}");

    // Once it is released, the infra PR does not hold the node.
    *github.notes.lock().unwrap() = Some("* Add the widget by @ann in #42".into());
    let outcome = autopilot(&fx, Budget::default()).run(&fx.fleet, &mut agent).unwrap();
    assert_eq!(outcome, Outcome::Done, "{outcome:?}");
}

#[test]
fn check_now_records_where_each_pull_request_stands_with_no_run() {
    let fx = bare();
    retire_outside_criteria(&fx);
    let github = AfterReview::new();
    github.merged.store(true, std::sync::atomic::Ordering::SeqCst);
    crate::pr_readiness::set_feed_override(Some(github.clone()));
    linked_to_prs(
        &fx,
        "merged",
        &["https://github.com/acme/app/pull/42", "https://github.com/acme/infra/pull/7"],
    );

    let found = super::check_now(&fx.fleet, fx.node, &fx.root).unwrap();
    let super::GithubCheck::Waiting { what, .. } = found else {
        panic!("expected Waiting, got {found:?}");
    };
    assert!(what.contains("acme/app#42 merged, no release names it yet"), "{what}");
    assert_eq!(pending_specs(&fx), ["github:release:published"]);
    let waits = fx.fleet.read(|conn| tod_store::waits::WaitRepo::new(conn).list_pending_for_node(fx.node)).unwrap();
    assert_eq!(crate::runner_status::github_wait(&waits).unwrap().0, what);

    // Both released: nothing left, and the wait is gone.
    *github.notes.lock().unwrap() = Some("* Add the widget in #42\n* Apply the infra change in #7".into());
    let found = super::check_now(&fx.fleet, fx.node, &fx.root).unwrap();
    assert_eq!(found, super::GithubCheck::Clear);
    assert!(pending_specs(&fx).is_empty());
}

#[test]
fn check_now_in_pr_waits_for_the_draft_to_be_ready_and_reviewed() {
    use tod_store::github::{PrReview, PrSnapshot, PrStatus};
    struct Feed(std::sync::Mutex<PrSnapshot>);
    impl crate::pr_readiness::PrFeed for Feed {
        fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<PrSnapshot, String> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn comment(&self, _: &tod_store::github::NodePr, _: &str) -> Result<(), String> {
            Ok(())
        }
    }
    let fx = bare();
    retire_outside_criteria(&fx);
    let status = PrStatus {
        mergeable: Some(true),
        mergeable_state: Some("blocked".into()),
        merged: false,
        checks: None,
        head_sha: None,
        head_committed_at: None,
        draft: true,
        open: true,
        reviewers: Vec::new(),
        base_ref: None,
        body: None,
    };
    let feed = Arc::new(Feed(std::sync::Mutex::new(PrSnapshot {
        status,
        threads: vec![],
        comments: vec![],
        reviews: vec![],
    })));
    crate::pr_readiness::set_feed_override(Some(feed.clone()));
    linked_to_prs(&fx, "pr", &["https://github.com/acme/app/pull/42"]);

    let found = super::check_now(&fx.fleet, fx.node, &fx.root).unwrap();
    let super::GithubCheck::Waiting { what, .. } = found else { panic!("expected Waiting, got {found:?}") };
    assert!(what.contains("marked ready for review"), "{what}");
    assert_eq!(pending_specs(&fx), ["github:pr:42:ready_for_review"]);

    feed.0.lock().unwrap().status.draft = false;
    let found = super::check_now(&fx.fleet, fx.node, &fx.root).unwrap();
    let super::GithubCheck::Waiting { what, .. } = found else { panic!("expected Waiting, got {found:?}") };
    assert!(what.contains("reviewer"), "{what}");
    assert_eq!(pending_specs(&fx), ["github:pr:42:review_requested"]);

    feed.0.lock().unwrap().reviews.push(PrReview {
        author: Some("alice".into()),
        submitted_at: "2026-01-01T10:00:00Z".into(),
        state: "COMMENTED".into(),
    });
    assert_eq!(super::check_now(&fx.fleet, fx.node, &fx.root).unwrap(), super::GithubCheck::Clear);
    assert!(pending_specs(&fx).is_empty());
}

#[test]
fn check_now_in_pr_clears_a_leftover_review_wait_and_ignores_a_bot_reviewer() {
    use tod_store::github::{PrSnapshot, PrStatus};
    struct Feed(PrSnapshot);
    impl crate::pr_readiness::PrFeed for Feed {
        fn snapshot(&self, _: &tod_store::github::NodePr) -> Result<PrSnapshot, String> {
            Ok(self.0.clone())
        }
        fn comment(&self, _: &tod_store::github::NodePr, _: &str) -> Result<(), String> {
            Ok(())
        }
    }
    let fx = bare();
    retire_outside_criteria(&fx);
    let status = |reviewers: &[&str]| PrStatus {
        mergeable: Some(true),
        mergeable_state: Some("blocked".into()),
        merged: false,
        checks: None,
        head_sha: None,
        head_committed_at: None,
        draft: false,
        open: true,
        reviewers: reviewers.iter().map(|r| r.to_string()).collect(),
        base_ref: None,
        body: None,
    };
    let snapshot = |reviewers: &[&str]| {
        Arc::new(Feed(PrSnapshot { status: status(reviewers), threads: vec![], comments: vec![], reviews: vec![] }))
    };
    linked_to_prs(&fx, "pr", &["https://github.com/acme/app/pull/42"]);

    // Only a bot is asked: no reviewer yet.
    crate::pr_readiness::set_feed_override(Some(snapshot(&["greptile-apps[bot]"])));
    let found = super::check_now(&fx.fleet, fx.node, &fx.root).unwrap();
    assert!(matches!(found, super::GithubCheck::Waiting { .. }), "{found:?}");

    // A wait from the review babysitter is still open when a person is put on it.
    fx.fleet
        .interview(
            "test",
            InterviewCommand::RecordWait {
                node_id: fx.node,
                wait: tod_store::waits::NewWait::event("github:pr:42:review".to_string(), i64::MAX / 2),
            },
        )
        .unwrap();
    crate::pr_readiness::set_feed_override(Some(snapshot(&["alice"])));
    assert_eq!(super::check_now(&fx.fleet, fx.node, &fx.root).unwrap(), super::GithubCheck::Clear);
    assert!(pending_specs(&fx).is_empty(), "{:?}", pending_specs(&fx));
}
