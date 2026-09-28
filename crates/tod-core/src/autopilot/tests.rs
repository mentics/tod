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
}

impl FakeAgent {
    fn new(fleet: &Arc<FleetStore>) -> Self {
        Self {
            fleet: fleet.clone(),
            runs: HashMap::new(),
            sessions: HashMap::new(),
            turns: 0,
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

/// A node with a workspace and nothing else: no requirements, no plan.
fn bare() -> Fixture {
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
    fx.fleet
        .enqueue(tod_store::fleet::FleetMutation::UpdateTaskRepo {
            id: fx.node.to_string(),
            repo: Some(workspace.display().to_string()),
        })
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
         WHERE slug = ?1 OR from_state IN ('pr', 'approved')",
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
