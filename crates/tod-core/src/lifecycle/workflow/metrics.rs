//! Task-specific revision metrics from durable visits and decision traces.

use super::{
    Revision,
    runner::{ActionOutput, RunStatus},
};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use tod_store::{fleet::FleetStore, outline::uuid_blob::now_ms, workflow_run};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RevisionMetrics {
    pub revision: Revision,
    pub runs: u64,
    pub completed_runs: u64,
    pub visits: u64,
    pub revisits: u64,
    /// Total run wall time, including waits. In-progress runs use the read time.
    pub elapsed_ms: u64,
    /// Action processing time reported by completed action adapters.
    pub action_ms: u64,
    /// Time in recorded gate waits plus wait time reported by action adapters.
    pub wait_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_micros: u64,
}

impl RevisionMetrics {
    fn new(revision: Revision) -> Self {
        Self {
            revision,
            runs: 0,
            completed_runs: 0,
            visits: 0,
            revisits: 0,
            elapsed_ms: 0,
            action_ms: 0,
            wait_ms: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_micros: 0,
        }
    }
}

pub fn for_task(fleet: &FleetStore, task: Uuid) -> Result<Vec<RevisionMetrics>> {
    fleet.read(|conn| {
        let snapshot = conn.unchecked_transaction()?;
        let as_of = now_ms();
        let mut revisions = BTreeMap::new();
        for run in workflow_run::history(&snapshot, task)? {
            let metrics = revisions
                .entry((run.workflow_id.clone(), run.revision))
                .or_insert_with(|| {
                    RevisionMetrics::new(Revision {
                        id: run.workflow_id,
                        number: run.revision,
                    })
                });
            metrics.runs += 1;
            metrics.completed_runs += u64::from(run.status == RunStatus::Complete);
            let end = if run.status.is_terminal() {
                run.updated_at
            } else {
                as_of.max(run.updated_at)
            };
            metrics.elapsed_ms = metrics
                .elapsed_ms
                .saturating_add(elapsed(run.started_at, end));

            let mut seen = BTreeSet::new();
            for visit in workflow_run::visits(&snapshot, run.id)? {
                metrics.visits += 1;
                metrics.revisits += u64::from(!seen.insert(visit.node));
                if let Some(output) = visit.output {
                    let action = serde_json::from_str::<ActionOutput>(&output)?.metrics;
                    metrics.action_ms = metrics.action_ms.saturating_add(action.duration_ms);
                    metrics.wait_ms = metrics.wait_ms.saturating_add(action.wait_ms);
                    metrics.input_tokens = metrics.input_tokens.saturating_add(action.input_tokens);
                    metrics.output_tokens =
                        metrics.output_tokens.saturating_add(action.output_tokens);
                    metrics.cost_micros = metrics.cost_micros.saturating_add(action.cost_micros);
                }
            }

            let traces = workflow_run::traces(&snapshot, run.id)?;
            let times = traces.iter().map(|trace| trace.recorded_at);
            for (trace, next) in traces.iter().zip(times.skip(1).chain([end])) {
                if trace.resulting_status == RunStatus::Waiting {
                    metrics.wait_ms = metrics
                        .wait_ms
                        .saturating_add(elapsed(trace.recorded_at, next));
                }
            }
        }
        Ok(revisions.into_values().collect())
    })
}

fn elapsed(start: i64, end: i64) -> u64 {
    end.saturating_sub(start).max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        interview::test_support::fixture,
        lifecycle::workflow::{
            Comparison, Condition, Definition, FORMAT_VERSION, Field, Node, NodeKind, Observations,
            Operand, Route, Source, Value, ValueType,
            builtin::MANUAL,
            catalog::Catalog,
            runner::{RunMetrics, Runner},
        },
    };

    #[test]
    fn groups_attempts_and_completed_action_evidence_by_pinned_revision() {
        let fixture = fixture();
        let catalog = Catalog::new(&fixture.fleet);
        let draft = catalog
            .create(
                "Metrics example",
                Definition {
                    task_phases: BTreeMap::new(),
                    adoption_points: BTreeMap::new(),
                    format: FORMAT_VERSION,
                    revision: Revision {
                        id: String::new(),
                        number: 0,
                    },
                    entry: "review".into(),
                    nodes: BTreeMap::from([
                        (
                            "review".into(),
                            Node {
                                label: "Review".into(),
                                kind: NodeKind::Action {
                                    action: Revision {
                                        id: MANUAL.into(),
                                        number: 1,
                                    },
                                },
                                outputs: BTreeMap::from([("repeat".into(), ValueType::Boolean)]),
                                routes: vec![
                                    Route {
                                        id: "repeat".into(),
                                        to: "review".into(),
                                        when: Some(Condition::Compare {
                                            left: Operand::Field("repeat".into()),
                                            op: Comparison::Equal,
                                            right: Operand::Literal(Value::Boolean(true)),
                                        }),
                                    },
                                    Route {
                                        id: "done".into(),
                                        to: "done".into(),
                                        when: None,
                                    },
                                ],
                            },
                        ),
                        (
                            "done".into(),
                            Node {
                                label: "Finish".into(),
                                kind: NodeKind::Finish,
                                outputs: BTreeMap::new(),
                                routes: Vec::new(),
                            },
                        ),
                    ]),
                    fields: BTreeMap::from([(
                        "repeat".into(),
                        Field {
                            value_type: ValueType::Boolean,
                            source: Source::NodeOutput {
                                node: "review".into(),
                                field: "repeat".into(),
                            },
                        },
                    )]),
                },
            )
            .unwrap();
        let revision = catalog
            .publish(&draft.definition.revision.id, draft.generation)
            .unwrap()
            .definition
            .revision;
        let runner = Runner::new(&fixture.fleet);
        let mut run = runner.assign(fixture.node, &revision, 10).unwrap();
        for repeat in [true, false] {
            let request = runner.dispatch(&run).unwrap();
            runner
                .complete(
                    &request,
                    ActionOutput {
                        values: BTreeMap::from([("repeat".into(), Value::Boolean(repeat))]),
                        metrics: RunMetrics {
                            duration_ms: 7,
                            wait_ms: 3,
                            input_tokens: 10,
                            output_tokens: 5,
                            cost_micros: 6,
                        },
                        ..Default::default()
                    },
                )
                .unwrap();
            run = runner.get(run.id).unwrap().unwrap();
            runner.evaluate(&run, Observations::new()).unwrap();
            run = runner.get(run.id).unwrap().unwrap();
        }
        runner.evaluate(&run, Observations::new()).unwrap();
        let abandoned = runner.assign(fixture.node, &revision, 10).unwrap();
        runner.cancel(&abandoned).unwrap();
        let draft = catalog.draft(&revision.id).unwrap().unwrap();
        let newer = catalog
            .publish(&revision.id, draft.generation)
            .unwrap()
            .definition
            .revision;
        runner.assign(fixture.node, &newer, 10).unwrap();

        let metrics = for_task(&fixture.fleet, fixture.node).unwrap();
        let [original, new] = metrics.as_slice() else {
            panic!("Expected two revision totals")
        };
        assert_eq!(original.revision, revision);
        assert_eq!((original.runs, original.completed_runs), (2, 1));
        assert_eq!((original.visits, original.revisits), (4, 1));
        assert_eq!((original.action_ms, original.wait_ms), (14, 6));
        assert_eq!(
            (
                original.input_tokens,
                original.output_tokens,
                original.cost_micros
            ),
            (20, 10, 12)
        );
        assert_eq!((&new.revision, new.runs, new.visits), (&newer, 1, 1));
    }
}
