//! Task-bound execution of a published workflow. The runner durably records
//! action requests, results and evaluations through the fleet writer.

use super::{
    Decision, Definition, NodeKind, Observation, Observations, Revision, Source, Trace, Value,
    retry::RetryPlan,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tod_store::{
    fleet::FleetStore,
    interview::{ACTOR_USER, InterviewCommand},
    workflow_run::{self as store, Checkpoint, Mutation, Transition},
};
use uuid::Uuid;

pub use tod_store::workflow_run::RunStatus;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: Uuid,
    pub task: Uuid,
    pub revision: Revision,
    pub node: String,
    pub visit: u64,
    pub generation: u64,
    pub max_visits: u32,
    pub status: RunStatus,
    pub started_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adopted_from: Option<store::LegacyStay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detached_at: Option<i64>,
}

impl From<store::RunRow> for Run {
    fn from(row: store::RunRow) -> Self {
        Self {
            id: row.id,
            task: row.task,
            revision: Revision {
                id: row.workflow_id,
                number: row.revision,
            },
            node: row.node,
            visit: row.visit,
            generation: row.generation,
            max_visits: row.max_visits,
            status: row.status,
            started_at: row.started_at,
            updated_at: row.updated_at,
            adopted_from: row.adopted_from,
            detached_at: row.detached_at,
        }
    }
}

impl Run {
    pub(crate) fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            run: self.id,
            visit: self.visit,
            generation: self.generation,
        }
    }

    fn require_active(&self) -> Result<()> {
        ensure!(!self.status.is_terminal(), "Workflow run has ended.");
        Ok(())
    }
}

/// Durable handoff to an action adapter. Use the request ID as the
/// idempotency key for external effects when resuming a pending visit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRequest {
    pub id: String,
    pub run: Uuid,
    pub visit: u64,
    pub task: Uuid,
    pub node: String,
    pub action: Revision,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMetrics {
    pub duration_ms: u64,
    pub wait_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_micros: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionOutput {
    pub values: BTreeMap<String, Value>,
    pub evidence: Vec<String>,
    pub metrics: RunMetrics,
    pub assets: Vec<Revision>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Visit {
    pub number: u64,
    pub node: String,
    pub dispatch: Option<ActionRequest>,
    pub output: Option<ActionOutput>,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub ended_at: Option<i64>,
}

impl TryFrom<store::VisitRow> for Visit {
    type Error = anyhow::Error;

    fn try_from(row: store::VisitRow) -> Result<Self> {
        Ok(Self {
            number: row.number,
            node: row.node,
            dispatch: row
                .dispatch
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            output: row
                .output
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            started_at: row.started_at,
            completed_at: row.completed_at,
            ended_at: row.ended_at,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisitTrace {
    pub sequence: u64,
    pub trace: Trace,
    pub recorded_at: i64,
    /// The actual durable outcome, including a stop imposed by the visit budget.
    pub resulting_status: RunStatus,
}

pub struct Runner<'a> {
    fleet: &'a FleetStore,
}

impl<'a> Runner<'a> {
    pub fn new(fleet: &'a FleetStore) -> Self {
        Self { fleet }
    }

    pub fn current(&self, task: Uuid) -> Result<Option<Run>> {
        self.fleet
            .read(|conn| Ok(store::current(conn, task)?.map(Run::from)))
    }

    pub fn get(&self, run: Uuid) -> Result<Option<Run>> {
        self.fleet
            .read(|conn| Ok(store::get(conn, run)?.map(Run::from)))
    }

    pub fn history(&self, task: Uuid) -> Result<Vec<Run>> {
        self.fleet.read(|conn| {
            Ok(store::history(conn, task)?
                .into_iter()
                .map(Run::from)
                .collect())
        })
    }

    pub fn visits(&self, run: Uuid) -> Result<Vec<Visit>> {
        self.fleet.read(|conn| {
            store::visits(conn, run)?
                .into_iter()
                .map(Visit::try_from)
                .collect()
        })
    }

    pub fn current_visit(&self, run: &Run) -> Result<Visit> {
        self.fleet.read(|conn| {
            let stored = store::get(conn, run.id)?.context("Workflow run no longer exists.")?;
            ensure!(
                stored.visit == run.visit,
                "This workflow visit is no longer current."
            );
            store::visit(conn, run.id, run.visit)?
                .context("Current workflow visit is missing.")?
                .try_into()
        })
    }

    pub fn traces(&self, run: Uuid) -> Result<Vec<VisitTrace>> {
        self.fleet.read(|conn| {
            store::traces(conn, run)?
                .into_iter()
                .map(|row| {
                    Ok(VisitTrace {
                        sequence: row.sequence,
                        trace: serde_json::from_str(&row.trace)?,
                        recorded_at: row.recorded_at,
                        resulting_status: row.resulting_status,
                    })
                })
                .collect()
        })
    }

    pub fn assign(&self, task: Uuid, revision: &Revision, max_visits: u32) -> Result<Run> {
        let definition = self.definition(revision)?;
        definition.validate().map_err(invalid)?;
        let run = Uuid::new_v4();
        self.mutate(Mutation::Assign {
            run,
            task,
            workflow_id: revision.id.clone(),
            revision: revision.number,
            entry: definition.entry,
            max_visits,
        })?;
        self.get(run)?
            .context("Workflow assignment was not found after publication.")
    }

    pub fn dispatch(&self, run: &Run) -> Result<ActionRequest> {
        run.require_active()?;
        let definition = self.definition(&run.revision)?;
        let node = definition
            .nodes
            .get(&run.node)
            .context("Workflow node no longer exists.")?;
        let NodeKind::Action { action } = &node.kind else {
            return Err(anyhow!("This workflow node has no action to dispatch."));
        };
        let request = ActionRequest {
            id: format!("{}/{}", run.id, run.visit),
            run: run.id,
            visit: run.visit,
            task: run.task,
            node: run.node.clone(),
            action: action.clone(),
        };
        self.mutate(Mutation::Dispatch {
            checkpoint: run.checkpoint(),
            request: serde_json::to_string(&request)?,
        })?;
        Ok(request)
    }

    pub fn complete(&self, request: &ActionRequest, output: ActionOutput) -> Result<()> {
        let run = self
            .get(request.run)?
            .context("Workflow run no longer exists.")?;
        ensure!(
            run.visit == request.visit && run.node == request.node,
            "This workflow visit is no longer current."
        );
        let definition = self.definition(&run.revision)?;
        let node = definition
            .nodes
            .get(&run.node)
            .context("Workflow node no longer exists.")?;
        for (field, value) in &output.values {
            let expected = node
                .outputs
                .get(field)
                .with_context(|| format!("Unknown action output `{field}`."))?;
            ensure!(
                value.value_type() == *expected,
                "Action output `{field}` has the wrong type."
            );
        }
        for field in node.outputs.keys() {
            ensure!(
                output.values.contains_key(field),
                "Action output `{field}` is missing."
            );
        }
        self.mutate(Mutation::Complete {
            run: run.id,
            visit: run.visit,
            request: serde_json::to_string(request)?,
            output: serde_json::to_string(&output)?,
        })
    }

    /// Provider snapshots must be current for the supplied checkpoint.
    /// Outputs recorded by actions are resolved from the run's visits.
    pub fn evaluate(&self, run: &Run, observations: Observations) -> Result<Trace> {
        self.evaluate_with_wait(run, observations, |_| Ok(None))
            .map(|(trace, _)| trace)
    }

    pub(crate) fn evaluate_with_wait(
        &self,
        run: &Run,
        observations: Observations,
        schedule: impl FnOnce(&Trace) -> Result<Option<store::WaitSchedule>>,
    ) -> Result<(Trace, Option<store::WaitSchedule>)> {
        self.evaluate_with_retry(run, observations, RetryPlan::default(), schedule)
    }

    pub(crate) fn evaluate_with_retry(
        &self,
        run: &Run,
        mut observations: Observations,
        retry: RetryPlan,
        schedule: impl FnOnce(&Trace) -> Result<Option<store::WaitSchedule>>,
    ) -> Result<(Trace, Option<store::WaitSchedule>)> {
        run.require_active()?;
        let definition = self.definition(&run.revision)?;
        let visits = self.visits(run.id)?;
        let current = visits
            .last()
            .context("Current workflow visit is missing.")?;
        ensure!(
            current.number == run.visit,
            "This workflow visit is no longer current."
        );
        let node = definition
            .nodes
            .get(&run.node)
            .context("Workflow node no longer exists.")?;
        ensure!(
            !matches!(node.kind, NodeKind::Action { .. }) || current.output.is_some(),
            "Complete the workflow action before evaluating its routes."
        );

        let latest: BTreeMap<_, _> = visits
            .iter()
            .map(|visit| (visit.node.as_str(), visit))
            .collect();
        for (key, field) in &definition.fields {
            if let Source::NodeOutput { node, field } = &field.source {
                observations.remove(key);
                if let Some(visit) = latest.get(node.as_str())
                    && let Some(output) = &visit.output
                    && let Some(value) = output.values.get(field)
                {
                    observations.insert(
                        key.clone(),
                        Observation::Ready {
                            value: value.clone(),
                            evidence: format!(
                                "workflow/{}/visit/{}/output/{field}",
                                run.id, visit.number
                            ),
                        },
                    );
                }
            }
        }
        let mut trace = definition
            .validate()
            .map_err(invalid)?
            .decide(&run.node, run.visit, &observations)
            .context("Workflow node no longer exists.")?;
        let transition = match &trace.decision {
            Decision::OperatorMove { .. } => {
                bail!("Only an explicit task move can enter this phase.")
            }
            Decision::Take { route, to } => Transition::Move {
                route: route.clone(),
                to: to.clone(),
            },
            Decision::Complete => Transition::Complete,
            Decision::Blocked => Transition::Stay(RunStatus::Blocked),
            Decision::Waiting => Transition::Stay(RunStatus::Waiting),
            Decision::Error => Transition::Stay(RunStatus::Error),
            Decision::Conflict { .. } => Transition::Stay(RunStatus::Conflict),
        };
        trace.github_retries = retry.history;
        let wait = match retry.wait {
            Some(wait) if trace.decision == Decision::Waiting => Some(wait),
            _ => schedule(&trace)?,
        };
        self.mutate(Mutation::Decide {
            checkpoint: run.checkpoint(),
            trace: serde_json::to_string(&trace)?,
            transition,
            wait: wait.clone(),
        })?;
        Ok((trace, wait))
    }

    pub fn cancel(&self, run: &Run) -> Result<()> {
        self.mutate(Mutation::Cancel {
            checkpoint: run.checkpoint(),
        })
    }

    fn definition(&self, revision: &Revision) -> Result<Definition> {
        let definition = super::catalog::Catalog::new(self.fleet)
            .published(&revision.id, revision.number)?
            .context("Use a published workflow revision.")?
            .definition;
        ensure!(
            definition.revision == *revision,
            "Published workflow revision disagrees with its key."
        );
        Ok(definition)
    }

    fn mutate(&self, mutation: Mutation) -> Result<()> {
        self.fleet
            .interview(ACTOR_USER, InterviewCommand::WorkflowRun { mutation })?;
        self.fleet.reload_if_stale()?;
        Ok(())
    }
}

fn invalid(diagnostics: Vec<super::Diagnostic>) -> anyhow::Error {
    let messages: Vec<_> = diagnostics
        .into_iter()
        .map(|d| format!("{} — {}", d.location, d.message))
        .collect();
    anyhow!("Invalid workflow definition. {}", messages.join(" "))
}

#[cfg(test)]
mod tests {
    use super::super::{
        Comparison, Condition, FORMAT_VERSION, Field, Node, Operand, Route, ValueType,
        catalog::Catalog,
    };
    use super::*;
    use crate::interview::test_support::fixture;

    fn publish(fleet: &FleetStore) -> Revision {
        let action = |id: &str| NodeKind::Action {
            action: Revision {
                id: id.into(),
                number: 1,
            },
        };
        let compare = |field: &str, op, value| Condition::Compare {
            left: Operand::Field(field.into()),
            op,
            right: Operand::Literal(value),
        };
        let nodes = BTreeMap::from([
            (
                "review".into(),
                Node {
                    label: "Read the result".into(),
                    kind: action("review-result"),
                    outputs: BTreeMap::from([("count".into(), ValueType::Integer)]),
                    routes: vec![
                        Route {
                            id: "redo".into(),
                            to: "fix".into(),
                            when: Some(compare("findings", Comparison::Greater, Value::Integer(0))),
                        },
                        Route {
                            id: "finish".into(),
                            to: "done".into(),
                            when: Some(Condition::All(vec![
                                compare("findings", Comparison::Equal, Value::Integer(0)),
                                compare("approval", Comparison::Equal, Value::Boolean(true)),
                            ])),
                        },
                    ],
                },
            ),
            (
                "fix".into(),
                Node {
                    label: "Improve it".into(),
                    kind: action("revise-result"),
                    outputs: BTreeMap::new(),
                    routes: vec![Route {
                        id: "review_again".into(),
                        to: "review".into(),
                        when: None,
                    }],
                },
            ),
            (
                "done".into(),
                Node {
                    label: "Finish".into(),
                    kind: NodeKind::Finish,
                    outputs: BTreeMap::new(),
                    routes: vec![],
                },
            ),
        ]);
        let fields = BTreeMap::from([
            (
                "findings".into(),
                Field {
                    value_type: ValueType::Integer,
                    source: Source::NodeOutput {
                        node: "review".into(),
                        field: "count".into(),
                    },
                },
            ),
            (
                "approval".into(),
                Field {
                    value_type: ValueType::Boolean,
                    source: Source::Provider {
                        binding: "manual".into(),
                        field: "approved".into(),
                    },
                },
            ),
        ]);
        let catalog = Catalog::new(fleet);
        let draft = catalog
            .create(
                "Review loop",
                Definition {
                    format: FORMAT_VERSION,
                    revision: Revision {
                        id: String::new(),
                        number: 0,
                    },
                    entry: "review".into(),
                    nodes,
                    fields,
                    task_phases: BTreeMap::new(),
                    adoption_points: BTreeMap::new(),
                },
            )
            .unwrap();
        catalog
            .publish(&draft.definition.revision.id, draft.generation)
            .unwrap()
            .definition
            .revision
    }

    fn count(value: i64) -> ActionOutput {
        ActionOutput {
            values: BTreeMap::from([("count".into(), Value::Integer(value))]),
            evidence: vec!["sample-artifact:v3".into()],
            metrics: RunMetrics {
                duration_ms: 10,
                output_tokens: 20,
                ..RunMetrics::default()
            },
            assets: vec![Revision {
                id: "evaluation-criteria".into(),
                number: 3,
            }],
        }
    }

    #[test]
    fn run_persists_across_reopen_and_rejects_old_completions() {
        let fixture = fixture();
        let revision = publish(&fixture.fleet);
        let runner = Runner::new(&fixture.fleet);
        let first = runner.assign(fixture.node, &revision, 20).unwrap();
        assert!(runner.assign(fixture.node, &revision, 20).is_err());
        assert!(runner.evaluate(&first, Observations::new()).is_err());
        let initial = runner.dispatch(&first).unwrap();
        assert_eq!(initial, runner.dispatch(&first).unwrap());
        assert!(runner.complete(&initial, ActionOutput::default()).is_err());
        runner.complete(&initial, count(2)).unwrap();
        runner.complete(&initial, count(2)).unwrap();
        assert!(runner.complete(&initial, count(3)).is_err());
        assert!(runner.evaluate(&first, Observations::new()).is_err());

        let first = runner.get(first.id).unwrap().unwrap();
        let initial_trace = runner
            .evaluate(
                &first,
                Observations::from([(
                    "findings".into(),
                    Observation::Ready {
                        value: Value::Integer(0),
                        evidence: "caller".into(),
                    },
                )]),
            )
            .unwrap();
        assert_eq!(
            initial_trace.decision,
            Decision::Take {
                route: "redo".into(),
                to: "fix".into()
            }
        );
        let fix = runner.get(first.id).unwrap().unwrap();
        assert_eq!(fix.visit, 2);
        let fix_request = runner.dispatch(&fix).unwrap();
        runner
            .complete(&fix_request, ActionOutput::default())
            .unwrap();
        let fix = runner.get(first.id).unwrap().unwrap();
        runner.evaluate(&fix, Observations::new()).unwrap();

        let second = runner.get(first.id).unwrap().unwrap();
        assert_eq!(second.visit, 3);
        assert_eq!(second.node, first.node);
        assert!(runner.complete(&initial, count(0)).is_err());
        let review_again = runner.dispatch(&second).unwrap();
        assert_ne!(review_again.id, initial.id);
        runner.complete(&review_again, count(0)).unwrap();
        let second = runner.get(first.id).unwrap().unwrap();
        let schedule = store::WaitSchedule {
            what: "an external approval".into(),
            due_at_ms: chrono::Utc::now().timestamp_millis() + 60_000,
        };
        assert_eq!(
            runner
                .evaluate_with_wait(&second, Observations::new(), |_| Ok(Some(schedule.clone())))
                .unwrap()
                .0
                .decision,
            Decision::Waiting
        );
        assert_eq!(runner.traces(first.id).unwrap().len(), 3);

        drop(fixture.fleet);
        let reopened = FleetStore::open_without_reattach(&fixture.root).unwrap();
        let runner = Runner::new(&reopened);
        let catalog = Catalog::new(&reopened);
        let mut draft = catalog.draft(&revision.id).unwrap().unwrap();
        draft.definition.entry = "fix".into();
        let draft = catalog
            .save(
                &revision.id,
                draft.generation,
                "Changed entry",
                draft.definition,
            )
            .unwrap();
        let revised = catalog
            .publish(&revision.id, draft.generation)
            .unwrap()
            .definition
            .revision;
        let pending = runner.current(fixture.node).unwrap().unwrap();
        assert_eq!(pending.status, RunStatus::Waiting);
        assert_eq!(pending.revision, revision);
        let saved = reopened
            .read(|conn| store::scheduled_wait(conn, fixture.node))
            .unwrap()
            .unwrap();
        assert_eq!(saved.schedule, schedule);
        assert_eq!(
            runner.current_visit(&pending).unwrap().output,
            Some(count(0))
        );
        assert!(runner.evaluate(&second, Observations::new()).is_err());
        assert!(
            runner
                .evaluate_with_wait(&second, Observations::new(), |_| Ok(Some(
                    store::WaitSchedule {
                        what: "stale retry".into(),
                        due_at_ms: 1
                    },
                )))
                .is_err()
        );
        assert_eq!(
            reopened
                .read(|conn| store::scheduled_wait(conn, fixture.node))
                .unwrap(),
            Some(saved),
        );
        runner
            .evaluate(
                &pending,
                Observations::from([(
                    "approval".into(),
                    Observation::Ready {
                        value: Value::Boolean(true),
                        evidence: "operator approval #1".into(),
                    },
                )]),
            )
            .unwrap();
        let done = runner.current(fixture.node).unwrap().unwrap();
        assert_eq!(done.node, "done");
        assert!(
            reopened
                .read(|conn| store::scheduled_wait(conn, fixture.node))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            runner
                .evaluate(&done, Observations::new())
                .unwrap()
                .decision,
            Decision::Complete
        );
        assert_eq!(
            runner.current(fixture.node).unwrap().unwrap().status,
            RunStatus::Complete
        );
        let history = runner.traces(done.id).unwrap();
        assert_eq!(history.len(), 5);
        assert_eq!(history[0].trace, initial_trace);
        assert_eq!(
            history.last().unwrap().resulting_status,
            RunStatus::Complete
        );
        let next = runner.assign(fixture.node, &revised, 20).unwrap();
        assert_ne!(next.id, first.id);
        assert_eq!(next.node, "fix");
        assert_eq!(runner.current(fixture.node).unwrap(), Some(next.clone()));
        let history = runner.history(fixture.node).unwrap();
        assert_eq!(history[0], next);
        assert_eq!(history[1].revision, revision);
    }

    #[test]
    fn mapped_phases_follow_the_cursor_and_only_finish_on_completion() {
        use tod_store::outline::{OutlineMutation, repos::NodeRepo};

        let fixture = fixture();
        let fleet = &fixture.fleet;
        let task = fixture.node;
        let move_task = |phase: &str| {
            fleet
                .enqueue_outline(OutlineMutation::SetLifecycle {
                    node_id: task,
                    state: phase.into(),
                })
                .unwrap();
            fleet.writer().flush().unwrap();
        };
        let projected = || {
            fleet
                .read(|conn| NodeRepo::new(conn).get_lifecycle(task))
                .unwrap()
        };
        let initial = publish(fleet);
        let catalog = Catalog::new(fleet);
        let mut draft = catalog.draft(&initial.id).unwrap().unwrap();
        draft.definition.task_phases = BTreeMap::from([
            ("review".into(), "review".into()),
            ("fix".into(), "verifying".into()),
            ("done".into(), "done".into()),
        ]);
        let draft = catalog
            .save(
                &initial.id,
                draft.generation,
                &draft.title,
                draft.definition,
            )
            .unwrap();
        let revision = catalog
            .publish(&initial.id, draft.generation)
            .unwrap()
            .definition
            .revision;
        let runner = Runner::new(fleet);
        move_task("ready");
        assert!(runner.assign(task, &revision, 10).is_err());
        assert!(runner.current(task).unwrap().is_none());
        move_task("review");
        let run = runner.assign(task, &revision, 10).unwrap();
        runner
            .complete(&runner.dispatch(&run).unwrap(), count(1))
            .unwrap();
        let review = runner.get(run.id).unwrap().unwrap();

        move_task("planning");
        assert!(runner.evaluate(&review, Observations::new()).is_err());
        assert_eq!(runner.current(task).unwrap(), Some(review.clone()));
        assert!(runner.traces(run.id).unwrap().is_empty());
        move_task("verifying"); // An existing protocol wrote the selected sendback first.
        runner.evaluate(&review, Observations::new()).unwrap();
        let fix = runner.get(run.id).unwrap().unwrap();
        assert_eq!(
            (fix.node.as_str(), projected().as_deref()),
            ("fix", Some("verifying"))
        );
        runner
            .complete(&runner.dispatch(&fix).unwrap(), ActionOutput::default())
            .unwrap();
        runner
            .evaluate(&runner.get(run.id).unwrap().unwrap(), Observations::new())
            .unwrap();
        let review = runner.get(run.id).unwrap().unwrap();
        assert_eq!(
            (review.node.as_str(), projected().as_deref()),
            ("review", Some("review"))
        );
        runner
            .complete(&runner.dispatch(&review).unwrap(), count(0))
            .unwrap();
        runner
            .evaluate(
                &runner.get(run.id).unwrap().unwrap(),
                Observations::from([(
                    "approval".into(),
                    Observation::Ready {
                        value: Value::Boolean(true),
                        evidence: "approval".into(),
                    },
                )]),
            )
            .unwrap();
        let finish = runner.get(run.id).unwrap().unwrap();
        assert_eq!(
            (finish.node.as_str(), projected().as_deref()),
            ("done", Some("review"))
        );
        runner.evaluate(&finish, Observations::new()).unwrap();
        assert_eq!(projected().as_deref(), Some("done"));
        assert_eq!(
            runner.get(run.id).unwrap().unwrap().status,
            RunStatus::Complete
        );
    }

    #[test]
    fn visit_limit_records_decision_without_starting_another_visit() {
        let fixture = fixture();
        let revision = publish(&fixture.fleet);
        let runner = Runner::new(&fixture.fleet);
        let run = runner.assign(fixture.node, &revision, 1).unwrap();
        let request = runner.dispatch(&run).unwrap();
        runner.complete(&request, count(1)).unwrap();
        let run = runner.get(run.id).unwrap().unwrap();
        let evaluated = runner.evaluate(&run, Observations::new()).unwrap();
        assert_eq!(
            evaluated.decision,
            Decision::Take {
                route: "redo".into(),
                to: "fix".into()
            }
        );
        let stopped = runner.get(run.id).unwrap().unwrap();
        assert_eq!(stopped.status, RunStatus::LimitReached);
        let visits = runner.visits(run.id).unwrap();
        assert_eq!(visits.len(), 1);
        assert!(visits[0].ended_at.is_some());
        assert_eq!(
            runner.traces(run.id).unwrap()[0].resulting_status,
            RunStatus::LimitReached
        );
        assert!(runner.dispatch(&stopped).is_err());
    }

    #[test]
    fn assignment_requires_the_legacy_run_to_stop_and_cancel_ends_the_current_visit() {
        use tod_store::activity::{ActivityLease, KIND_RUN};

        let fixture = fixture();
        let revision = publish(&fixture.fleet);
        let runner = Runner::new(&fixture.fleet);
        let lease = ActivityLease::start(&fixture.fleet, fixture.node, KIND_RUN, "test").unwrap();
        fixture.fleet.writer().flush().unwrap();
        let error = runner.assign(fixture.node, &revision, 20).unwrap_err();
        assert!(format!("{error:#}").contains("running lifecycle"));
        lease.end(&fixture.fleet).unwrap();

        let run = runner.assign(fixture.node, &revision, 20).unwrap();
        let request = runner.dispatch(&run).unwrap();
        assert!(runner.cancel(&run).is_err());
        let run = runner.get(run.id).unwrap().unwrap();
        runner.cancel(&run).unwrap();
        assert!(runner.complete(&request, count(0)).is_err());
        let cancelled = runner.get(run.id).unwrap().unwrap();
        assert_eq!(cancelled.status, RunStatus::Cancelled);
        assert!(runner.current_visit(&cancelled).unwrap().ended_at.is_some());
    }
}
