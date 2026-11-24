//! Entry points shared by operator interfaces for the built-in workflow actions.

use super::{
    Decision, Definition, NodeKind, Observation, Observations, Source, Trace, Value,
    actions::PublishedAction,
    agent,
    builtin::{self, MANUAL, TASK_SNAPSHOT, WORKSPACE_CAPTURE, WORKSPACE_SETUP},
    catalog::Catalog,
    native, retry,
    runner::{ActionOutput, ActionRequest, Run, Runner},
};
use crate::{
    autopilot::{Autopilot, Boundary, BudgetLimit, NeedsHuman, Outcome, StepHook, Turn},
    conversation::driver::{AgentAccess, ConversationConfig},
};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::collections::BTreeMap;
use tod_store::fleet::FleetStore;
use tod_store::workflow_run::WaitSchedule as ExternalWait;

#[derive(Clone, Debug, Serialize)]
pub struct Progress {
    pub run: Run,
    pub pending_action: Option<ActionRequest>,
    pub stopped: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_limit: Option<BudgetLimit>,
    #[serde(skip)]
    pub(crate) external_wait: Option<ExternalWait>,
    #[serde(skip)]
    pub(crate) native_outcome: Option<Outcome>,
}

/// Drive this run as far as ready built-in actions and observations permit.
/// Manual actions return the durable request a user can finish later.
pub fn resume(fleet: &FleetStore, run: Run, manual: &Observations) -> Result<Progress> {
    resume_with(fleet, run, manual, &mut Operator)
}

pub(crate) fn resume_agent<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
    fleet: &FleetStore,
    run: Run,
    agent: &mut A,
    hook: &mut H,
    autopilot: &mut Autopilot,
    config: &ConversationConfig,
    poll: std::time::Duration,
) -> Result<Progress> {
    let mut driver = WithAgent {
        fleet,
        agent,
        hook,
        autopilot,
        config,
        poll,
    };
    resume_with(fleet, run, &Observations::new(), &mut driver)
}

trait Driver {
    fn stop_requested(&mut self) -> Result<Option<String>>;
    fn budget_limit(&self) -> Option<BudgetLimit> {
        None
    }
    fn external_wait(
        &self,
        _run: &Run,
        _definition: &Definition,
        _trace: &Trace,
    ) -> Result<Option<ExternalWait>> {
        Ok(None)
    }
    fn agent(
        &mut self,
        request: &ActionRequest,
        action: &PublishedAction,
        node: &super::Node,
    ) -> Result<agent::Outcome>;

    fn native(&mut self, _: &ActionRequest, phase: &str, _: i64) -> Result<native::StepResult> {
        Ok(native::StepResult::stopped(format!(
            "Start the task runner to run the {phase} protocol."
        )))
    }
}

struct Operator;

impl Driver for Operator {
    fn stop_requested(&mut self) -> Result<Option<String>> {
        Ok(None)
    }

    fn agent(
        &mut self,
        _: &ActionRequest,
        action: &PublishedAction,
        _: &super::Node,
    ) -> Result<agent::Outcome> {
        Ok(agent::Outcome::Waiting {
            reason: format!("Start the task runner to run {}.", action.spec.title),
        })
    }
}

struct WithAgent<'a, A: ?Sized, H: ?Sized> {
    fleet: &'a FleetStore,
    agent: &'a mut A,
    hook: &'a mut H,
    autopilot: &'a mut Autopilot,
    config: &'a ConversationConfig,
    poll: std::time::Duration,
}

impl<A: AgentAccess + ?Sized, H: StepHook + ?Sized> Driver for WithAgent<'_, A, H> {
    fn stop_requested(&mut self) -> Result<Option<String>> {
        self.hook.at(self.fleet, Boundary::Step)
    }

    fn budget_limit(&self) -> Option<BudgetLimit> {
        self.autopilot.over_budget()
    }

    fn external_wait(
        &self,
        run: &Run,
        definition: &Definition,
        trace: &Trace,
    ) -> Result<Option<ExternalWait>> {
        if trace.decision != Decision::Waiting {
            return Ok(None);
        }
        let Some(reason) = trace.observations.iter().find_map(|(name, value)| {
            let (Source::Provider { binding, field }, Observation::Waiting { reason }) =
                (&definition.fields.get(name)?.source, value)
            else {
                return None;
            };
            (binding == builtin::GITHUB
                && matches!(field.as_str(), "merge_confirmation" | "all_released"))
            .then_some(reason)
        }) else {
            return Ok(None);
        };
        let links = self
            .fleet
            .read(|conn| tod_store::github::NodePrRepo::new(conn).read(run.task))?;
        let root = self.fleet.paths().root();
        // No poll can supply a missing link or credentials.
        if links.prs.is_empty()
            || !links.unrecognized.is_empty()
            || crate::pr_readiness::feed_for(root).is_none()
        {
            return Ok(None);
        }
        let settings = crate::pr_readiness::settings_for_node(self.fleet, run.task, root)?;
        Ok(Some(ExternalWait {
            what: reason.clone(),
            due_at_ms: crate::wait_cadence::next_check(
                chrono::Utc::now(),
                &settings.review_schedule,
            )
            .timestamp_millis(),
        }))
    }

    fn agent(
        &mut self,
        request: &ActionRequest,
        action: &PublishedAction,
        node: &super::Node,
    ) -> Result<agent::Outcome> {
        self.hook.working_on(&action.spec.title);
        agent::run(
            self.fleet,
            self.agent,
            self.config,
            request,
            action,
            &node.outputs,
            self.poll,
            || self.autopilot.record_workflow_launch(),
            |id, status| {
                self.hook.watch(Turn {
                    protocol: tod_store::conversation::ProtocolKind::Chat,
                    conversation_id: Some(id),
                    status,
                })
            },
        )
    }

    fn native(
        &mut self,
        request: &ActionRequest,
        phase: &str,
        entered_at: i64,
    ) -> Result<native::StepResult> {
        let fleet = self.fleet;
        tod_store::activity::with_lease(
            fleet,
            request.task,
            tod_store::activity::KIND_RUN,
            "workflow protocol",
            || {
                let runner = Runner::new(fleet);
                let run = runner
                    .get(request.run)?
                    .context("The workflow run disappeared.")?;
                ensure!(
                    !run.status.is_terminal()
                        && run.visit == request.visit
                        && runner.current_visit(&run)?.dispatch.as_ref() == Some(request),
                    "This workflow visit is no longer current."
                );
                let mut result = self
                    .autopilot
                    .workflow_protocol(fleet, self.agent, self.hook, request, phase, entered_at)?;
                if let native::StepResult::Complete(output) = &mut result {
                    self.autopilot
                        .workflow_protocol_evidence(fleet, phase, entered_at, request, output)?;
                }
                Ok(result)
            },
        )?
    }
}

fn resume_with(
    fleet: &FleetStore,
    run: Run,
    manual: &Observations,
    driver: &mut impl Driver,
) -> Result<Progress> {
    fleet.read(|conn| tod_store::workflow_run::require_local_execution(conn, run.task))?;
    let runner = Runner::new(fleet);
    let definition = pinned_definition(fleet, &run)?;
    let mut run = run;
    loop {
        if run.status.is_terminal() {
            return Ok(Progress {
                run,
                pending_action: None,
                stopped: None,
                budget_limit: None,
                external_wait: None,
                native_outcome: None,
            });
        }
        if let Some(reason) = driver.stop_requested()? {
            return Ok(Progress {
                run,
                pending_action: None,
                stopped: Some(reason),
                budget_limit: None,
                external_wait: None,
                native_outcome: None,
            });
        }
        if let Some(limit) = driver.budget_limit() {
            return Ok(Progress {
                run,
                pending_action: None,
                stopped: None,
                budget_limit: Some(limit),
                external_wait: None,
                native_outcome: None,
            });
        }
        let node = definition
            .nodes
            .get(&run.node)
            .context("The workflow node is missing.")?;
        if let NodeKind::Action { action } = &node.kind {
            let visit = runner.current_visit(&run)?;
            if visit.output.is_none() {
                match (action.id.as_str(), action.number) {
                    (MANUAL, 1) => {
                        let request = runner.dispatch(&run)?;
                        run = runner
                            .get(run.id)?
                            .context("The workflow run disappeared.")?;
                        return Ok(Progress {
                            run,
                            pending_action: Some(request),
                            stopped: None,
                            budget_limit: None,
                            external_wait: None,
                            native_outcome: None,
                        });
                    }
                    (TASK_SNAPSHOT, 1) => {
                        let mut values = builtin::task_snapshot(fleet, run.task)?;
                        values.retain(|name, _| node.outputs.contains_key(name));
                        let request = runner.dispatch(&run)?;
                        runner.complete(
                            &request,
                            ActionOutput {
                                values,
                                evidence: vec![format!("task:{}", run.task)],
                                ..Default::default()
                            },
                        )?;
                        run = runner
                            .get(run.id)?
                            .context("The workflow run disappeared.")?;
                    }
                    (WORKSPACE_SETUP, 1) => {
                        use tod_store::activity::{KIND_GATE_CHECK, with_lease};
                        use tod_store::outline::repos::gate::OUTCOME_PASS;
                        let request = runner.dispatch(&run)?;
                        let result = with_lease(
                            fleet,
                            run.task,
                            KIND_GATE_CHECK,
                            "workflow workspace setup",
                            || {
                                let current = runner
                                    .get(run.id)?
                                    .context("The workflow run disappeared.")?;
                                ensure!(
                                    !current.status.is_terminal() && current.visit == request.visit,
                                    "This workflow visit is no longer current."
                                );
                                let result = builtin::workspace_setup(fleet, run.task)?;
                                if result.outcome == OUTCOME_PASS {
                                    runner.complete(
                                        &request,
                                        ActionOutput {
                                            evidence: vec![result.detail.clone()],
                                            ..Default::default()
                                        },
                                    )?;
                                }
                                Ok::<_, anyhow::Error>(result)
                            },
                        )??;
                        run = runner
                            .get(run.id)?
                            .context("The workflow run disappeared.")?;
                        if result.outcome != OUTCOME_PASS {
                            return Ok(Progress {
                                run,
                                pending_action: Some(request),
                                stopped: Some(result.detail),
                                budget_limit: None,
                                external_wait: None,
                                native_outcome: None,
                            });
                        }
                    }
                    (WORKSPACE_CAPTURE, 1) => {
                        let request = runner.dispatch(&run)?;
                        match builtin::checkout_revision(fleet, run.task) {
                            Observation::Ready { value, evidence } => runner.complete(
                                &request,
                                ActionOutput {
                                    values: BTreeMap::from([("revision".into(), value)]),
                                    evidence: vec![evidence],
                                    ..Default::default()
                                },
                            )?,
                            Observation::Waiting { reason } | Observation::Error { reason } => {
                                return Ok(Progress {
                                    run: runner
                                        .get(run.id)?
                                        .context("The workflow run disappeared.")?,
                                    pending_action: Some(request),
                                    stopped: Some(reason),
                                    budget_limit: None,
                                    external_wait: None,
                                    native_outcome: None,
                                });
                            }
                        }
                        run = runner
                            .get(run.id)?
                            .context("The workflow run disappeared.")?;
                    }
                    (MANUAL | TASK_SNAPSHOT | WORKSPACE_SETUP | WORKSPACE_CAPTURE, number) => {
                        bail!("Unsupported built-in action revision {number}.")
                    }
                    (id, 1) if native::is_native(id) => {
                        let phase = definition
                            .task_phases
                            .get(&run.node)
                            .context("This protocol action needs a mapped task phase.")?;
                        let request = runner.dispatch(&run)?;
                        match driver.native(&request, phase, visit.started_at)? {
                            native::StepResult::Complete(mut output) => {
                                let redirecting = matches!(
                                    output.values.get("redirect"),
                                    Some(Value::Text(phase)) if !phase.is_empty()
                                );
                                ensure!(
                                    !redirecting
                                        || node.outputs.get("redirect")
                                            == Some(&super::ValueType::Text),
                                    "The task returned to an earlier phase. Add a redirect output and route for this action."
                                );
                                output
                                    .values
                                    .retain(|name, _| node.outputs.contains_key(name));
                                for (name, ty) in &node.outputs {
                                    if name != "redirect" && !redirecting {
                                        continue;
                                    }
                                    output
                                        .values
                                        .entry(name.clone())
                                        .or_insert_with(|| match ty {
                                            super::ValueType::Boolean => Value::Boolean(false),
                                            super::ValueType::Text => Value::Text(String::new()),
                                            super::ValueType::Integer => Value::Integer(0),
                                        });
                                }
                                runner.complete(&request, output)?;
                                run = runner
                                    .get(run.id)?
                                    .context("The workflow run disappeared.")?;
                            }
                            native::StepResult::Suspended(outcome) => {
                                return Ok(Progress {
                                    run: runner
                                        .get(run.id)?
                                        .context("The workflow run disappeared.")?,
                                    pending_action: Some(request),
                                    stopped: match &outcome {
                                        Outcome::Stopped { reason } => Some(reason.clone()),
                                        Outcome::NeedsHuman { reason } => Some(reason.describe()),
                                        Outcome::Waiting { what, .. } => Some(what.clone()),
                                        _ => None,
                                    },
                                    budget_limit: None,
                                    external_wait: None,
                                    native_outcome: Some(outcome),
                                });
                            }
                        }
                    }
                    (id, _) => {
                        let asset = Catalog::new(fleet)
                            .action_for(&run.revision, action)?
                            .with_context(|| {
                                format!("Action {id} is unavailable in the pinned revision.")
                            })?;
                        let request = runner.dispatch(&run)?;
                        match driver.agent(&request, &asset, node)? {
                            agent::Outcome::Complete(output) => {
                                runner.complete(&request, output)?;
                                run = runner
                                    .get(run.id)?
                                    .context("The workflow run disappeared.")?;
                            }
                            agent::Outcome::Waiting { reason }
                            | agent::Outcome::Stopped { reason } => {
                                run = runner
                                    .get(run.id)?
                                    .context("The workflow run disappeared.")?;
                                return Ok(Progress {
                                    run,
                                    pending_action: Some(request),
                                    stopped: Some(reason),
                                    budget_limit: None,
                                    external_wait: None,
                                    native_outcome: None,
                                });
                            }
                        }
                    }
                }
            }
        }
        let (mut observations, reads) = builtin::observations_with_github_reads(
            fleet,
            run.task,
            &definition,
            &run.node,
            manual,
        )?;
        let retry = retry::prepare(fleet, &run, &mut observations, reads)?;
        let (trace, external_wait) =
            runner.evaluate_with_retry(&run, observations, retry, |trace| {
                driver.external_wait(&run, &definition, trace)
            })?;
        let previous_phase = definition.task_phases.get(&run.node);
        run = runner
            .get(run.id)?
            .context("The workflow run disappeared.")?;
        if let Some(phase) = definition.task_phases.get(&run.node)
            && Some(phase) != previous_phase
            && !matches!(definition.nodes[&run.node].kind, NodeKind::Finish)
            && let Some(milestone) = crate::linear_sync::Milestone::for_lifecycle(phase)
        {
            crate::linear_sync::push(fleet, run.task, milestone);
        }
        if !matches!(trace.decision, Decision::Take { .. }) {
            let native_outcome = match retry::failed_outcome(&trace) {
                Some(outcome) => Some(outcome),
                None => approval_link_failure(fleet, &run, &definition, &trace)?,
            };
            let stopped = match &native_outcome {
                Some(Outcome::Stopped { reason }) => Some(reason.clone()),
                _ => None,
            };
            return Ok(Progress {
                run,
                pending_action: None,
                stopped,
                budget_limit: None,
                external_wait,
                native_outcome,
            });
        }
    }
}

fn approval_link_failure(
    fleet: &FleetStore,
    run: &Run,
    definition: &Definition,
    trace: &Trace,
) -> Result<Option<Outcome>> {
    use crate::gate::derived::evaluate_derived_criterion;
    use tod_store::outline::repos::gate::{APPROVED_MERGED_PR_MERGED_SLUG, GateRepo, OUTCOME_FAIL};

    if !matches!(trace.decision, Decision::Waiting | Decision::Error)
        || !definition.task_phases.get(&run.node).is_some_and(|phase| phase == "approved")
        || !trace.observations.iter().any(|(name, observation)| {
            let invalid_link = match observation {
                Observation::Waiting { reason } => reason == builtin::MISSING_PR_LINKS,
                Observation::Error { reason } => reason == builtin::MALFORMED_PR_LINKS,
                Observation::Ready { .. } => false,
            };
            invalid_link && matches!(definition.fields.get(name).map(|field| &field.source),
                    Some(Source::Provider { binding, field }) if binding == builtin::GITHUB && field == "merge_confirmation")
        })
    {
        return Ok(None);
    }

    // Use the same active criterion and its detail as the native lifecycle.
    // Missing and malformed links fail before the criterion attempts a GitHub read.
    fleet.read(|conn| {
        let Some(criterion) = GateRepo::new(conn)
            .get_by_slug(APPROVED_MERGED_PR_MERGED_SLUG)?
            .filter(|criterion| criterion.active)
        else {
            return Ok(None);
        };
        let Some(result) = evaluate_derived_criterion(conn, run.task, &criterion)?
            .filter(|result| result.outcome == OUTCOME_FAIL)
        else {
            return Ok(None);
        };
        let detail = if result.detail.is_empty() {
            criterion.label
        } else {
            format!("{}: {}", criterion.label, result.detail)
        };
        Ok(Some(Outcome::NeedsHuman {
            reason: NeedsHuman::FailingCriteria {
                criteria: vec![detail],
            },
        }))
    })
}

/// The request ID and visit supplied by the interface guard against recording
/// a result after the task has moved on or been cancelled.
pub fn record_manual(
    fleet: &FleetStore,
    request: &ActionRequest,
    values: BTreeMap<String, Value>,
    evidence: Vec<String>,
) -> Result<()> {
    ensure!(
        request.action.id == MANUAL && request.action.number == 1,
        "This action cannot be completed with a manual result."
    );
    Runner::new(fleet).complete(
        request,
        ActionOutput {
            values,
            evidence,
            ..Default::default()
        },
    )
}

pub fn pinned_definition(fleet: &FleetStore, run: &Run) -> Result<Definition> {
    Catalog::new(fleet)
        .published(&run.revision.id, run.revision.number)?
        .map(|published| published.definition)
        .context("The workflow revision for this run is unavailable.")
}
