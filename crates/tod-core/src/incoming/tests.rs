use super::*;
use crate::conversation::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV};
use crate::conversation::mock::Direct;
use crate::interview::test_support::{Fixture, fixture};
use std::collections::HashMap;
use std::path::PathBuf;
use tod_agent::agent_traffic::InterviewAgentCounts;
use tod_agent::{
    AgentLaunchOptions, AgentPlatform, AgentProvider, AgentRunHandle, AgentRunState, RunId, SessionTurn,
};
use tod_store::outline::{Capability, CreatePosition, KIND_CONSTRAINT, OutlineMutation};
use tod_store::settings::InterviewContextSettings;

/// Plays the evaluation agent with the mock, synchronously. With `silent`
/// it replies without recording a verdict.
struct FakeAgent {
    fleet: Arc<FleetStore>,
    runs: HashMap<RunId, AgentRunState>,
    turns: Vec<SessionTurn>,
    closed: Vec<String>,
    silent: bool,
}

use std::sync::Arc;

impl FakeAgent {
    fn new(fx: &Fixture) -> Self {
        Self {
            fleet: fx.fleet.clone(),
            runs: HashMap::new(),
            turns: Vec::new(),
            closed: Vec::new(),
            silent: false,
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
        let env = |name: &str| {
            turn.env
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        let state = if self.silent {
            AgentRunState::Success(Some("Looked, decided nothing.".into()))
        } else {
            let client = Direct {
                fleet: &self.fleet,
                actor: ACTOR_USER.to_string(),
            };
            let result = crate::conversation::incoming::mock_turn(
                &client,
                env(IMPLEMENT_NODE_ENV).unwrap().parse().unwrap(),
                env(IMPLEMENT_CONVERSATION_ENV).unwrap().parse().unwrap(),
                crate::conversation::incoming::parse_action_ids(
                    env(INCOMING_ACTIONS_ENV).as_deref(),
                )
                .unwrap(),
                &turn.prompt_blocks().join("\n\n"),
            );
            match result {
                Ok(text) => AgentRunState::Success(Some(text)),
                Err(err) => AgentRunState::Failure(format!("{err:#}")),
            }
        };
        self.runs.insert(id, state);
        self.turns.push(turn);
        Ok(AgentRunHandle { id })
    }

    fn session_id(&self, _: &str) -> Option<String> {
        None
    }

    fn fleet_run_session_id(&self, _: RunId) -> Option<String> {
        None
    }

    fn session_context_chars(&self, _: &str) -> Option<u64> {
        None
    }

    fn session_reply_parts(&self, _: &str) -> Option<Vec<tod_agent::ReplyPart>> {
        None
    }

    fn close_session(&mut self, key: &str) {
        self.closed.push(key.to_string());
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
        context: InterviewContextSettings::default(),
    }
}

fn outline(fx: &Fixture, mutation: OutlineMutation) {
    fx.user(InterviewCommand::Outline {
        mutation,
        target: None,
    });
}

/// A Spec child of the fixture's node, in `active`, titled `title` with
/// `details`.
fn child(fx: &Fixture, title: &str, details: &str) -> Uuid {
    let list_id = fx.fleet.list_outline_lists().unwrap()[0].id;
    let id = Uuid::new_v4();
    fx.fleet
        .enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(id),
            list_id,
            parent_id: Some(fx.node),
            anchor_id: None,
            position: CreatePosition::Below,
            title: title.into(),
        })
        .unwrap();
    outline(
        fx,
        OutlineMutation::EnableCapabilities {
            node_id: id,
            capabilities: vec![Capability::Spec],
        },
    );
    if !details.is_empty() {
        fx.fleet
            .enqueue_outline(OutlineMutation::SetExtraContent {
                node_id: id,
                content_type: EXTRA_CONTENT_DETAILS.into(),
                body: details.into(),
            })
            .unwrap();
    }
    for state in ["ready", "active"] {
        fx.fleet
            .enqueue_outline(OutlineMutation::SetLifecycle {
                node_id: id,
                state: state.into(),
            })
            .unwrap();
    }
    id
}

/// Add a constraint on the fixture's node; returns its id.
fn constraint(fx: &Fixture, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    outline(
        fx,
        OutlineMutation::CreateObligation {
            obligation_id: Some(id),
            node_id: fx.node,
            kind: KIND_CONSTRAINT.into(),
            after_id: None,
            before: false,
            section: None,
            body: body.into(),
            phase: tod_store::interview::PHASE_REQUIREMENTS.into(),
        },
    );
    id
}

fn pending(fx: &Fixture, node: Uuid) -> usize {
    fx.fleet
        .read(|conn| IncomingRepo::new(conn).pending(node))
        .unwrap()
        .len()
}

fn run(fx: &Fixture, agent: &mut FakeAgent, cap: usize, nodes: Vec<Uuid>) -> IncomingRunner {
    let mut runner = IncomingRunner::new(config(fx), cap, nodes);
    for _ in 0..20 {
        runner.tick(&fx.fleet, agent);
        assert!(runner.running_nodes().len() <= cap);
        if runner.is_done() {
            break;
        }
    }
    assert!(runner.is_done());
    runner
}

fn outcome(runner: &IncomingRunner, node: Uuid) -> NodeOutcome {
    runner
        .results()
        .iter()
        .find(|r| r.node == node)
        .unwrap()
        .outcome
        .clone()
}

#[test]
fn each_node_gets_its_own_session_and_its_verdict() {
    let fx = fixture();
    let quiet = child(&fx, "Quiet", "");
    let plan = child(&fx, "Planned", "affects plan: the dialog step has no Escape");
    let obligations = child(&fx, "Obliged", "affects obligations: needs an Escape requirement");
    constraint(&fx, "All dialogs close on Escape");
    for node in [quiet, plan, obligations] {
        assert_eq!(pending(&fx, node), 1);
    }

    let mut agent = FakeAgent::new(&fx);
    let runner = run(&fx, &mut agent, 2, vec![quiet, plan, obligations]);

    assert_eq!(agent.turns.len(), 3, "one session per node");
    let keys: std::collections::HashSet<_> = agent.turns.iter().map(|t| t.key.clone()).collect();
    assert_eq!(keys.len(), 3);
    assert_eq!(agent.closed.len(), 3, "every session is closed when it ends");
    // The context is the node's own, and the changes: no ancestors.
    let context = agent.turns[0].opening.as_ref().unwrap().context.clone().unwrap();
    assert!(context.contains("This surface: incoming-changes check"), "{context}");
    assert!(context.contains("## Incoming changes"), "{context}");
    assert!(context.contains("All dialogs close on Escape"), "{context}");
    assert!(context.contains("\"Interview node\""), "{context}");
    assert!(!context.contains("Inherited context"), "{context}");

    assert!(matches!(
        outcome(&runner, quiet),
        NodeOutcome::Verdict { ref affects, target: None, .. } if affects == "none"
    ));
    assert!(matches!(
        outcome(&runner, plan),
        NodeOutcome::Verdict { target: Some("planning"), ref note, .. }
            if note == "the dialog step has no Escape"
    ));
    assert!(matches!(
        outcome(&runner, obligations),
        NodeOutcome::Verdict { target: Some("design"), .. }
    ));
    for node in [quiet, plan, obligations] {
        assert_eq!(pending(&fx, node), 0);
    }
    // Regression picks the verdicts up.
    let found = fx
        .fleet
        .read(|conn| crate::lifecycle_validity::regression(conn, obligations))
        .unwrap()
        .unwrap();
    assert_eq!(found.target, "design");
    assert!(found.reasons[0].contains("All dialogs close on Escape"), "{:?}", found.reasons);
}

#[test]
fn changes_that_net_to_nothing_are_cleared_without_an_agent() {
    let fx = fixture();
    let node = child(&fx, "Quiet", "");
    let id = constraint(&fx, "Temporary");
    outline(&fx, OutlineMutation::DeleteObligation { obligation_id: id });
    assert_eq!(pending(&fx, node), 2);

    let mut agent = FakeAgent::new(&fx);
    let runner = run(&fx, &mut agent, 4, vec![node]);
    assert!(agent.turns.is_empty());
    assert_eq!(outcome(&runner, node), NodeOutcome::Cleared);
    assert_eq!(pending(&fx, node), 0);
}

#[test]
fn a_session_that_records_no_verdict_leaves_the_changes_pending() {
    let fx = fixture();
    let node = child(&fx, "Quiet", "");
    constraint(&fx, "All dialogs close on Escape");

    let mut agent = FakeAgent::new(&fx);
    agent.silent = true;
    let runner = run(&fx, &mut agent, 4, vec![node]);
    assert!(matches!(outcome(&runner, node), NodeOutcome::Failed(_)));
    assert_eq!(pending(&fx, node), 1);
}

/// What a gate check waiting on `node` would do after its check.
fn before_gate(fx: &Fixture, agent: &mut FakeAgent, node: Uuid) -> BeforeGate {
    assert!(fx
        .fleet
        .read(|conn| needs_check_before_gate(conn, node))
        .unwrap());
    let runner = run(fx, agent, 1, vec![node]);
    BeforeGate::from_outcome(&outcome(&runner, node))
}

#[test]
fn a_gate_check_proceeds_when_nothing_inherited_affects_the_node() {
    let fx = fixture();
    let node = child(&fx, "Quiet", "");
    constraint(&fx, "All dialogs close on Escape");
    let mut agent = FakeAgent::new(&fx);
    let gate = before_gate(&fx, &mut agent, node);
    assert_eq!(gate, BeforeGate::Proceed);
    assert_eq!(gate.report(), None);
    assert_eq!(pending(&fx, node), 0);
    assert!(!fx.fleet.read(|conn| needs_check_before_gate(conn, node)).unwrap());
    assert!(fx
        .fleet
        .read(|conn| crate::lifecycle_validity::regression(conn, node))
        .unwrap()
        .is_none());
}

#[test]
fn a_gate_check_reports_the_regression_when_a_change_affects_the_plan() {
    let fx = fixture();
    let node = child(&fx, "Planned", "affects plan: the dialog step has no Escape");
    constraint(&fx, "All dialogs close on Escape");
    let mut agent = FakeAgent::new(&fx);
    let gate = before_gate(&fx, &mut agent, node);
    assert!(matches!(
        gate,
        BeforeGate::Regressed { target: "planning", ref affects, .. } if affects == "plan"
    ));
    let report = gate.report().unwrap();
    assert!(report.contains("affects its plan"), "{report}");
    assert!(report.contains("planning"), "{report}");
    let found = fx
        .fleet
        .read(|conn| crate::lifecycle_validity::regression(conn, node))
        .unwrap()
        .unwrap();
    assert_eq!(found.target, "planning");
    assert_eq!(found.incoming.as_deref(), Some("plan"));
    assert!(!found.own);
    assert!(found.explanation().starts_with("A change it inherits affects its plan"));
}

#[test]
fn a_gate_check_proceeds_after_changes_that_net_to_nothing_are_cleared() {
    let fx = fixture();
    let node = child(&fx, "Quiet", "");
    let id = constraint(&fx, "Temporary");
    outline(&fx, OutlineMutation::DeleteObligation { obligation_id: id });
    let mut agent = FakeAgent::new(&fx);
    assert_eq!(before_gate(&fx, &mut agent, node), BeforeGate::Proceed);
    assert!(agent.turns.is_empty());
    assert_eq!(pending(&fx, node), 0);
}

#[test]
fn a_gate_check_fails_when_the_check_records_no_verdict() {
    let fx = fixture();
    let node = child(&fx, "Quiet", "");
    constraint(&fx, "All dialogs close on Escape");
    let mut agent = FakeAgent::new(&fx);
    agent.silent = true;
    let gate = before_gate(&fx, &mut agent, node);
    assert!(matches!(gate, BeforeGate::Failed(_)));
    assert!(gate.report().unwrap().contains("failed"));
}

#[test]
fn nodes_before_ready_are_not_checked_before_a_gate() {
    let fx = fixture();
    constraint(&fx, "All dialogs close on Escape");
    assert!(!fx
        .fleet
        .read(|conn| needs_check_before_gate(conn, fx.node))
        .unwrap());
}
