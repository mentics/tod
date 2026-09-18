use super::context::{
    DELTA_HEADING, RESUME_HEADING, ReportedStale, delta, focus_selection, opening,
};
use super::driver::*;
use super::implement::{IMPLEMENT_CONVERSATION_ENV, IMPLEMENT_NODE_ENV, TestRun};
use super::mock::{Direct, reply};
use crate::interview::test_support::{Fixture, fixture};
use crate::media::MediaPaths;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tod_agent::agent_traffic::InterviewAgentCounts;
use tod_agent::{
    AgentLaunchOptions, AgentPlatform, AgentProvider, AgentRunHandle, AgentRunState, RunId,
    SessionPurpose, SessionTurn,
};
use tod_store::conversation::ProtocolKind;
use tod_store::conversation::{
    ActionActor, ActionKind, ConversationRepo, Focus, NetOp, ReverseOutcome, TurnRole, actor_for,
    net_changes,
};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_ENV, ACTOR_USER, InterviewCommand, short_id};
use tod_store::outline::OutlineMutation;
use tod_store::outline::repos::{NodeRepo, ObligationRepo};
use tod_store::settings::InterviewContextSettings;
use uuid::Uuid;

/// Plays the mock conversation agent synchronously against the fixture's
/// open store; every run finishes at once.
struct FakeAgent {
    fleet: Arc<FleetStore>,
    turns: Vec<SessionTurn>,
    runs: HashMap<RunId, AgentRunState>,
    sessions: HashMap<String, String>,
    chars: HashMap<String, u64>,
    parts: HashMap<String, Vec<tod_agent::ReplyPart>>,
    /// Refuse to resume a recorded session (as after it expired).
    fail_resume: bool,
}

impl FakeAgent {
    fn new(fleet: &Arc<FleetStore>) -> Self {
        Self {
            fleet: fleet.clone(),
            turns: Vec::new(),
            runs: HashMap::new(),
            sessions: HashMap::new(),
            chars: HashMap::new(),
            parts: HashMap::new(),
            fail_resume: false,
        }
    }

    fn last(&self) -> &SessionTurn {
        self.turns.last().expect("a turn was sent")
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
    ) -> anyhow::Result<AgentRunHandle> {
        anyhow::bail!("not used")
    }

    fn send_session_turn(&mut self, turn: SessionTurn) -> anyhow::Result<AgentRunHandle> {
        let id = RunId::new();
        let state = if self.fail_resume
            && turn.resume_session_id.is_some()
            && !self.sessions.contains_key(&turn.key)
        {
            AgentRunState::Failure("no conversation found with that session id".into())
        } else {
            let session = turn
                .resume_session_id
                .clone()
                .unwrap_or_else(|| format!("agent-side-{}", Uuid::new_v4()));
            self.sessions.entry(turn.key.clone()).or_insert(session);
            let blocks = turn.prompt_blocks();
            *self.chars.entry(turn.key.clone()).or_default() +=
                blocks.iter().map(|b| b.len() as u64).sum::<u64>();
            let env = |name: &str| {
                turn.env
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.clone())
            };
            // An implementation turn carries its node and conversation, not
            // a conversation actor.
            let implementing = env(IMPLEMENT_NODE_ENV).zip(env(IMPLEMENT_CONVERSATION_ENV));
            let client = Direct {
                fleet: &self.fleet,
                actor: env(ACTOR_ENV).unwrap_or_else(|| ACTOR_USER.to_string()),
            };
            let reply = match implementing {
                Some((node, conversation)) => super::implement::mock_turn(
                    &client,
                    node.parse().unwrap(),
                    conversation.parse().unwrap(),
                )
                .map(|text| tod_agent::MockReply::from(text)),
                None => reply(&client, &blocks),
            };
            match reply {
                Ok(reply) => {
                    self.parts
                        .insert(turn.key.clone(), reply.parts.unwrap_or_default());
                    AgentRunState::Success(Some(reply.text))
                }
                Err(err) => AgentRunState::Failure(format!("{err:#}")),
            }
        };
        self.runs.insert(id, state);
        self.turns.push(turn);
        Ok(AgentRunHandle { id })
    }

    fn session_id(&self, key: &str) -> Option<String> {
        self.sessions.get(key).cloned()
    }

    fn fleet_run_session_id(&self, _: RunId) -> Option<String> {
        None
    }


    fn session_context_chars(&self, key: &str) -> Option<u64> {
        self.chars.get(key).copied()
    }

    fn session_reply_parts(&self, key: &str) -> Option<Vec<tod_agent::ReplyPart>> {
        self.parts.get(key).cloned()
    }

    fn close_session(&mut self, key: &str) {
        self.sessions.remove(key);
        self.chars.remove(key);
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

fn media() -> MediaPaths {
    MediaPaths::from_media_root(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("tod")
            .join("media"),
    )
    .unwrap()
}

fn config(fx: &Fixture, budget: u64) -> ConversationConfig {
    ConversationConfig {
        data_root: fx.root.clone(),
        media: media(),
        launch: AgentLaunchOptions::for_platform(AgentPlatform::Claude),
        context: InterviewContextSettings {
            context_budget_tokens: budget,
            ..Default::default()
        },
    }
}

fn slug(fx: &Fixture) -> String {
    fx.fleet
        .read(|conn| NodeRepo::new(conn).get(fx.node))
        .unwrap()
        .unwrap()
        .slug
}

fn turns(fx: &Fixture, id: Uuid) -> Vec<(TurnRole, String)> {
    fx.fleet
        .read(|conn| ConversationRepo::new(conn).turns(id))
        .unwrap()
        .into_iter()
        .map(|t| (t.role, t.body))
        .collect()
}

/// Send `text` and collect the finished turn.
fn say(
    driver: &mut ConversationDriver,
    fx: &Fixture,
    agent: &mut FakeAgent,
    text: &str,
) -> Vec<ConversationEvent> {
    driver.send(&fx.fleet, agent, text).unwrap();
    assert!(driver.status().running);
    let mut events = driver.tick(&fx.fleet, agent);
    // A failed resume rotates and resends; collect that turn too.
    if events.contains(&ConversationEvent::Rotated) {
        events.extend(driver.tick(&fx.fleet, agent));
    }
    assert!(!driver.status().running);
    events
}

const DONE: ConversationEvent = ConversationEvent::TurnFinished { error: None };

#[test]
fn a_first_send_opens_the_session_and_records_actions_with_an_empty_reply() {
    let fx = fixture();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver = ConversationDriver::new(
        config(&fx, 100_000),
        Focus::Node(fx.node),
        ProtocolKind::Outline,
    );
    assert_eq!(
        driver.conversation_id(),
        None,
        "nothing is stored before a send"
    );

    let events = say(
        &mut driver,
        &fx,
        &mut agent,
        &format!("add obligation {}: Passwords are hashed.", slug(&fx)),
    );
    assert_eq!(events, [DONE]);
    let id = driver.conversation_id().unwrap();

    let turn = agent.last();
    assert_eq!(turn.purpose, SessionPurpose::Conversation);
    assert_eq!(turn.key, format!("conversation-{id}"));
    assert_eq!(turn.env, [(ACTOR_ENV.to_string(), actor_for(id))]);
    assert_eq!(turn.resume_session_id, None);
    assert!(
        turn.title.starts_with("Conversation · Interview node · "),
        "{}",
        turn.title
    );
    let opening = turn.opening.as_ref().unwrap();
    let context = opening.context.as_deref().unwrap();
    assert!(
        context.contains("This surface: the conversation view"),
        "{context}"
    );
    assert!(context.contains("## Focus"), "{context}");
    assert!(context.contains(&fx.node.to_string()), "{context}");
    assert!(context.contains("## `tod-cli changeset`"), "{context}");
    assert_eq!(
        turn.message,
        format!("add obligation {}: Passwords are hashed.", slug(&fx))
    );

    assert_eq!(
        turns(&fx, id),
        [
            (TurnRole::User, turn.message.clone()),
            (TurnRole::Agent, String::new()),
        ]
    );
    let (conversation, actions) = fx
        .fleet
        .read(|conn| {
            let repo = ConversationRepo::new(conn);
            Ok((repo.get(id)?.unwrap(), repo.actions(id)?))
        })
        .unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].actor, ActionActor::Agent);
    assert_eq!(actions[0].kind, ActionKind::Create);
    assert_eq!(actions[0].turn_seq, 1);
    assert_eq!(conversation.focus, Focus::Node(fx.node));
    assert_eq!(conversation.platform.as_deref(), Some("claude"));
    assert_eq!(conversation.agent_session_id, agent.session_id(&turn.key));
    // The name the session was given is the one recorded.
    assert_eq!(
        conversation.session_name.as_deref(),
        Some(turn.title.as_str())
    );
}

#[test]
fn after_a_reversal_the_next_send_carries_a_delta_and_resumes_the_session() {
    let fx = fixture();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver = ConversationDriver::new(
        config(&fx, 100_000),
        Focus::Node(fx.node),
        ProtocolKind::Outline,
    );
    say(
        &mut driver,
        &fx,
        &mut agent,
        &format!("add obligation {}: Sessions expire.", slug(&fx)),
    );
    let id = driver.conversation_id().unwrap();
    let session = agent.session_id(&format!("conversation-{id}")).unwrap();

    let change = fx
        .fleet
        .read(|conn| net_changes(conn, id))
        .unwrap()
        .remove(0);
    let outcome: ReverseOutcome =
        serde_json::from_value(fx.user(InterviewCommand::ReverseConversationActions {
            conversation_id: id,
            action_ids: change.action_ids.clone(),
            include_dependents: false,
            force: false,
        }))
        .unwrap();
    assert!(
        matches!(outcome, ReverseOutcome::Applied { .. }),
        "{outcome:?}"
    );

    let events = say(&mut driver, &fx, &mut agent, "ask Why did that go?");
    assert_eq!(events, [DONE]);
    let turn = agent.last();
    assert!(
        turn.opening.is_none(),
        "the session already has its context"
    );
    let name = fx
        .fleet
        .read(|conn| ConversationRepo::new(conn).get(id))
        .unwrap()
        .unwrap()
        .session_name
        .unwrap();
    assert_eq!(turn.title, name, "later turns keep the session's name");
    assert_eq!(turn.resume_session_id, None, "the live process is reused");
    assert!(turn.message.starts_with(DELTA_HEADING), "{}", turn.message);
    assert!(
        turn.message.contains(&format!(
            "The user reversed creating obligation {}",
            short_id(change.id)
        )),
        "{}",
        turn.message
    );
    assert!(
        turn.message.ends_with("# Message\n\nask Why did that go?"),
        "{}",
        turn.message
    );
    assert_eq!(agent.session_id(&turn.key), Some(session.clone()));
    assert_eq!(
        turns(&fx, id).last().unwrap(),
        &(TurnRole::Agent, "Why did that go?".to_string())
    );

    // Nothing new since: the next message goes out alone.
    say(&mut driver, &fx, &mut agent, "ask Still there?");
    assert_eq!(agent.last().message, "ask Still there?");

    // A later process has no live session: it resumes the recorded one.
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver = ConversationDriver::open(config(&fx, 100_000), &fx.fleet, id).unwrap();
    assert_eq!(driver.focus(), Focus::Node(fx.node));
    say(&mut driver, &fx, &mut agent, "ask Again?");
    let turn = agent.last();
    assert_eq!(turn.resume_session_id.as_deref(), Some(session.as_str()));
    assert!(turn.opening.is_none());
    assert_eq!(turn.message, "ask Again?");
}

#[test]
fn a_session_over_budget_rotates_to_a_snapshot() {
    let fx = fixture();
    let mut agent = FakeAgent::new(&fx.fleet);
    // Any opening already exceeds this budget.
    let mut driver =
        ConversationDriver::new(config(&fx, 1), Focus::Node(fx.node), ProtocolKind::Outline);
    say(
        &mut driver,
        &fx,
        &mut agent,
        &format!("add obligation {}: Logins are rate limited.", slug(&fx)),
    );
    let id = driver.conversation_id().unwrap();
    let key = format!("conversation-{id}");
    let first_session = agent.session_id(&key).unwrap();

    let events = say(&mut driver, &fx, &mut agent, "ask Anything else?");
    assert_eq!(events, [DONE]);
    let roles: Vec<TurnRole> = turns(&fx, id).into_iter().map(|(r, _)| r).collect();
    assert_eq!(
        roles,
        [
            TurnRole::User,
            TurnRole::Agent,
            TurnRole::User,
            TurnRole::Rotation,
            TurnRole::Agent,
        ]
    );
    assert_eq!(turns(&fx, id)[3].1, ROTATION_NOTE);
    let turn = agent.last();
    assert_eq!(turn.resume_session_id, None);
    assert_eq!(turn.message, "ask Anything else?");
    let context = turn.opening.as_ref().unwrap().context.as_deref().unwrap();
    assert!(
        context.contains("This surface: the conversation view"),
        "{context}"
    );
    assert!(
        context.contains("added obligation"),
        "the change set: {context}"
    );
    assert!(context.contains(RESUME_HEADING), "{context}");
    assert!(
        !context.contains("Anything else?"),
        "the message being sent is not repeated in the snapshot: {context}"
    );
    let second_session = agent.session_id(&key).unwrap();
    assert_ne!(first_session, second_session);
    let stored = fx
        .fleet
        .read(|conn| ConversationRepo::new(conn).get(id))
        .unwrap()
        .unwrap();
    assert_eq!(stored.agent_session_id, Some(second_session));
}

#[test]
fn a_session_that_cannot_be_resumed_rotates_and_resends() {
    let fx = fixture();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver =
        ConversationDriver::new(config(&fx, 100_000), Focus::Project, ProtocolKind::Outline);
    say(&mut driver, &fx, &mut agent, "ask Hello?");
    let id = driver.conversation_id().unwrap();

    let mut agent = FakeAgent::new(&fx.fleet);
    agent.fail_resume = true;
    let mut driver = ConversationDriver::open(config(&fx, 100_000), &fx.fleet, id).unwrap();
    let events = say(&mut driver, &fx, &mut agent, "ask Are you back?");
    assert_eq!(events, [ConversationEvent::Rotated, DONE]);
    assert_eq!(agent.turns.len(), 2);
    assert!(agent.turns[0].resume_session_id.is_some());
    assert_eq!(agent.turns[1].resume_session_id, None);
    assert!(agent.turns[1].opening.is_some());
    let roles: Vec<TurnRole> = turns(&fx, id).into_iter().map(|(r, _)| r).collect();
    assert_eq!(
        roles,
        [
            TurnRole::User,
            TurnRole::Agent,
            TurnRole::User,
            TurnRole::Rotation,
            TurnRole::Agent,
        ]
    );
    assert_eq!(turns(&fx, id)[4].1, "Are you back?");
    assert_eq!(driver.status().last_error, None);
}

#[test]
fn a_failed_turn_is_an_error_turn() {
    let fx = fixture();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver =
        ConversationDriver::new(config(&fx, 100_000), Focus::Project, ProtocolKind::Outline);
    say(&mut driver, &fx, &mut agent, "ask Hi?");
    let id = driver.conversation_id().unwrap();
    // Refuse the next run outright.
    agent.runs.clear();
    driver.send(&fx.fleet, &mut agent, "ask Again?").unwrap();
    let run = *agent.runs.keys().next().unwrap();
    agent
        .runs
        .insert(run, AgentRunState::Failure("rate limited".into()));
    let events = driver.tick(&fx.fleet, &mut agent);
    assert_eq!(
        events,
        [ConversationEvent::TurnFinished {
            error: Some("rate limited".into())
        }]
    );
    assert_eq!(
        turns(&fx, id).last().unwrap(),
        &(TurnRole::Error, "rate limited".to_string())
    );
    assert_eq!(driver.status().last_error.as_deref(), Some("rate limited"));
    assert!(
        driver.send(&fx.fleet, &mut agent, "  ").is_err(),
        "an empty message is refused"
    );
}

#[test]
fn an_item_changed_elsewhere_is_reported_once() {
    let fx = fixture();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver = ConversationDriver::new(
        config(&fx, 100_000),
        Focus::Node(fx.node),
        ProtocolKind::Outline,
    );
    say(
        &mut driver,
        &fx,
        &mut agent,
        &format!("add obligation {}: Tokens rotate.", slug(&fx)),
    );
    let obligation = fx
        .fleet
        .read(|conn| ObligationRepo::new(conn).list_for_node(fx.node))
        .unwrap()[0]
        .id;
    // Edited in the Tasks view, outside the conversation.
    fx.outline(OutlineMutation::UpdateObligationBody {
        obligation_id: obligation,
        body: "Tokens rotate daily.".into(),
    });

    say(&mut driver, &fx, &mut agent, "ask Ok?");
    let message = &agent.last().message;
    assert!(
        message.contains(&format!(
            "obligation {} on {} changed outside this conversation. It is now requirement \"Tokens rotate daily.\"",
            short_id(obligation),
            slug(&fx)
        )),
        "{message}"
    );
    say(&mut driver, &fx, &mut agent, "ask Ok again?");
    assert_eq!(agent.last().message, "ask Ok again?", "reported once");
}

#[test]
fn delta_lists_user_edits_and_is_empty_without_them() {
    let fx = fixture();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver = ConversationDriver::new(
        config(&fx, 100_000),
        Focus::Node(fx.node),
        ProtocolKind::Outline,
    );
    say(
        &mut driver,
        &fx,
        &mut agent,
        &format!("add obligation {}: Old wording.", slug(&fx)),
    );
    let id = driver.conversation_id().unwrap();
    let head = |fx: &Fixture| {
        fx.fleet
            .read(|conn| Ok(ConversationRepo::new(conn).actions(id)?.last().unwrap().id))
            .unwrap()
    };
    let since = head(&fx);
    let text = |fx: &Fixture, since| {
        fx.fleet
            .read(|conn| delta(conn, id, since, &mut ReportedStale::new()))
            .unwrap()
    };
    assert_eq!(text(&fx, since), "");

    let obligation = net_changes_first(&fx, id);
    fx.user(InterviewCommand::ConversationEdit {
        conversation_id: id,
        mutation: OutlineMutation::UpdateObligationBody {
            obligation_id: obligation,
            body: "New wording.".into(),
        },
    });
    let delta = text(&fx, since);
    assert!(
        delta.contains(&format!(
            "The user edited obligation {} on {}: was requirement \"Old wording.\"; now requirement \"New wording.\".",
            short_id(obligation),
            slug(&fx)
        )),
        "{delta}"
    );
    assert_eq!(text(&fx, head(&fx)), "");
}

fn net_changes_first(fx: &Fixture, id: Uuid) -> Uuid {
    fx.fleet.read(|conn| net_changes(conn, id)).unwrap()[0].id
}

#[test]
fn the_mock_carries_out_every_directive() {
    let fx = fixture();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver =
        ConversationDriver::new(config(&fx, 100_000), Focus::Project, ProtocolKind::Outline);
    let root = slug(&fx);
    say(
        &mut driver,
        &fx,
        &mut agent,
        &format!(
            "add node {root}: Child one\nadd node {root}: Child two\nadd obligation {root}: First.\nadd plan {root}: Build it."
        ),
    );
    let id = driver.conversation_id().unwrap();
    assert_eq!(turns(&fx, id).last().unwrap().1, "");
    let (child_one, child_two) = fx
        .fleet
        .read(|conn| {
            let nodes = NodeRepo::new(conn).list_all()?;
            let find = |t: &str| nodes.iter().find(|n| n.title == t).unwrap().slug.clone();
            Ok((find("Child one"), find("Child two")))
        })
        .unwrap();
    let child_one_id = fx
        .fleet
        .read(|conn| NodeRepo::new(conn).get_by_slug(&child_one))
        .unwrap()
        .unwrap()
        .id;
    // Obligations can only live on a node with the Spec capability.
    fx.outline(OutlineMutation::EnableCapabilities {
        node_id: child_one_id,
        capabilities: vec![tod_store::outline::Capability::Spec],
    });
    fx.fleet.writer().flush().unwrap();
    let changes = fx.fleet.read(|conn| net_changes(conn, id)).unwrap();
    assert_eq!(changes.len(), 4);
    assert!(changes.iter().all(|c| c.op == NetOp::Added));
    let obligation = short_id(
        changes
            .iter()
            .find(|c| c.entity == tod_store::conversation::Entity::Obligation)
            .unwrap()
            .id,
    );
    let step = short_id(
        changes
            .iter()
            .find(|c| c.entity == tod_store::conversation::Entity::PlanStep)
            .unwrap()
            .id,
    );

    say(
        &mut driver,
        &fx,
        &mut agent,
        &format!(
            "rename {obligation}: First, reworded.\nmove {obligation} under {child_one}\n\
             move {child_two} under {child_one}\nrename {child_one}: Child uno\n\
             flag {obligation}: Not sure it belongs here\ndelete {step}\n\
             ask Which one?\nmake it better\nrename nothing-here: x"
        ),
    );
    let reply = turns(&fx, id).last().unwrap().1.clone();
    // One markdown paragraph per note.
    let lines: Vec<&str> = reply.split("\n\n").collect();
    assert_eq!(lines.len(), 3, "{reply}");
    assert_eq!(lines[0], "Which one?");
    assert_eq!(lines[1], "Mock: I don't understand `make it better`.");
    assert!(
        lines[2].starts_with("Mock: could not do `rename nothing-here: x`"),
        "{reply}"
    );

    let changes = fx.fleet.read(|conn| net_changes(conn, id)).unwrap();
    let find = |entity| changes.iter().find(|c| c.entity == entity).unwrap();
    use tod_store::conversation::{Entity, EntitySnapshot};
    let ob = find(Entity::Obligation);
    assert_eq!(ob.op, NetOp::Added);
    assert_eq!(ob.flag.as_deref(), Some("Not sure it belongs here"));
    let Some(EntitySnapshot::Obligation { body, node_id, .. }) = &ob.current else {
        panic!("{ob:?}");
    };
    assert_eq!(body, "First, reworded.");
    let uno = fx
        .fleet
        .read(|conn| NodeRepo::new(conn).get_by_slug(&child_one))
        .unwrap()
        .unwrap();
    assert_eq!(*node_id, uno.id);
    assert_eq!(uno.title, "Child uno");
    assert!(
        changes.iter().all(|c| c.entity != Entity::PlanStep),
        "a step added then deleted is hidden: {changes:?}"
    );
}

#[test]
fn the_focus_block_describes_each_kind() {
    let fx = fixture();
    let obligation = fx.obligation("Every request is logged.");
    let media = media();
    let (project, node, item) = fx
        .fleet
        .read(|conn| {
            Ok((
                focus_selection(conn, Focus::Project)?,
                focus_selection(conn, Focus::Node(fx.node))?,
                focus_selection(
                    conn,
                    Focus::Obligation {
                        node: fx.node,
                        id: obligation,
                    },
                )?,
            ))
        })
        .unwrap();
    assert_eq!(project.sections[0].0, "Top-level nodes");
    assert!(project.sections[0].1[0].contains(&fx.node.to_string()));
    assert_eq!(node.title, "Interview node");
    assert!(node.sections[0].1[0].contains("Every request is logged."));
    assert_eq!(node.sections[1].1.len(), 0);
    assert_eq!(item.path, ["Interview node"]);
    assert_eq!(item.text.as_deref(), Some("Every request is logged."));

    // The opening renders it under the recipe's fragments.
    let id = Uuid::new_v4();
    fx.user(InterviewCommand::CreateConversation {
        id,
        focus: Focus::Obligation {
            node: fx.node,
            id: obligation,
        },
        protocol: ProtocolKind::Outline,
        platform: None,
        model: None,
        effort: None,
    });
    let text = fx
        .fleet
        .read(|conn| opening(conn, &media, &fx.root, id))
        .unwrap();
    let focus_at = text.find("## Focus").unwrap();
    assert!(text.find("This surface: the conversation view").unwrap() < focus_at);
    assert!(text[focus_at..].contains("**Kind:** obligation"), "{text}");
    assert!(
        text[focus_at..].contains("Every request is logged."),
        "{text}"
    );
    assert!(
        text.contains(&format!("`{}`", fx.root.display())),
        "data root: {text}"
    );
}

#[test]
fn a_reply_keeps_its_parts_and_its_body_is_the_answer() {
    let fx = fixture();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver = ConversationDriver::new(
        config(&fx, 100_000),
        Focus::Node(fx.node),
        ProtocolKind::Outline,
    );
    let message = format!(
        "think Which node?\nadd obligation {}: Passwords are hashed.\nask Anything else?",
        slug(&fx)
    );
    assert_eq!(say(&mut driver, &fx, &mut agent, &message), [DONE]);
    let id = driver.conversation_id().unwrap();

    let turns = fx
        .fleet
        .read(|conn| ConversationRepo::new(conn).turns(id))
        .unwrap();
    let reply = turns.last().unwrap();
    assert_eq!(reply.role, TurnRole::Agent);
    // The narration before the work is not the answer.
    assert_eq!(reply.body, "Anything else?");
    let kinds: Vec<&str> = reply
        .parts
        .iter()
        .map(|part| match part {
            tod_agent::ReplyPart::Text { .. } => "text",
            tod_agent::ReplyPart::Thought { .. } => "thought",
            tod_agent::ReplyPart::Tool { .. } => "tool",
        })
        .collect();
    assert_eq!(kinds, ["text", "thought", "tool", "text"]);
    // The user's turn has none.
    assert!(turns[0].parts.is_empty());
}

/// The implementation loop end to end: the mock closes one plan step a turn
/// and records a green test run, so a two-step plan takes one continuation.
/// Its replies are empty — the steps and the test run are the report — and
/// nothing is parsed out of them.
#[test]
fn an_implementation_loops_on_recorded_state_until_the_plan_is_done() {
    let fx = fixture();
    let workspace = fx.root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    fx.fleet
        .enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: fx.node,
            capabilities: vec![tod_store::outline::Capability::Files],
        })
        .unwrap();
    fx.fleet
        .enqueue(tod_store::fleet::FleetMutation::UpdateTaskRepo {
            id: fx.node.to_string(),
            repo: Some(workspace.display().to_string()),
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

    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver = ConversationDriver::new(
        config(&fx, 100_000),
        Focus::Node(fx.node),
        ProtocolKind::Implementation,
    );
    driver.send(&fx.fleet, &mut agent, "Implement the plan.").unwrap();
    let id = driver.conversation_id().unwrap();
    assert!(
        agent
            .last()
            .env
            .contains(&(IMPLEMENT_CONVERSATION_ENV.to_string(), id.to_string())),
        "the agent is told which conversation to record its tests against"
    );

    // One step closed and green tests: the other step is still open.
    assert_eq!(
        driver.tick(&fx.fleet, &mut agent),
        [ConversationEvent::Continued]
    );
    assert!(agent.last().message.contains("Step 1"), "{}", agent.last().message);
    // Both closed and green tests, recorded this turn: done.
    assert_eq!(driver.tick(&fx.fleet, &mut agent), [DONE]);

    assert_eq!(
        turns(&fx, id),
        [
            (TurnRole::User, "Implement the plan.".to_string()),
            (TurnRole::Agent, String::new()),
            (
                TurnRole::Continuation,
                "Asked the agent to finish the last open plan step".to_string()
            ),
            (TurnRole::Agent, String::new()),
        ]
    );
    let run = fx
        .fleet
        .read(|conn| ConversationRepo::new(conn).latest_report(id))
        .unwrap()
        .and_then(|value| TestRun::from_report(&value))
        .expect("a recorded test run");
    assert!(run.green(), "{run:?}");
}

/// The session's id is stored as soon as the agent reports it, so a turn
/// that never finishes still leaves the session to resume.
#[test]
fn the_session_id_is_stored_while_the_first_turn_runs() {
    let fx = fixture();
    let mut agent = FakeAgent::new(&fx.fleet);
    let mut driver =
        ConversationDriver::new(config(&fx, 100_000), Focus::Project, ProtocolKind::Outline);
    driver.send(&fx.fleet, &mut agent, "ask Hi?").unwrap();
    let run = *agent.runs.keys().next().unwrap();
    agent.runs.insert(run, AgentRunState::InFlight(None));
    assert!(driver.tick(&fx.fleet, &mut agent).is_empty());
    assert!(driver.status().running);

    let id = driver.conversation_id().unwrap();
    let conversation = fx
        .fleet
        .read(|conn| ConversationRepo::new(conn).get(id))
        .unwrap()
        .unwrap();
    let key = ConversationDriver::session_key(id);
    assert!(agent.session_id(&key).is_some());
    assert_eq!(conversation.agent_session_id, agent.session_id(&key));
    assert_eq!(
        conversation.session_name.as_deref(),
        Some(agent.last().title.as_str())
    );
}
