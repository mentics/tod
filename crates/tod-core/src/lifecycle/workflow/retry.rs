//! Retry GitHub reads without losing the decision or giving the scheduler two wakes.

use super::{
    Decision, Observation, Observations, Trace,
    runner::{Run, Runner},
};
use crate::{
    autopilot::{Outcome, github_wait},
    incident,
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tod_store::{
    fleet::FleetStore, incident_class::Source, incidents::Verdict, waits::DEFAULT_MAX_ATTEMPTS,
    workflow_run::WaitSchedule,
};

/// Pull-request reads and release reads maintain independent retry budgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubOperation {
    PullRequests,
    Releases,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GithubReadFailure {
    pub operation: GithubOperation,
    pub message: String,
}

#[derive(Default)]
pub(crate) struct GithubReads {
    /// Only failures from a provider call, keyed by workflow observation.
    pub failures: BTreeMap<String, GithubReadFailure>,
    pub succeeded: bool,
}

/// What the provider returned and the durable retry decision for that read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubRetry {
    pub message: String,
    /// Retry budget used so far. A permanent failure exhausts the budget.
    pub attempt: u32,
    pub rate_limited: bool,
    pub parked: bool,
    /// The proposed deadline, scheduled only when the visit is waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at_ms: Option<i64>,
}

#[derive(Default)]
pub(crate) struct RetryPlan {
    pub history: BTreeMap<GithubOperation, GithubRetry>,
    pub wait: Option<WaitSchedule>,
}

pub(crate) fn prepare(
    fleet: &FleetStore,
    run: &Run,
    observations: &mut Observations,
    reads: GithubReads,
) -> Result<RetryPlan> {
    prepare_at(
        fleet,
        run,
        observations,
        reads,
        chrono::Utc::now().timestamp_millis(),
    )
}

pub(crate) fn failed_outcome(trace: &Trace) -> Option<Outcome> {
    if trace.decision != Decision::Error {
        return None;
    }
    trace
        .github_retries
        .values()
        .find(|retry| retry.due_at_ms.is_none())
        .map(|retry| github_wait::read_failed_outcome(&retry.message))
}

fn prepare_at(
    fleet: &FleetStore,
    run: &Run,
    observations: &mut Observations,
    reads: GithubReads,
    now: i64,
) -> Result<RetryPlan> {
    if reads.failures.is_empty() {
        if reads.succeeded {
            incident::node_ok(fleet, run.task)?;
        }
        return Ok(RetryPlan::default());
    }
    let previous = Runner::new(fleet)
        .traces(run.id)?
        .pop()
        .filter(|last| last.trace.visit == run.visit)
        .map(|last| last.trace.github_retries)
        .unwrap_or_default();
    let mut first_by_operation = BTreeMap::new();
    for failure in reads.failures.values() {
        first_by_operation
            .entry(failure.operation)
            .or_insert(&failure.message);
    }
    let mut waits = BTreeMap::new();
    let mut history = BTreeMap::new();
    for (operation, message) in first_by_operation {
        let previous = previous.get(&operation).map_or(0, |retry| retry.attempt);
        let (retry, wait) = recover(fleet, run, now, previous, message)?;
        history.insert(operation, retry);
        if let Some(wait) = wait {
            waits.insert(operation, wait);
        }
    }
    for (field, failure) in reads.failures {
        let Some(observation @ Observation::Error { .. }) = observations.get_mut(&field) else {
            bail!("GitHub failure for {field} has no error observation.");
        };
        if let Some(wait) = waits.get(&failure.operation) {
            *observation = Observation::Waiting {
                reason: wait.what.clone(),
            };
        }
    }
    Ok(RetryPlan {
        history,
        wait: waits.into_values().min_by_key(|wait| wait.due_at_ms),
    })
}

fn recover(
    fleet: &FleetStore,
    run: &Run,
    now: i64,
    previous: u32,
    error: &str,
) -> Result<(GithubRetry, Option<WaitSchedule>)> {
    let observed = incident::observe(fleet, run.task, Source::Github, "looking at GitHub", error)?;
    if observed.verdict == Verdict::Parked
        && let Some(parking) = incident::parking(fleet, run.task)?
        && let Outcome::Waiting { what, due_at_ms } = incident::parked_outcome(&parking)
    {
        return Ok((
            GithubRetry {
                message: error.into(),
                attempt: 0,
                rate_limited: false,
                parked: true,
                due_at_ms: Some(due_at_ms),
            },
            Some(WaitSchedule { what, due_at_ms }),
        ));
    }

    let (due_at_ms, rate_limited) =
        github_wait::read_retry(
            now,
            previous,
            error,
            observed.verdict == Verdict::Retry,
            github_wait::pull_requests_read_before(fleet, run.task)?,
        );
    let max = DEFAULT_MAX_ATTEMPTS;
    let attempt = match due_at_ms {
        Some(_) if rate_limited => previous.min(max.saturating_sub(1)),
        Some(_) => previous.saturating_add(1).min(max),
        None => max,
    };
    let wait = due_at_ms.filter(|_| attempt < max).map(|due_at_ms| {
        let what = if rate_limited {
            format!("GitHub's rate limit; looking again when it resets: {error}")
        } else {
            format!("GitHub could not be read ({attempt} of {max}): {error}")
        };
        WaitSchedule { what, due_at_ms }
    });
    Ok((
        GithubRetry {
            message: error.into(),
            attempt,
            rate_limited,
            parked: false,
            due_at_ms: wait.as_ref().map(|wait| wait.due_at_ms),
        },
        wait,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        interview::test_support::fixture,
        lifecycle::workflow::{
            Comparison, Condition, Definition, FORMAT_VERSION, Field, Node, NodeKind, Operand,
            Revision, Route, Source, Value, ValueType, builtin::GITHUB, catalog::Catalog,
            runner::RunStatus,
        },
    };
    use tod_store::{github::GithubError, waits::WaitRepo, workflow_run::scheduled_wait};
    use uuid::Uuid;

    fn assign(fleet: &FleetStore, task: Uuid) -> Run {
        let catalog = Catalog::new(fleet);
        let draft = catalog
            .create(
                "Merge",
                Definition {
                    format: FORMAT_VERSION,
                    revision: Revision {
                        id: String::new(),
                        number: 0,
                    },
                    entry: "merge".into(),
                    nodes: BTreeMap::from([
                        (
                            "merge".into(),
                            Node {
                                label: "Wait for merge".into(),
                                kind: NodeKind::Decision,
                                outputs: BTreeMap::new(),
                                routes: vec![Route {
                                    id: "merged".into(),
                                    to: "done".into(),
                                    when: Some(Condition::Compare {
                                        left: Operand::Field("merged".into()),
                                        op: Comparison::Equal,
                                        right: Operand::Literal(Value::Boolean(true)),
                                    }),
                                }],
                            },
                        ),
                        (
                            "done".into(),
                            Node {
                                label: "Done".into(),
                                kind: NodeKind::Finish,
                                outputs: BTreeMap::new(),
                                routes: Vec::new(),
                            },
                        ),
                    ]),
                    fields: BTreeMap::from([(
                        "merged".into(),
                        Field {
                            value_type: ValueType::Boolean,
                            source: Source::Provider {
                                binding: GITHUB.into(),
                                field: "merge_confirmation".into(),
                            },
                        },
                    )]),
                    task_phases: BTreeMap::new(),
                    adoption_points: BTreeMap::new(),
                },
            )
            .unwrap();
        let published = catalog
            .publish(&draft.definition.revision.id, draft.generation)
            .unwrap();
        Runner::new(fleet)
            .assign(task, &published.definition.revision, 10)
            .unwrap()
    }

    fn record(
        fleet: &FleetStore,
        run: &Run,
        error: &GithubError,
        now: i64,
    ) -> (Trace, Option<WaitSchedule>) {
        let message = error.to_string();
        let mut observations = Observations::from([(
            "merged".into(),
            Observation::Error {
                reason: message.clone(),
            },
        )]);
        let reads = GithubReads {
            failures: BTreeMap::from([(
                "merged".into(),
                GithubReadFailure {
                    operation: GithubOperation::PullRequests,
                    message,
                },
            )]),
            succeeded: false,
        };
        let plan = prepare_at(fleet, run, &mut observations, reads, now).unwrap();
        Runner::new(fleet)
            .evaluate_with_retry(run, observations, plan, |_| Ok(None))
            .unwrap()
    }

    #[test]
    fn transient_failures_keep_one_durable_wake_until_the_retry_budget_runs_out() {
        let fixture = fixture();
        let original = assign(&fixture.fleet, fixture.node);
        let error = GithubError::Unavailable("HTTP 503".into());
        let now = chrono::Utc::now().timestamp_millis();
        let (first, wait) = record(&fixture.fleet, &original, &error, now);
        assert_eq!(first.decision, Decision::Waiting);
        let first_wait = wait.unwrap();
        assert_eq!(first_wait.due_at_ms, now + github_wait::retry_delay_ms(0));

        drop(fixture.fleet);
        let reopened = FleetStore::open_without_reattach(&fixture.root).unwrap();
        let runner = Runner::new(&reopened);
        let mut run = runner.get(original.id).unwrap().unwrap();
        let stored = reopened
            .read(|conn| scheduled_wait(conn, fixture.node))
            .unwrap()
            .unwrap();
        assert_eq!(stored.schedule, first_wait);
        assert_eq!(runner.traces(run.id).unwrap()[0].trace, first);

        for attempt in 2..=DEFAULT_MAX_ATTEMPTS {
            let now = now + i64::from(attempt) * 1_000_000;
            let (trace, wait) = record(&reopened, &run, &error, now);
            run = runner.get(run.id).unwrap().unwrap();
            let retry = &trace.github_retries[&GithubOperation::PullRequests];
            assert_eq!(retry.attempt, attempt);
            assert_eq!(retry.message, error.to_string());
            let scheduled = reopened
                .read(|conn| scheduled_wait(conn, fixture.node))
                .unwrap();
            if attempt < DEFAULT_MAX_ATTEMPTS {
                let wait = wait.unwrap();
                assert_eq!(run.status, RunStatus::Waiting);
                assert_eq!(
                    wait.due_at_ms,
                    now + github_wait::retry_delay_ms(attempt - 1)
                );
                let scheduled = scheduled.unwrap();
                assert_eq!(scheduled.id, stored.id);
                assert_eq!(scheduled.schedule, wait);
            } else {
                assert_eq!(run.status, RunStatus::Error);
                assert_eq!(wait, None);
                assert_eq!(scheduled, None);
                assert_eq!(
                    failed_outcome(&trace),
                    Some(github_wait::read_failed_outcome(&error.to_string()))
                );
            }
        }
        assert!(
            reopened
                .read(|conn| WaitRepo::new(conn).list_pending_for_node(fixture.node))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rate_limit_waits_for_reset_without_spending_attempts_and_success_starts_a_new_streak() {
        let fixture = fixture();
        let runner = Runner::new(&fixture.fleet);
        let mut run = assign(&fixture.fleet, fixture.node);
        let now = 1_800_000_000_000;
        let reset_epoch_secs = now / 1000 + 900;
        let rate_limit = GithubError::RateLimited {
            reset_epoch_secs,
            detail: "primary".into(),
            request: "GET /repos/o/r/pulls/1".into(),
            refused: 1,
            quota: "core 0/5000".into(),
        };
        let error = GithubError::Unavailable("HTTP 503".into());
        record(&fixture.fleet, &run, &error, now);
        run = runner.get(run.id).unwrap().unwrap();
        for _ in 0..6 {
            let (trace, wait) = record(&fixture.fleet, &run, &rate_limit, now);
            run = runner.get(run.id).unwrap().unwrap();
            let retry = &trace.github_retries[&GithubOperation::PullRequests];
            assert_eq!(retry.attempt, 1);
            assert!(retry.rate_limited);
            assert_eq!(wait.unwrap().due_at_ms, reset_epoch_secs * 1000 + 5_000);
        }
        let mut observations = Observations::from([(
            "merged".into(),
            Observation::Ready {
                value: Value::Boolean(false),
                evidence: "read GitHub".into(),
            },
        )]);
        let plan = prepare_at(
            &fixture.fleet,
            &run,
            &mut observations,
            GithubReads {
                succeeded: true,
                ..Default::default()
            },
            now,
        )
        .unwrap();
        let (success, wait) = runner
            .evaluate_with_retry(&run, observations, plan, |_| Ok(None))
            .unwrap();
        assert!(success.github_retries.is_empty());
        assert_eq!(wait, None);
        run = runner.get(run.id).unwrap().unwrap();
        let (trace, _) = record(&fixture.fleet, &run, &error, now);
        assert_eq!(
            trace.github_retries[&GithubOperation::PullRequests].attempt,
            1
        );
        run = runner.get(run.id).unwrap().unwrap();
        let (trace, wait) = record(&fixture.fleet, &run, &GithubError::NotFound, now);
        assert_eq!(trace.decision, Decision::Error);
        assert_eq!(wait, None);
    }
}
