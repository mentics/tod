//! Resolve the pinned context selectors of a workflow action at launch time.

use crate::context_recipes::{CHAT, ContextRecipe};
use crate::dynamic::{self, DynamicBlock as Block, DynamicContext, NodeSelection};
use crate::gate::PlanStepWithLinks;
use crate::lifecycle::workflow::{
    actions::RuntimeContext,
    runner::{ActionRequest, Runner},
};
use anyhow::{Context, Result};
use std::{fmt::Write, path::PathBuf};
use tod_store::{
    decisions::DecisionRepo,
    fleet::{FleetStore, Workdir},
    incoming::IncomingRepo,
    outline::{
        EXTRA_CONTENT_DETAILS,
        repos::{NodeRepo, ObligationRepo, PlanStepRepo},
    },
    review::{ReviewFinding, ReviewRepo},
    verification::VerdictRepo,
};

pub(crate) const SNAPSHOT_HEADING: &str = "# Workflow context snapshot";

#[derive(Debug, thiserror::Error)]
#[error("{0:#}")]
pub(crate) struct ContextUnavailable(pub anyhow::Error);

const WORKFLOW_ACTION: ContextRecipe = ContextRecipe {
    name: "workflow action",
    layers: CHAT.layers,
    situational: false,
    // Connection and transport information remains available. Task records
    // come from the workflow's selection instead of the chat focus block.
    blocks: &[Block::Databases, Block::DataRoot, Block::Environment],
};

pub(crate) struct WorkflowContext {
    pub request: ActionRequest,
    pub selectors: Vec<RuntimeContext>,
}

fn render(block: Block, context: DynamicContext<'_>) -> String {
    let mut text = String::new();
    dynamic::render_block(block, &context, &mut text);
    text
}

impl WorkflowContext {
    pub fn recipe(&self) -> Option<&'static ContextRecipe> {
        (!self.selectors.is_empty()).then_some(&WORKFLOW_ACTION)
    }

    pub fn render(&self, fleet: &FleetStore, workdir: &Workdir) -> Result<String> {
        if self.selectors.is_empty() {
            return Ok(String::new());
        }
        let mut snapshot = format!(
            "{SNAPSHOT_HEADING}\n\nTask `{}`, workflow run `{}`, visit {}.\n",
            self.request.task, self.request.run, self.request.visit
        );
        for selector in &self.selectors {
            let source = serde_json::to_string(selector)?;
            let value = self
                .resolve(fleet, workdir, selector)
                .with_context(|| format!("resolve workflow context {source}"))?;
            let value = value.trim();
            writeln!(
                snapshot,
                "\nWorkflow context source: `{source}`\n\n{}",
                if value.is_empty() {
                    "(No current records.)"
                } else {
                    value
                }
            )?;
        }
        Ok(snapshot)
    }

    fn resolve(
        &self,
        fleet: &FleetStore,
        workdir: &Workdir,
        selector: &RuntimeContext,
    ) -> Result<String> {
        let task = self.request.task;
        Ok(match selector {
            RuntimeContext::Task => fleet.read(|conn| {
                let repo = NodeRepo::new(conn);
                let task_node = repo.get(task)?.context("Workflow task no longer exists.")?;
                let node = NodeSelection {
                    id: task_node.id,
                    title: task_node.title,
                    slug: Some(task_node.slug),
                    body: repo.get_extra_content(task, EXTRA_CONTENT_DETAILS)?,
                    // The generic run, not the independent legacy state, identifies the step.
                    lifecycle: Some(self.request.node.clone()),
                };
                Ok(render(
                    Block::Node,
                    DynamicContext {
                        node: Some(&node),
                        ..Default::default()
                    },
                ))
            })?,
            RuntimeContext::WorkingDirectory => render(
                Block::WorkingDirectory,
                DynamicContext {
                    working_dir: Some(&PathBuf::from(workdir.path_text())),
                    ..Default::default()
                },
            ),
            RuntimeContext::Obligations { phase } => fleet.read(|conn| {
                let mut obligations = ObligationRepo::new(conn).list_for_node(task)?;
                if let Some(phase) = phase {
                    obligations.retain(|obligation| obligation.phase == *phase);
                }
                Ok(render(
                    Block::NodeObligations { note: "" },
                    DynamicContext {
                        obligations: &obligations,
                        ..Default::default()
                    },
                ))
            })?,
            RuntimeContext::InheritedContext => fleet.read(|conn| {
                crate::node_context::render_inherited_context(
                    conn,
                    &NodeRepo::new(conn),
                    task,
                    None,
                )
            })?,
            RuntimeContext::Plan => fleet.read(|conn| {
                let repo = PlanStepRepo::new(conn);
                let steps = repo
                    .list_for_node(task)?
                    .into_iter()
                    .map(|step| {
                        Ok(PlanStepWithLinks {
                            depends_on: repo.list_dependencies(step.id)?,
                            satisfies: repo.list_obligations(step.id)?,
                            step,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(render(
                    Block::Plan,
                    DynamicContext {
                        plan_steps: &steps,
                        ..Default::default()
                    },
                ))
            })?,
            RuntimeContext::ObligationVerdicts => fleet.read(|conn| {
                let mut verdicts = VerdictRepo::new(conn)
                    .latest_for_node(task)?
                    .into_values()
                    .collect::<Vec<_>>();
                verdicts.sort_by_key(|verdict| verdict.id);
                Ok(render(
                    Block::ObligationVerdicts,
                    DynamicContext {
                        verdicts: &verdicts,
                        ..Default::default()
                    },
                ))
            })?,
            RuntimeContext::ReviewFindings => fleet.read(|conn| {
                let findings = ReviewRepo::new(conn)
                    .list_for_node(task)?
                    .into_iter()
                    .filter(ReviewFinding::is_open)
                    .collect::<Vec<_>>();
                Ok(render(
                    Block::ReviewFindings,
                    DynamicContext {
                        findings: &findings,
                        ..Default::default()
                    },
                ))
            })?,
            RuntimeContext::ReviewerAssignment { reviewer } => {
                let mut text = super::review::render_assignment(*reviewer, false);
                if let Ok(head) = workdir.git(&["rev-parse", "--verify", "HEAD"]) {
                    writeln!(text, "\nCheckout commit at launch: `{}`.", head.trim())?;
                }
                let findings = fleet.read(|conn| ReviewRepo::new(conn).list_for_node(task))?;
                text.push_str(&super::review::known_causes(&findings));
                text
            }
            RuntimeContext::IncomingChanges => fleet.read(|conn| {
                let snapshot = conn.unchecked_transaction()?;
                let incoming = crate::incoming::items(&snapshot, task)?;
                let mut text = render(
                    Block::IncomingChanges,
                    DynamicContext {
                        incoming: &incoming,
                        ..Default::default()
                    },
                );
                let entries = IncomingRepo::new(&snapshot).pending(task)?;
                if !entries.is_empty() {
                    let ids = entries
                        .into_iter()
                        .map(|entry| entry.action_id)
                        .collect::<Vec<_>>();
                    writeln!(
                        text,
                        "\nIncoming action IDs: `{}`.",
                        serde_json::to_string(&ids)?
                    )?;
                }
                Ok(text)
            })?,
            RuntimeContext::TriageRequest => fleet.read(|conn| {
                let Some(decision) = DecisionRepo::new(conn)
                    .list_triage_for_node(task)?
                    .into_iter()
                    .next()
                else {
                    return Ok(String::new());
                };
                let request = format!(
                    "Decision `{}`.\n\n{}",
                    decision.id,
                    super::triage::render(conn, &decision)?
                );
                Ok(render(
                    Block::TriageRequest,
                    DynamicContext {
                        triage: &request,
                        ..Default::default()
                    },
                ))
            })?,
            RuntimeContext::IncidentSamples => fleet.read(|conn| {
                let Some(incident) = super::incident::claimed_for(conn, task)? else {
                    return Ok(String::new());
                };
                let samples = super::incident::render(&incident);
                Ok(render(
                    Block::IncidentSamples,
                    DynamicContext {
                        incident: &samples,
                        ..Default::default()
                    },
                ))
            })?,
            RuntimeContext::ProcessReviewLessons => fleet.read(|conn| {
                let snapshot = conn.unchecked_transaction()?;
                if tod_store::lessons::for_run(&snapshot, task)?.is_empty() {
                    return Ok(String::new());
                }
                let lessons = crate::process_review::render_context(&snapshot, task)?;
                Ok(render(
                    Block::ProcessReviewLessons,
                    DynamicContext {
                        process_review: &lessons,
                        ..Default::default()
                    },
                ))
            })?,
            RuntimeContext::WorkHistory => {
                fleet.read(|conn| crate::node_context::render_work_history(conn, task))?
            }
            RuntimeContext::PullRequests => {
                fleet.read(|conn| crate::pr_context::render(conn, task))?
            }
            RuntimeContext::ConfiguredSkills { phase, group } => {
                crate::skills_context::for_work(fleet, task, phase, workdir, *group)?
            }
            RuntimeContext::Checks { phase } => super::fix::checks_block(fleet, task, phase)?,
            RuntimeContext::NodeOutput { node, field } => {
                let visit = Runner::new(fleet)
                    .visits(self.request.run)?
                    .into_iter()
                    .rev()
                    .find(|visit| {
                        visit.number < self.request.visit
                            && visit.node == *node
                            && visit.output.is_some()
                    });
                let Some(visit) = visit else {
                    return Ok(String::new());
                };
                let mut output = visit
                    .output
                    .context("Expected recorded workflow action output.")?;
                if let Some(field) = field {
                    output.values.retain(|key, _| key == field);
                    if output.values.is_empty() {
                        return Ok(String::new());
                    }
                }
                format!(
                    "Workflow run `{}` visit {} action output.\n\n```json\n{}\n```",
                    self.request.run,
                    visit.number,
                    serde_json::to_string_pretty(&output.values)?
                )
            }
        })
    }
}
