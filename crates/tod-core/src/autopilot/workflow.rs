//! Native protocol actions for an explicitly projected workflow. A protocol
//! may update its task records. Graph routes own normal phase advancement.

use super::*;
use crate::lifecycle::workflow::runner::ActionRequest;
use crate::lifecycle::workflow::{Value, native};
use std::collections::{BTreeMap, BTreeSet};

impl Autopilot {
    pub(crate) fn workflow_protocol_evidence(
        &self,
        fleet: &FleetStore,
        phase: &str,
        entered_at: i64,
        request: &ActionRequest,
        output: &mut crate::lifecycle::workflow::runner::ActionOutput,
    ) -> Result<()> {
        let mut conversations = BTreeSet::new();
        for step in self
            .state
            .steps
            .iter()
            .filter(|step| step.from == phase && step.at_ms >= entered_at)
        {
            if let Some(id) = step.conversation_id {
                conversations.insert(id);
                if step.step == "review" {
                    conversations.extend(
                        fleet.read(|conn| tod_store::review::round::conversations(conn, id))?,
                    );
                }
            }
        }
        output.assets.push(request.action.clone());
        fleet.read(|conn| {
            for id in conversations {
                let reference = format!("conversation/{id}");
                if !output.evidence.contains(&reference) {
                    output.evidence.push(reference);
                }
                let totals = tod_store::conversation_usage::conversation_totals(conn, id)?;
                let cost = tod_store::conversation_usage::conversation_cost(conn, id)?;
                output.metrics.input_tokens = output
                    .metrics
                    .input_tokens
                    .saturating_add(totals.counts.prompt());
                output.metrics.output_tokens = output
                    .metrics
                    .output_tokens
                    .saturating_add(totals.counts.output);
                output.metrics.cost_micros = output.metrics.cost_micros.saturating_add(cost.micros);
            }
            Ok(())
        })
    }

    pub(crate) fn workflow_protocol<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        request: &ActionRequest,
        expected_phase: &str,
        entered_at: i64,
    ) -> Result<native::StepResult> {
        use native::StepResult;
        let mut last = None;
        loop {
            if let Some(reason) = hook.at(fleet, Boundary::Step)? {
                return Ok(StepResult::stopped(reason));
            }
            let current = fleet.read(|conn| tod_store::workflow_run::get(conn, request.run))?;
            if !current.is_some_and(|run| !run.status.is_terminal() && run.visit == request.visit) {
                return Ok(StepResult::stopped(
                    "The task moved to a different workflow visit.",
                ));
            }
            if let Some(limit) = self.over_budget() {
                return Ok(StepResult::Suspended(Outcome::BudgetExhausted { limit }));
            }
            let phase = lifecycle::current_state(fleet, self.node)?;
            if phase != expected_phase {
                return self.workflow_redirect(fleet, &phase, expected_phase, entered_at);
            }
            if phase != "review" {
                self.state.loops.review_rounds = 0;
            }
            if let Some(outcome) = self.incident_step(fleet, agent, hook)? {
                return Ok(StepResult::Suspended(outcome));
            }
            if let Some(outcome) = self.triage_step(fleet, agent, hook)? {
                return Ok(StepResult::Suspended(outcome));
            }
            if let Some(outcome) = self.process_review_step(fleet, agent, hook)? {
                return Ok(StepResult::Suspended(outcome));
            }
            match self.reopen_lost_work(fleet, hook, &phase)? {
                ChecksStep::Proceed => {}
                ChecksStep::Fixed => {
                    return Ok(workflow_values(
                        "Lost work reopened",
                        [("redirect", Value::Text("active".into()))],
                    ));
                }
                ChecksStep::Stop(outcome) => return Ok(StepResult::Suspended(outcome)),
            }

            fleet.interview(
                "autopilot",
                InterviewCommand::RestoreCertificate { node_id: self.node },
            )?;
            let mut pending = fleet.read(|conn| {
                Ok(DecisionRepo::new(conn)
                    .list_pending_for_node(self.node)?
                    .len())
            })?;
            if pending > 0 && matches!(phase.as_str(), "pr_draft" | "pr_review") {
                if let Some(outcome) = self.withdraw_draft_bot_requests(fleet)? {
                    return Ok(StepResult::Suspended(outcome));
                }
                let (outcome, open_silence) = self.tend_silence_questions(fleet)?;
                if let Some(outcome) = outcome {
                    return Ok(StepResult::Suspended(outcome));
                }
                let (outcome, open_checks) = self.tend_unrelated_check_questions(fleet)?;
                if let Some(outcome) = outcome {
                    return Ok(StepResult::Suspended(outcome));
                }
                pending = fleet.read(|conn| {
                    Ok(DecisionRepo::new(conn)
                        .list_pending_for_node(self.node)?
                        .len())
                })?;
                pending = pending
                    .saturating_sub(open_silence)
                    .saturating_sub(open_checks);
            }
            if pending > 0 {
                return Ok(StepResult::Suspended(Outcome::NeedsHuman {
                    reason: NeedsHuman::Decision { pending },
                }));
            }
            if self.state.pending_groups.is_some() {
                if let Some(outcome) = self.run_pending_groups(fleet, agent, hook, &phase)? {
                    return Ok(StepResult::Suspended(outcome));
                }
                last = None;
            }
            match self.checks_round(fleet, agent, hook, &phase)? {
                ChecksStep::Proceed => {}
                ChecksStep::Fixed => {
                    last = None;
                    continue;
                }
                ChecksStep::Stop(outcome) => return Ok(StepResult::Suspended(outcome)),
            }

            match request.action.id.as_str() {
                native::REVIEW_INPUT => {
                    use crate::lifecycle::workflow::{Observation, builtin};
                    return Ok(match builtin::checkout_revision(fleet, self.node) {
                        Observation::Ready { value, evidence } => StepResult::complete(
                            vec![evidence],
                            BTreeMap::from([("revision".into(), value)]),
                        ),
                        Observation::Waiting { reason } | Observation::Error { reason } => {
                            StepResult::stopped(reason)
                        }
                    });
                }
                native::OPEN_PR => {
                    if let Some(outcome) = self.certify_draft_pr(fleet, agent, hook)? {
                        return self.workflow_suspend(fleet, &phase, entered_at, outcome);
                    }
                    if self.state.pending_groups.is_some() {
                        continue;
                    }
                    return self.workflow_gate(fleet, hook, &phase);
                }
                native::TEND_PR => {
                    if let Some(outcome) = self.babysit(fleet, agent, hook, phase == "pr_draft")? {
                        return self.workflow_suspend(fleet, &phase, entered_at, outcome);
                    }
                    if phase == "pr_draft"
                        && let Some(outcome) = self.hold_for_github(fleet, &phase)?
                    {
                        return Ok(StepResult::Suspended(outcome));
                    }
                    return self.workflow_gate(fleet, hook, &phase);
                }
                native::REVIEW | native::FIX => {
                    return self.workflow_review(fleet, agent, hook, request, entered_at);
                }
                _ => {}
            }

            let standing = fleet.read(|conn| Standing::load(conn, self.node, &phase))?;
            let Some(step) = next_step(&standing) else {
                let reason = if standing.steps_need_user > 0 {
                    NeedsHuman::BlockedSteps {
                        count: standing.steps_need_user,
                    }
                } else {
                    NeedsHuman::NoPlan
                };
                return Ok(StepResult::Suspended(Outcome::NeedsHuman { reason }));
            };
            if last.as_ref() == Some(&(standing.clone(), step)) {
                return Ok(StepResult::Suspended(Outcome::NeedsHuman {
                    reason: NeedsHuman::NoProgress {
                        step: step_name(step).into(),
                    },
                }));
            }
            last = Some((standing, step));
            let (outcome, groups) = match step {
                NextStep::GateCheck => return self.workflow_gate(fleet, hook, &phase),
                NextStep::Phase => (
                    self.phase(fleet, agent, hook, &phase)?,
                    Some((ProtocolKind::Phase, phase.as_str())),
                ),
                NextStep::Evaluate => (self.evaluate(fleet, agent, hook, &phase)?, None),
                NextStep::Implement => (
                    self.converse(fleet, agent, hook, ProtocolKind::Implementation, None)?,
                    Some((ProtocolKind::Implementation, "implement")),
                ),
                NextStep::Verify => (
                    self.converse(fleet, agent, hook, ProtocolKind::Verification, None)?,
                    Some((ProtocolKind::Verification, "verify")),
                ),
                NextStep::FixFailed => {
                    let failing = fleet.read(|conn| {
                        Ok(tod_store::verification::VerdictRepo::new(conn)
                            .standings(self.node)?
                            .into_iter()
                            .filter(|s| {
                                s.is_failed()
                                    && (s.phase
                                        == tod_store::outline::repos::plan_steps::PHASE_ACTIVE
                                        || s.due_by(&phase))
                            })
                            .map(|s| (s.obligation.id.to_string(), s.obligation.body))
                            .collect::<Vec<_>>())
                    })?;
                    if let Some(reason) = cycle_stop(&mut self.state.loops, &phase, &failing) {
                        return Ok(StepResult::Suspended(Outcome::NeedsHuman { reason }));
                    }
                    self.state.loops.fix_cycles += 1;
                    self.record(fleet, "fix_failed", &phase, None)?;
                    return Ok(workflow_values(
                        "Verification recorded failed plan work",
                        [("verified", Value::Boolean(false))],
                    ));
                }
                _ => {
                    return Ok(StepResult::stopped(
                        "The task reached a step owned by a different protocol action.",
                    ));
                }
            };
            if let Some(outcome) = outcome {
                return self.workflow_suspend(fleet, &phase, entered_at, outcome);
            }
            if let Some((protocol, key)) = groups {
                self.owe_groups(fleet, protocol, key, &phase)?;
            }
        }
    }

    fn workflow_gate<H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        hook: &mut H,
        phase: &str,
    ) -> Result<native::StepResult> {
        let settings =
            tod_store::TodSettings::load(&tod_store::TodPaths::at(&self.config.data_root))?;
        let gate = crate::phase::settle_gate_notifying(
            fleet,
            self.node,
            &settings.branch_template,
            &mut |what| hook.working_on(what),
        )?;
        if gate.from != phase {
            return Ok(native::StepResult::stopped(format!(
                "The task moved to {} before the {phase} gate finished.",
                gate.from
            )));
        }
        if !gate.clear() {
            let criteria = gate
                .failing()
                .map(|check| format!("{}: {}", check.criterion.label, check.detail))
                .collect();
            return Ok(native::StepResult::Suspended(Outcome::NeedsHuman {
                reason: NeedsHuman::FailingCriteria { criteria },
            }));
        }
        let evidence = gate
            .criteria
            .iter()
            .map(|check| {
                format!(
                    "{} · {} · {}",
                    check.criterion.label, check.outcome, check.detail
                )
            })
            .collect();
        let mut values = BTreeMap::new();
        if phase == "verifying" {
            values.insert("verified".into(), Value::Boolean(true));
        }
        hook.wait_over();
        Ok(native::StepResult::complete(evidence, values))
    }

    fn workflow_suspend(
        &mut self,
        fleet: &FleetStore,
        phase: &str,
        entered_at: i64,
        outcome: Outcome,
    ) -> Result<native::StepResult> {
        let current = lifecycle::current_state(fleet, self.node)?;
        if current != phase {
            return self.workflow_redirect(fleet, &current, phase, entered_at);
        }
        Ok(native::StepResult::Suspended(outcome))
    }

    fn workflow_redirect(
        &mut self,
        fleet: &FleetStore,
        current: &str,
        expected: &str,
        entered_at: i64,
    ) -> Result<native::StepResult> {
        let recorded = fleet.read(|conn| {
            Ok(tod_store::send_back::SendBackRepo::new(conn)
                .list_for_node(self.node)?
                .into_iter()
                .any(|row| {
                    row.created_at >= entered_at
                        && row.moved()
                        && row.from_state == expected
                        && row.to_state == current
                }))
        })?;
        let reopened = current == "active"
            && self.state.steps.iter().any(|step| {
                step.at_ms >= entered_at
                    && step.from == expected
                    && step.to == current
                    && step.step == "lost_work"
            });
        if recorded || reopened {
            self.sent_back = false;
            Ok(workflow_values(
                if reopened {
                    "Lost work reopened"
                } else {
                    "Recorded task send-back"
                },
                [("redirect", Value::Text(current.into()))],
            ))
        } else {
            Ok(native::StepResult::stopped(format!(
                "This action expects the task in {expected}. The task is in {current}."
            )))
        }
    }

    fn workflow_review<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        request: &ActionRequest,
        entered_at: i64,
    ) -> Result<native::StepResult> {
        let phase = "review";
        if let Some(outcome) =
            self.escalate_fixes(fleet, phase, self.state.loops.review_floor_ms, false)?
        {
            return self.workflow_suspend(fleet, phase, entered_at, outcome);
        }
        let is_review = request.action.id == native::REVIEW;
        let protocol = if is_review {
            ProtocolKind::Review
        } else {
            ProtocolKind::Fix
        };
        let label = protocol_name(protocol);
        let started = self
            .state
            .steps
            .iter()
            .rev()
            .find(|step| step.at_ms >= entered_at && step.from == phase && step.step == label)
            .cloned();
        if is_review {
            let floor = fleet
                .read(|conn| PhaseRepo::new(conn).stay_started_at(self.node))?
                .unwrap_or(entered_at)
                .max(self.state.loops.review_floor_ms);
            let rounds = crate::lifecycle::workflow::runner::Runner::new(fleet)
                .visits(request.run)?
                .iter()
                .filter(|visit| {
                    visit.number < request.visit
                        && visit.started_at >= floor
                        && visit.output.is_some()
                        && visit
                            .dispatch
                            .as_ref()
                            .is_some_and(|dispatch| dispatch.action.id == native::REVIEW)
                })
                .count();
            self.state.loops.review_rounds = u32::try_from(rounds).unwrap_or(u32::MAX);
        }
        if started.is_none() {
            if is_review && let Some(reason) = self.review_limit_reached(fleet)? {
                return Ok(native::StepResult::Suspended(Outcome::NeedsHuman {
                    reason,
                }));
            }
            if let Some(outcome) = self.converse(fleet, agent, hook, protocol, None)? {
                return self.workflow_suspend(fleet, phase, entered_at, outcome);
            }
            self.owe_groups(
                fleet,
                protocol,
                if is_review { "review" } else { "fix" },
                phase,
            )?;
        }
        if let Some(outcome) = self.run_pending_groups(fleet, agent, hook, phase)? {
            return self.workflow_suspend(fleet, phase, entered_at, outcome);
        }
        let evidence = self
            .state
            .steps
            .iter()
            .rev()
            .find(|step| step.from == phase && step.step == label)
            .and_then(|step| step.conversation_id)
            .map(|id| format!("conversation/{id}"))
            .into_iter()
            .collect();
        if !is_review {
            return Ok(native::StepResult::complete(evidence, BTreeMap::new()));
        }
        self.state.loops.review_rounds = self.state.loops.review_rounds.saturating_add(1);
        self.save()?;
        if let Some(outcome) = self.settle_review_round(
            fleet,
            agent,
            hook,
            phase,
            self.state.loops.review_rounds,
            quick_fix::Lanes::All,
        )? {
            return self.workflow_suspend(fleet, phase, entered_at, outcome);
        }
        if fleet.read(|conn| crate::pr_draft_review::enabled(conn, self.node))? {
            let cwd =
                tod_store::fleet::provision::resolve_launch_cwd(fleet, &self.node.to_string())
                    .context("finding the task checkout to record the review's heads")?;
            crate::pr_draft_review::record_review_phase_heads(fleet, self.node, &cwd)?;
        }
        let standing = fleet.read(|conn| Standing::load(conn, self.node, phase))?;
        if !standing.review_done {
            return Ok(native::StepResult::stopped(
                "The review round has no recorded result.",
            ));
        }
        let clear = standing.review_clean && standing.open_findings == 0;
        let mut evidence: Vec<String> = evidence;
        if clear {
            match self.workflow_gate(fleet, hook, phase)? {
                native::StepResult::Complete(gate) => evidence.extend(gate.evidence),
                suspended => return Ok(suspended),
            }
        }
        Ok(native::StepResult::complete(
            evidence,
            BTreeMap::from([
                (
                    "open_findings".into(),
                    Value::Integer(i64::try_from(standing.open_findings).unwrap_or(i64::MAX)),
                ),
                ("review_clear".into(), Value::Boolean(clear)),
            ]),
        ))
    }
}

fn workflow_values<const N: usize>(
    evidence: &str,
    values: [(&str, Value); N],
) -> native::StepResult {
    native::StepResult::complete(
        vec![evidence.into()],
        values
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    )
}
