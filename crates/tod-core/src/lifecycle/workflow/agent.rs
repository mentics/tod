//! Conversation-backed actions. A durable dispatch owns one conversation, and
//! only a finished, typed reply can become the action's recorded output.

use super::{
    Revision, Value, ValueType,
    actions::PublishedAction,
    runner::{ActionOutput, ActionRequest, RunMetrics, Runner},
};
use crate::{
    conversation::driver::{
        AgentAccess, ConversationConfig, ConversationDriver, ConversationEvent, ConversationStatus,
    },
    conversation::workflow_context::{ContextUnavailable, SNAPSHOT_HEADING, WorkflowContext},
    permission,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tod_agent::{AgentLaunchOptions, PermissionRequest};
use tod_store::{
    activity::{KIND_RUN, with_lease},
    conversation::{ConversationRepo, Turn, TurnRole},
    decisions::{DecisionRepo, NewDecision, REASON_ACCESS},
    fleet::FleetStore,
    interview::{ACTOR_USER, InterviewCommand},
    workflow_run::{
        LAUNCH_PAUSE, Mutation, PERMISSION_PAUSE, WORKTREE_PAUSE, agent_conversation_id,
    },
};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Complete(ActionOutput),
    Waiting { reason: String },
    Stopped { reason: String },
}

#[derive(Debug, thiserror::Error)]
#[error("{0:#}")]
struct LaunchDeferred(anyhow::Error);

#[allow(clippy::too_many_arguments)]
pub fn run<A: AgentAccess + ?Sized>(
    fleet: &FleetStore,
    agent: &mut A,
    config: &ConversationConfig,
    request: &ActionRequest,
    action: &PublishedAction,
    outputs: &BTreeMap<String, ValueType>,
    poll: Duration,
    before_launch: impl FnOnce() -> Result<()>,
    watch: impl FnMut(Uuid, &ConversationStatus) -> Option<String>,
) -> Result<Outcome> {
    // Publish activity before verifying the current visit. Migration checks
    // this lease, including the window before ConversationDriver starts a turn.
    with_lease(fleet, request.task, KIND_RUN, "workflow action", || {
        run_claimed(
            fleet,
            agent,
            config,
            request,
            action,
            outputs,
            poll,
            before_launch,
            watch,
        )
    })?
}

#[allow(clippy::too_many_arguments)]
fn run_claimed<A: AgentAccess + ?Sized>(
    fleet: &FleetStore,
    agent: &mut A,
    config: &ConversationConfig,
    request: &ActionRequest,
    action: &PublishedAction,
    outputs: &BTreeMap<String, ValueType>,
    poll: Duration,
    before_launch: impl FnOnce() -> Result<()>,
    mut watch: impl FnMut(Uuid, &ConversationStatus) -> Option<String>,
) -> Result<Outcome> {
    ensure!(
        request.action == action.revision,
        "Action revision disagrees with the workflow dispatch."
    );
    let runner = Runner::new(fleet);
    let run = runner
        .get(request.run)?
        .context("Workflow run no longer exists.")?;
    let visit = runner.current_visit(&run)?;
    ensure!(
        !run.status.is_terminal()
            && run.task == request.task
            && visit.number == request.visit
            && visit.dispatch.as_ref() == Some(request),
        "This workflow action is no longer current."
    );
    if let Some(output) = visit.output {
        return Ok(Outcome::Complete(output));
    }
    let id = agent_conversation_id(request.run, request.visit);
    let turns = fleet.read(|conn| ConversationRepo::new(conn).turns(id))?;
    if let Some(turn) = turns.last().filter(|turn| turn.role == TurnRole::Agent) {
        return collect_output(fleet, id, &turns, turn, action, outputs).map(Outcome::Complete);
    }
    let (resume_after, resume_session) = match turns.last() {
        Some(turn) if turn.role == TurnRole::Error && turn.body.starts_with(PERMISSION_PAUSE) => {
            let title = turn.body.trim_start_matches(PERMISSION_PAUSE);
            if matches!(
                permission::verdict(
                    &fleet.read(|conn| permission::load(conn, request.task))?,
                    title
                ),
                permission::Verdict::Ask
            ) {
                return Ok(Outcome::Waiting {
                    reason: format!("Waiting for permission to {title}."),
                });
            }
            let resumable = fleet
                .read(|conn| ConversationRepo::new(conn).get(id))?
                .is_some_and(|row| row.agent_session_id.is_some());
            if !resumable {
                return Ok(Outcome::Waiting {
                    reason: format!(
                        "Permission was answered, but session {id} has no saved provider identity. Inspect the conversation before retrying the action."
                    ),
                });
            }
            (Some(turn.seq), true)
        }
        Some(turn)
            if turn.role == TurnRole::Error
                && (turn.body.starts_with(WORKTREE_PAUSE)
                    || turn.body.starts_with(LAUNCH_PAUSE)) =>
        {
            let continuation = turns.iter().any(|turn| {
                turn.role == TurnRole::Error && turn.body.starts_with(PERMISSION_PAUSE)
            });
            (Some(turn.seq), continuation)
        }
        Some(turn) if turn.role == TurnRole::Error => {
            return Ok(Outcome::Waiting {
                reason: format!(
                    "Workflow action conversation {id} ended without a result. {}",
                    turn.body
                ),
            });
        }
        Some(_) => {
            return Ok(Outcome::Waiting {
                reason: format!(
                    "Workflow action conversation {id} has an unfinished turn. Inspect it before retrying the action."
                ),
            });
        }
        None => (None, false),
    };
    let mut launch = AgentLaunchOptions::for_platform(action.spec.provider);
    if let Some(model) = &action.spec.model {
        launch.model.clone_from(model);
    }
    if let Some(effort) = &action.spec.effort {
        launch.effort.clone_from(effort);
    }
    let claimed = fleet.interview(
        ACTOR_USER,
        InterviewCommand::WorkflowRun {
            mutation: Mutation::ClaimAgent {
                run: request.run,
                visit: request.visit,
                request: serde_json::to_string(request)?,
                conversation: id,
                launch: launch.clone(),
                resume_after,
            },
        },
    )?;
    if claimed["claimed"].as_bool() != Some(true) {
        return Ok(Outcome::Waiting {
            reason: format!(
                "Workflow action conversation {id} has already been started. Its result is still pending."
            ),
        });
    }

    let mut driver = ConversationDriver::open(config.clone(), fleet, id)?
        .with_launch_override(launch)
        .with_workflow_context(WorkflowContext {
            request: request.clone(),
            selectors: action.spec.runtime_context.clone(),
        });
    let message = prompt(action, outputs, resume_session)?;
    let mut before_launch = Some(before_launch);
    if let Err(error) = driver.send_with_prelaunch(fleet, agent, &message, Vec::new(), &mut || {
        if runner
            .get(request.run)?
            .is_none_or(|run| run.status.is_terminal() || run.visit != request.visit)
        {
            return Err(LaunchDeferred(anyhow::anyhow!(
                "The workflow action is no longer current."
            ))
            .into());
        }
        before_launch.take().map_or(Ok(()), |before_launch| {
            before_launch().map_err(|error| LaunchDeferred(error).into())
        })
    }) {
        if error.is::<LaunchDeferred>() {
            let reason = format!("{LAUNCH_PAUSE}{error:#}");
            pause(fleet, id, reason.clone())?;
            return Ok(Outcome::Stopped { reason });
        }
        if error.is::<ContextUnavailable>() {
            let reason = format!("{LAUNCH_PAUSE}{error:#}");
            pause(fleet, id, reason.clone())?;
            return Ok(Outcome::Waiting { reason });
        }
        if tod_store::fleet::worktree::is_lock_busy(&error) {
            // A busy worktree failed before sending this turn to the provider.
            let reason = format!("{WORKTREE_PAUSE}{error:#}");
            pause(fleet, id, reason.clone())?;
            return Ok(Outcome::Waiting { reason });
        }
        return Err(error);
    }
    let mut answered = None;
    loop {
        let status = driver.status();
        let active = runner
            .get(request.run)?
            .is_some_and(|run| !run.status.is_terminal() && run.visit == request.visit);
        if !active {
            driver.cancel(fleet, agent)?;
            return Ok(Outcome::Stopped {
                reason: "The workflow action is no longer current.".into(),
            });
        }
        if let Some(reason) = watch(id, &status) {
            driver.cancel(fleet, agent)?;
            return Ok(Outcome::Stopped { reason });
        }
        for event in driver.tick(fleet, agent) {
            match event {
                ConversationEvent::TurnFinished { error: Some(error) } => bail!("{error}"),
                ConversationEvent::TurnFinished { error: None } => {
                    let turns = fleet.read(|conn| ConversationRepo::new(conn).turns(id))?;
                    let reply = turns
                        .last()
                        .filter(|turn| turn.role == TurnRole::Agent)
                        .context("The workflow action finished without a conversation reply.")?;
                    agent.with(|provider| {
                        provider.close_session(&ConversationDriver::session_key(id))
                    });
                    return collect_output(fleet, id, &turns, reply, action, outputs)
                        .map(Outcome::Complete);
                }
                ConversationEvent::FilesBusy { message } => {
                    return Ok(Outcome::Waiting { reason: message });
                }
                ConversationEvent::Notice(notice) => {
                    tracing::warn!(conversation = %id, ?notice, "workflow action notice");
                }
                ConversationEvent::Continued | ConversationEvent::Rotated => {}
            }
        }
        match driver.status().permission {
            Some(permission) if answered.as_ref() != Some(&permission) => {
                if let Some(outcome) =
                    permission_wait(fleet, agent, request.task, id, &mut driver, &permission)?
                {
                    return Ok(outcome);
                }
                answered = Some(permission);
            }
            None => answered = None,
            _ => {}
        }
        std::thread::sleep(poll.max(Duration::from_millis(10)));
    }
}

fn permission_wait<A: AgentAccess + ?Sized>(
    fleet: &FleetStore,
    agent: &mut A,
    task: Uuid,
    id: Uuid,
    driver: &mut ConversationDriver,
    request: &PermissionRequest,
) -> Result<Option<Outcome>> {
    let saved = fleet.read(|conn| permission::load(conn, task))?;
    let verdict = permission::verdict(&saved, &request.title);
    let option = match verdict {
        permission::Verdict::Allow => permission::allow_option(request),
        permission::Verdict::Deny => permission::deny_option(request),
        permission::Verdict::Ask => None,
    };
    if let Some(option) = option {
        agent.with(|provider| provider.respond_to_permission(request.run, option))?;
        return Ok(None);
    }
    if verdict == permission::Verdict::Ask {
        let pending = fleet.read(|conn| DecisionRepo::new(conn).list_pending_for_node(task))?;
        if !permission::already_asked(&pending, &request.title) {
            fleet.interview(
                ACTOR_USER,
                InterviewCommand::AskDecision {
                    node_id: task,
                    conversation_id: Some(id),
                    protocol: Some(permission::PROTOCOL.into()),
                    decision: NewDecision {
                        question: permission::question(&request.title),
                        options: permission::options(&request.title),
                        reason: REASON_ACCESS.into(),
                        ..Default::default()
                    },
                },
            )?;
        }
    }
    driver.cancel(fleet, agent)?;
    if verdict == permission::Verdict::Ask {
        // The paused sequence is the only automatically retryable attempt.
        // Other interruptions leave uncertain effects for the operator to inspect.
        pause(fleet, id, format!("{PERMISSION_PAUSE}{}", request.title))?;
    }
    Ok(Some(Outcome::Waiting {
        reason: if verdict == permission::Verdict::Ask {
            format!("Waiting for permission to {}.", request.title)
        } else {
            format!(
                "The agent did not offer a way to apply the saved permission for {}.",
                request.title
            )
        },
    }))
}

fn pause(fleet: &FleetStore, id: Uuid, reason: String) -> Result<()> {
    fleet.interview(
        ACTOR_USER,
        InterviewCommand::AppendConversationTurn {
            conversation_id: id,
            role: TurnRole::Error,
            body: reason,
            parts: Vec::new(),
            sent_context: None,
            attachments: Vec::new(),
        },
    )?;
    Ok(())
}

fn prompt(
    action: &PublishedAction,
    outputs: &BTreeMap<String, ValueType>,
    resume: bool,
) -> Result<String> {
    let mut text = if resume {
        String::from(
            "Continue the existing workflow conversation. Inspect the work already done and carry out only unfinished work.\n\n",
        )
    } else {
        format!("# {}\n\n{}\n\n", action.spec.title, action.spec.prompt)
    };
    if !resume {
        for (kind, items) in [
            ("Skills", &action.spec.skills),
            ("Context", &action.spec.context),
        ] {
            if items.is_empty() {
                continue;
            }
            text.push_str(&format!("## {kind}\n\n"));
            for item in items {
                text.push_str(&format!("### {}\n\n{}\n\n", item.title, item.text));
            }
        }
    }
    text.push_str("Finish your visible reply with only a JSON object. Include each declared key with its stated type. Return an empty object if there are no declared outputs. The output schema is ");
    text.push_str(&serde_json::to_string(outputs)?);
    Ok(text)
}

fn values(reply: &str, outputs: &BTreeMap<String, ValueType>) -> Result<BTreeMap<String, Value>> {
    let reply = reply.trim();
    let json = reply
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .map_or(reply, str::trim);
    let values: BTreeMap<String, Value> =
        serde_json::from_str(json).context("The workflow agent reply must be a JSON object.")?;
    ensure!(
        values.len() == outputs.len(),
        "The workflow agent must return exactly the declared output fields."
    );
    for (field, value_type) in outputs {
        let value = values
            .get(field)
            .with_context(|| format!("Missing workflow agent output `{field}`."))?;
        ensure!(
            value.value_type() == *value_type,
            "Workflow agent output `{field}` has the wrong type."
        );
    }
    Ok(values)
}

fn collect_output(
    fleet: &FleetStore,
    id: Uuid,
    turns: &[Turn],
    reply: &Turn,
    action: &PublishedAction,
    outputs: &BTreeMap<String, ValueType>,
) -> Result<ActionOutput> {
    let values = values(&reply.body, outputs)
        .with_context(|| format!("Read workflow conversation {id} turn {}.", reply.seq))?;
    let mut seen = BTreeSet::new();
    let assets = [&action.revision]
        .into_iter()
        .chain(
            action
                .spec
                .skills
                .iter()
                .chain(&action.spec.context)
                .filter_map(|item| item.source.as_ref()),
        )
        .filter(|revision| seen.insert((revision.id.as_str(), revision.number)))
        .cloned()
        .collect::<Vec<Revision>>();
    let (totals, cost) = fleet.read(|conn| {
        Ok((
            tod_store::conversation_usage::conversation_totals(conn, id)?,
            tod_store::conversation_usage::conversation_cost(conn, id)?,
        ))
    })?;
    let elapsed = |start: i64, end: i64| end.saturating_sub(start).max(0) as u64;
    let wall_time = turns
        .first()
        .map_or(0, |first| elapsed(first.created_at, reply.created_at));
    let mut paused_at = None;
    let mut wait_ms: u64 = 0;
    let mut evidence = vec![format!("conversation/{id}/turn/{}", reply.seq)];
    for turn in turns {
        if turn.role == TurnRole::Error
            && (turn.body.starts_with(PERMISSION_PAUSE)
                || turn.body.starts_with(WORKTREE_PAUSE)
                || turn.body.starts_with(LAUNCH_PAUSE))
        {
            paused_at.get_or_insert(turn.created_at);
        } else if turn.role == TurnRole::User
            && let Some(start) = paused_at.take()
        {
            wait_ms = wait_ms.saturating_add(elapsed(start, turn.created_at));
        }
        if turn.role == TurnRole::User
            && turn
                .sent_context
                .as_ref()
                .is_some_and(|context| context.lines().any(|line| line == SNAPSHOT_HEADING))
        {
            evidence.push(format!("conversation/{id}/turn/{}/context", turn.seq));
        }
    }
    Ok(ActionOutput {
        values,
        evidence,
        metrics: RunMetrics {
            duration_ms: wall_time.saturating_sub(wait_ms),
            wait_ms,
            input_tokens: totals.counts.prompt(),
            output_tokens: totals.counts.output,
            cost_micros: cost.micros,
        },
        assets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        interview::test_support::{Fixture, fixture},
        lifecycle::workflow::{
            Definition, FORMAT_VERSION, Node, NodeKind, Route,
            actions::{ActionCatalog, AgentAction, Instruction, RuntimeContext},
            catalog::Catalog,
        },
        media::MediaPaths,
    };
    use std::path::PathBuf;
    use tod_agent::{
        AgentEnvironment, AgentPlatform, AgentProvider, AgentRunHandle, AgentRunState,
        PermissionOption, RunId, SessionTurn,
    };

    struct TestAgent {
        sent: Vec<SessionTurn>,
        permission: bool,
        run: Option<(RunId, AgentRunState)>,
        cancelled: usize,
        approvals: Vec<String>,
    }

    impl TestAgent {
        fn new(permission: bool) -> Self {
            Self {
                sent: Vec::new(),
                permission,
                run: None,
                cancelled: 0,
                approvals: Vec::new(),
            }
        }

        fn reply() -> AgentRunState {
            AgentRunState::Success(Some("{\"findings\":2}".into()))
        }
    }

    impl AgentProvider for TestAgent {
        fn start_fleet_agent(
            &mut self,
            _: &str,
            _: PathBuf,
            _: String,
            _: AgentLaunchOptions,
            _: String,
            _: AgentEnvironment,
        ) -> Result<AgentRunHandle> {
            bail!("expected a conversation")
        }

        fn send_session_turn(&mut self, turn: SessionTurn) -> Result<AgentRunHandle> {
            self.sent.push(turn);
            let id = RunId::new();
            let state = if self.permission {
                AgentRunState::NeedsPermission(PermissionRequest {
                    run: id,
                    title: "Read the review artifact".into(),
                    options: vec![PermissionOption {
                        id: "allow-once".into(),
                        label: "Allow once".into(),
                    }],
                })
            } else {
                Self::reply()
            };
            self.run = Some((id, state));
            Ok(AgentRunHandle { id })
        }

        fn session_id(&self, key: &str) -> Option<String> {
            self.sent
                .iter()
                .any(|turn| turn.key == key)
                .then(|| "workflow-test-session".into())
        }

        fn fleet_run_session_id(&self, _: RunId) -> Option<String> {
            None
        }
        fn session_context_chars(&self, _: &str) -> Option<u64> {
            None
        }
        fn close_session(&mut self, _: &str) {}

        fn poll_run(&mut self, id: RunId) -> Option<AgentRunState> {
            self.run
                .as_ref()
                .filter(|(found, _)| *found == id)
                .map(|(_, state)| state.clone())
        }

        fn respond_to_permission(&mut self, id: RunId, option: &str) -> Result<()> {
            self.approvals.push(option.into());
            self.run = Some((id, Self::reply()));
            Ok(())
        }

        fn cancel_run(&mut self, _: RunId) -> Result<()> {
            self.cancelled += 1;
            self.run = None;
            Ok(())
        }
    }

    fn config(fixture: &Fixture) -> ConversationConfig {
        ConversationConfig {
            data_root: fixture.root.clone(),
            media: MediaPaths::from_media_root(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tod/media"),
            )
            .unwrap(),
            launch: AgentLaunchOptions::for_platform(AgentPlatform::Cursor),
            settings_path: None,
            context: Default::default(),
        }
    }

    fn published(
        fixture: &Fixture,
    ) -> (ActionRequest, PublishedAction, BTreeMap<String, ValueType>) {
        publish_with_context(fixture, Vec::new(), false)
    }

    fn publish_with_context(
        fixture: &Fixture,
        runtime_context: Vec<RuntimeContext>,
        seed_output: bool,
    ) -> (ActionRequest, PublishedAction, BTreeMap<String, ValueType>) {
        let asset = ActionCatalog::new(&fixture.fleet)
            .publish(
                None,
                None,
                AgentAction {
                    title: "Review artifact".into(),
                    prompt: "Inspect the artifact and count findings.".into(),
                    skills: vec![Instruction {
                        title: "Test inputs".into(),
                        text: "Check the boundary cases.".into(),
                        source: Some(Revision {
                            id: "review-skill".into(),
                            number: 4,
                        }),
                    }],
                    context: Vec::new(),
                    runtime_context,
                    provider: AgentPlatform::Claude,
                    model: Some("sonnet".into()),
                    effort: None,
                },
            )
            .unwrap();
        let outputs = BTreeMap::from([("findings".into(), ValueType::Integer)]);
        let catalog = Catalog::new(&fixture.fleet);
        let mut definition = Definition {
            task_phases: BTreeMap::new(),
            adoption_points: BTreeMap::new(),
            format: FORMAT_VERSION,
            revision: Revision {
                id: String::new(),
                number: 0,
            },
            entry: "step".into(),
            nodes: BTreeMap::from([
                (
                    "step".into(),
                    Node {
                        label: "Review artifact".into(),
                        kind: NodeKind::Action {
                            action: asset.revision.clone(),
                        },
                        outputs: outputs.clone(),
                        routes: vec![Route {
                            id: "finished".into(),
                            to: "end".into(),
                            when: None,
                        }],
                    },
                ),
                (
                    "end".into(),
                    Node {
                        label: "End".into(),
                        kind: NodeKind::Finish,
                        outputs: BTreeMap::new(),
                        routes: Vec::new(),
                    },
                ),
            ]),
            fields: BTreeMap::new(),
        };
        if seed_output {
            definition.entry = "source".into();
            definition.nodes.insert(
                "source".into(),
                Node {
                    label: "Source".into(),
                    kind: NodeKind::Action {
                        action: Revision {
                            id: super::super::builtin::MANUAL.into(),
                            number: 1,
                        },
                    },
                    outputs: BTreeMap::from([
                        (String::from("report"), ValueType::Text),
                        (String::from("internal"), ValueType::Text),
                    ]),
                    routes: vec![Route {
                        id: "step".into(),
                        to: "step".into(),
                        when: None,
                    }],
                },
            );
        }
        let draft = catalog.create("Review", definition).unwrap();
        let workflow = catalog
            .publish(&draft.definition.revision.id, draft.generation)
            .unwrap();
        let pinned = catalog
            .action_for(&workflow.definition.revision, &asset.revision)
            .unwrap()
            .unwrap();
        let runner = Runner::new(&fixture.fleet);
        let run = runner
            .assign(fixture.node, &workflow.definition.revision, 10)
            .unwrap();
        let run = if seed_output {
            let request = runner.dispatch(&run).unwrap();
            runner
                .complete(
                    &request,
                    ActionOutput {
                        values: BTreeMap::from([
                            (
                                "report".into(),
                                Value::Text("Only the previous visit knows this".into()),
                            ),
                            (
                                "internal".into(),
                                Value::Text("Exclude this unselected field".into()),
                            ),
                        ]),
                        ..Default::default()
                    },
                )
                .unwrap();
            let run = runner.get(run.id).unwrap().unwrap();
            runner
                .evaluate(&run, super::super::Observations::new())
                .unwrap();
            runner.get(run.id).unwrap().unwrap()
        } else {
            run
        };
        (runner.dispatch(&run).unwrap(), pinned, outputs)
    }

    #[test]
    fn a_pinned_action_reuses_the_recorded_reply_if_completion_was_interrupted() {
        let fixture = fixture();
        let (request, asset, outputs) = published(&fixture);
        let config = config(&fixture);
        let mut provider = TestAgent::new(false);
        let first = run(
            &fixture.fleet,
            &mut provider,
            &config,
            &request,
            &asset,
            &outputs,
            Duration::ZERO,
            || Ok(()),
            |_, _| None,
        )
        .unwrap();
        let Outcome::Complete(output) = first else {
            panic!("{first:?}")
        };
        assert_eq!(output.values["findings"], Value::Integer(2));
        assert_eq!(
            output.assets,
            vec![
                asset.revision.clone(),
                Revision {
                    id: "review-skill".into(),
                    number: 4
                }
            ]
        );
        assert_eq!(provider.sent.len(), 1);
        assert_eq!(provider.sent[0].options.platform, AgentPlatform::Claude);
        assert_eq!(provider.sent[0].options.model, "sonnet");
        assert!(
            provider.sent[0]
                .message
                .contains("Check the boundary cases.")
        );
        assert!(
            provider.sent[0]
                .message
                .contains("\"findings\":\"integer\"")
        );

        let replay = run(
            &fixture.fleet,
            &mut provider,
            &config,
            &request,
            &asset,
            &outputs,
            Duration::ZERO,
            || Ok(()),
            |_, _| None,
        )
        .unwrap();
        assert_eq!(replay, Outcome::Complete(output.clone()));
        assert_eq!(provider.sent.len(), 1);
        Runner::new(&fixture.fleet)
            .complete(&request, output)
            .unwrap();
    }

    #[test]
    fn a_stop_does_not_repeat_an_uncertain_agent_turn() {
        let fixture = fixture();
        let (request, asset, outputs) = published(&fixture);
        let config = config(&fixture);
        let mut provider = TestAgent::new(false);
        assert_eq!(
            run(
                &fixture.fleet,
                &mut provider,
                &config,
                &request,
                &asset,
                &outputs,
                Duration::ZERO,
                || Ok(()),
                |_, _| Some("Stop requested".into())
            )
            .unwrap(),
            Outcome::Stopped {
                reason: "Stop requested".into()
            },
        );
        assert_eq!(provider.cancelled, 1);
        assert!(matches!(
            run(
                &fixture.fleet,
                &mut provider,
                &config,
                &request,
                &asset,
                &outputs,
                Duration::ZERO,
                || Ok(()),
                |_, _| None
            )
            .unwrap(),
            Outcome::Waiting { .. },
        ));
        assert_eq!(provider.sent.len(), 1);
    }

    #[test]
    fn an_answered_permission_resumes_the_same_conversation() {
        let fixture = fixture();
        let (request, asset, outputs) = published(&fixture);
        let config = config(&fixture);
        let mut provider = TestAgent::new(true);
        let execute = |provider: &mut TestAgent| {
            run(
                &fixture.fleet,
                provider,
                &config,
                &request,
                &asset,
                &outputs,
                Duration::ZERO,
                || Ok(()),
                |_, _| None,
            )
            .unwrap()
        };
        assert!(matches!(execute(&mut provider), Outcome::Waiting { .. }));
        assert!(matches!(execute(&mut provider), Outcome::Waiting { .. }));
        assert_eq!(provider.sent.len(), 1);
        let pending = fixture
            .fleet
            .read(|conn| DecisionRepo::new(conn).list_pending_for_node(fixture.node))
            .unwrap();
        assert_eq!(pending.len(), 1);
        fixture.user(InterviewCommand::AnswerDecision {
            decision_id: pending[0].id,
            option: Some(1),
            text: None,
        });

        assert!(matches!(execute(&mut provider), Outcome::Complete(..)));
        assert_eq!(provider.sent[0].key, provider.sent[1].key);
        assert!(provider.sent[1].message.contains("only unfinished work"));
        assert_eq!(provider.approvals, ["allow-once"]);
    }

    #[test]
    fn captures_the_configured_dynamic_context_on_the_delivered_turn() {
        let fixture = fixture();
        fixture.obligation("Render the live obligation on this task.");
        fixture.obligation_with_phase("Exclude this unselected design obligation.", "design");
        let (request, asset, outputs) = publish_with_context(
            &fixture,
            vec![
                RuntimeContext::Task,
                RuntimeContext::Obligations {
                    phase: Some("requirements".into()),
                },
                RuntimeContext::WorkingDirectory,
                RuntimeContext::NodeOutput {
                    node: "source".into(),
                    field: Some("report".into()),
                },
            ],
            true,
        );
        let mut provider = TestAgent::new(false);
        let mut launches = 0;
        let outcome = run(
            &fixture.fleet,
            &mut provider,
            &config(&fixture),
            &request,
            &asset,
            &outputs,
            Duration::ZERO,
            || {
                launches += 1;
                Ok(())
            },
            |_, _| None,
        )
        .unwrap();
        let Outcome::Complete(output) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(launches, 1);
        let context = provider.sent[0]
            .opening
            .as_ref()
            .unwrap()
            .context
            .as_ref()
            .unwrap();
        assert!(context.contains(SNAPSHOT_HEADING));
        assert!(context.contains("Render the live obligation on this task."));
        assert!(context.contains("Only the previous visit knows this"));
        assert!(!context.contains("Exclude this unselected design obligation."));
        assert!(!context.contains("Exclude this unselected field"));
        let id = agent_conversation_id(request.run, request.visit);
        let turns = fixture
            .fleet
            .read(|conn| ConversationRepo::new(conn).turns(id))
            .unwrap();
        let user = turns
            .iter()
            .find(|turn| turn.role == TurnRole::User)
            .unwrap();
        assert_eq!(user.sent_context.as_ref(), Some(context));
        let captures = crate::lifecycle::workflow::improvement::captured_context(
            &fixture.fleet,
            request.run,
            request.visit,
        )
        .unwrap();
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].conversation, id);
        assert_eq!(captures[0].turn, user.seq);
        assert_eq!(&captures[0].text, context);
        assert!(
            output
                .evidence
                .contains(&format!("conversation/{id}/turn/{}/context", user.seq))
        );
    }

    #[test]
    fn delivers_the_reviewers_lenses_and_durable_incoming_change_snapshot() {
        use tod_store::{
            incoming::IncomingRepo,
            outline::{Capability, CreatePosition, KIND_CONSTRAINT, OutlineMutation},
            review::{RISK_LENSES, Reviewer},
        };

        let fixture = fixture();
        let ancestor = Uuid::new_v4();
        let list_id = fixture.fleet.list_outline_lists().unwrap()[0].id;
        fixture.outline(OutlineMutation::CreateNode {
            node_id: Some(ancestor),
            list_id,
            parent_id: None,
            anchor_id: None,
            position: CreatePosition::Below,
            title: "Ancestor rules".into(),
        });
        fixture.user(InterviewCommand::Outline {
            mutation: OutlineMutation::EnableCapabilities {
                node_id: ancestor,
                capabilities: vec![Capability::Spec],
            },
        });
        fixture.outline(OutlineMutation::ReparentNode {
            node_id: fixture.node,
            parent_id: Some(ancestor),
            ordinal: 0,
        });
        fixture.outline(OutlineMutation::SetLifecycle {
            node_id: fixture.node,
            state: "ready".into(),
        });
        let obligation = Uuid::new_v4();
        fixture.user(InterviewCommand::Outline {
            mutation: OutlineMutation::CreateObligation {
                obligation_id: Some(obligation),
                node_id: ancestor,
                kind: KIND_CONSTRAINT.into(),
                after_id: None,
                before: false,
                section: None,
                body: "The inherited constraint applies to every simulated batch.".into(),
                phase: tod_store::interview::PHASE_REQUIREMENTS.into(),
            },
        });
        fixture.user(InterviewCommand::Outline {
            mutation: OutlineMutation::UpdateObligationBody {
                obligation_id: obligation,
                body: "The inherited constraint applies to every simulated batch and report."
                    .into(),
            },
        });
        let pending = || {
            fixture
                .fleet
                .read(|conn| IncomingRepo::new(conn).pending(fixture.node))
                .unwrap()
        };
        let before = pending();
        let ids = before
            .iter()
            .map(|entry| entry.action_id)
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 2);

        let (request, asset, outputs) = publish_with_context(
            &fixture,
            vec![
                RuntimeContext::IncomingChanges,
                RuntimeContext::ReviewerAssignment {
                    reviewer: Some(Reviewer::Risk),
                },
            ],
            false,
        );
        let mut provider = TestAgent::new(false);
        let output = run(
            &fixture.fleet,
            &mut provider,
            &config(&fixture),
            &request,
            &asset,
            &outputs,
            Duration::ZERO,
            || Ok(()),
            |_, _| None,
        )
        .unwrap();
        let Outcome::Complete(output) = output else {
            panic!("{output:?}")
        };
        let context = provider.sent[0]
            .opening
            .as_ref()
            .unwrap()
            .context
            .as_ref()
            .unwrap();
        assert!(context.contains("Ancestor rules"));
        assert!(context.contains("every simulated batch and report"));
        assert!(context.contains(&format!(
            "Incoming action IDs: `{}`",
            serde_json::to_string(&ids).unwrap()
        )));
        assert!(context.contains("**risk reviewer**"));
        for lens in Reviewer::Main.lenses() {
            assert_eq!(
                context.contains(&format!("(`{lens}`)")),
                RISK_LENSES.contains(&lens)
            );
        }
        assert_eq!(pending(), before);
        assert!(
            output
                .evidence
                .iter()
                .any(|entry| entry.ends_with("/context"))
        );
    }

    #[test]
    fn a_launch_deferred_by_the_budget_remains_safe_to_retry() {
        let fixture = fixture();
        let (request, asset, outputs) = published(&fixture);
        let config = config(&fixture);
        let mut provider = TestAgent::new(false);
        let execute = |provider: &mut TestAgent, available: bool| {
            run(
                &fixture.fleet,
                provider,
                &config,
                &request,
                &asset,
                &outputs,
                Duration::ZERO,
                || {
                    ensure!(available, "Session budget exhausted");
                    Ok(())
                },
                |_, _| None,
            )
            .unwrap()
        };
        assert!(
            matches!(execute(&mut provider, false), Outcome::Stopped { reason } if reason.contains("Session budget exhausted"))
        );
        assert!(provider.sent.is_empty());
        assert!(matches!(
            execute(&mut provider, true),
            Outcome::Complete(..)
        ));
        assert_eq!(provider.sent.len(), 1);
    }

    #[test]
    fn a_missing_dynamic_input_can_be_repaired_and_retried() {
        use tod_store::outline::{Capability, OutlineMutation};

        let fixture = fixture();
        fixture.outline(OutlineMutation::EnableCapabilities {
            node_id: fixture.node,
            capabilities: vec![Capability::LifecycleConfig],
        });
        fixture.fleet.writer().flush().unwrap();
        let phase = tod_store::lifecycle_config::REVIEW;
        fixture.outline(OutlineMutation::SetNodeLifecycleConfig {
            node_id: fixture.node,
            skills: BTreeMap::from([(
                phase.into(),
                vec!["missing-skill-for-workflow-context-test".into()],
            )]),
        });
        let (request, asset, outputs) = publish_with_context(
            &fixture,
            vec![RuntimeContext::ConfiguredSkills {
                phase: phase.into(),
                group: 0,
            }],
            false,
        );
        let config = config(&fixture);
        let mut provider = TestAgent::new(false);
        let mut launches = 0;
        let mut execute = |provider: &mut TestAgent| {
            run(
                &fixture.fleet,
                provider,
                &config,
                &request,
                &asset,
                &outputs,
                Duration::ZERO,
                || {
                    launches += 1;
                    Ok(())
                },
                |_, _| None,
            )
            .unwrap()
        };
        assert!(
            matches!(execute(&mut provider), Outcome::Waiting { reason } if reason.contains("resolve workflow context") && reason.contains("configured_skills"))
        );
        assert!(provider.sent.is_empty());

        fixture.outline(OutlineMutation::SetNodeLifecycleConfig {
            node_id: fixture.node,
            skills: BTreeMap::new(),
        });
        assert!(matches!(execute(&mut provider), Outcome::Complete(..)));
        assert_eq!(launches, 1);
        assert_eq!(provider.sent.len(), 1);
    }

    #[test]
    fn a_worktree_wait_retries_the_launch_without_repeating_an_agent_turn() {
        let fixture = fixture();
        let (request, asset, outputs) = published(&fixture);
        let config = config(&fixture);
        let mut provider = TestAgent::new(false);
        crate::conversation::driver::set_cwd_error(
            tod_store::fleet::worktree::WorktreeLockBusy {
                repo: "example".into(),
                holder: "other run".into(),
                waited: Duration::from_secs(1),
            }
            .into(),
        );
        let execute = |provider: &mut TestAgent| {
            run(
                &fixture.fleet,
                provider,
                &config,
                &request,
                &asset,
                &outputs,
                Duration::ZERO,
                || Ok(()),
                |_, _| None,
            )
            .unwrap()
        };
        assert!(
            matches!(execute(&mut provider), Outcome::Waiting { reason } if reason.contains("other run"))
        );
        assert!(provider.sent.is_empty());
        assert!(matches!(execute(&mut provider), Outcome::Complete(..)));
        assert_eq!(provider.sent.len(), 1);
    }

    #[test]
    fn autopilot_runs_the_assigned_action_with_its_pinned_launch() {
        use crate::autopilot::{Autopilot, Budget, Outcome as AutopilotOutcome};
        use tod_store::outline::{Capability, OutlineMutation};

        let fixture = fixture();
        fixture.outline(OutlineMutation::EnableCapabilities {
            node_id: fixture.node,
            capabilities: vec![Capability::Lifecycle, Capability::Agent],
        });
        fixture.fleet.writer().flush().unwrap();
        fixture.outline(OutlineMutation::SetNodeAgent {
            node_id: fixture.node,
            platform: Some("cursor".into()),
            model: Some("auto".into()),
            effort: None,
        });
        let (request, _, _) = published(&fixture);
        let runner = Runner::new(&fixture.fleet);
        let prior = runner.get(request.run).unwrap().unwrap();
        runner.cancel(&prior).unwrap();
        let fresh = runner.assign(fixture.node, &prior.revision, 20).unwrap();
        let mut pilot = Autopilot::new(config(&fixture), fixture.node, Budget::default())
            .unwrap()
            .with_poll_interval(Duration::ZERO);
        let mut provider = TestAgent::new(false);
        assert_eq!(
            pilot.run(&fixture.fleet, &mut provider).unwrap(),
            AutopilotOutcome::Done
        );
        assert_eq!(provider.sent.len(), 1);
        assert_eq!(provider.sent[0].options.platform, AgentPlatform::Claude);
        let visits = runner.visits(fresh.id).unwrap();
        assert_eq!(
            visits[0].output.as_ref().unwrap().values["findings"],
            Value::Integer(2)
        );
        assert_eq!(visits[1].node, "end");
        assert_eq!(runner.traces(fresh.id).unwrap().len(), 2);
    }

    #[test]
    fn autopilot_keeps_a_workflow_session_limit_across_reopens() {
        use crate::autopilot::{Autopilot, Budget, BudgetLimit, Outcome as AutopilotOutcome};

        let fixture = fixture();
        let (request, _, _) = published(&fixture);
        let runner = Runner::new(&fixture.fleet);
        let original = runner.get(request.run).unwrap().unwrap();
        runner.cancel(&original).unwrap();
        let catalog = Catalog::new(&fixture.fleet);
        let draft = catalog.draft(&original.revision.id).unwrap().unwrap();
        let mut definition = draft.definition;
        let mut second = definition.nodes["step"].clone();
        second.label = "Follow-up review".into();
        second.routes[0].id = "followup_finished".into();
        definition.nodes.get_mut("step").unwrap().routes[0].to = "second".into();
        definition.nodes.insert("second".into(), second);
        let draft = catalog
            .save(
                &original.revision.id,
                draft.generation,
                "Two reviews",
                definition,
            )
            .unwrap();
        let workflow = catalog
            .publish(&original.revision.id, draft.generation)
            .unwrap();
        let assigned = runner
            .assign(fixture.node, &workflow.definition.revision, 10)
            .unwrap();
        let budget = Budget {
            max_sessions: 1,
            ..Budget::default()
        };
        let reopen = || {
            Autopilot::new(config(&fixture), fixture.node, budget)
                .unwrap()
                .with_poll_interval(Duration::ZERO)
        };
        let mut provider = TestAgent::new(false);

        for _ in 0..2 {
            let outcome = reopen().run(&fixture.fleet, &mut provider).unwrap();
            assert!(matches!(
                outcome,
                AutopilotOutcome::BudgetExhausted {
                    limit: BudgetLimit::Sessions { used: 1, .. }
                }
            ));
            assert_eq!(provider.sent.len(), 1);
            assert_eq!(runner.get(assigned.id).unwrap().unwrap().node, "second");
        }

        let mut renewed = reopen();
        renewed.renew_budget().unwrap();
        assert!(matches!(
            renewed.run(&fixture.fleet, &mut provider).unwrap(),
            AutopilotOutcome::BudgetExhausted {
                limit: BudgetLimit::Sessions { used: 1, .. }
            }
        ));
        assert_eq!(provider.sent.len(), 2);
        assert_eq!(runner.get(assigned.id).unwrap().unwrap().node, "end");

        // Every boundary observes the budget, including the final transition.
        renewed.renew_budget().unwrap();
        assert_eq!(
            renewed.run(&fixture.fleet, &mut provider).unwrap(),
            AutopilotOutcome::Done
        );
        assert_eq!(provider.sent.len(), 2);
    }
}
