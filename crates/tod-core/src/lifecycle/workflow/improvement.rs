//! Replay candidate routing rules over evidence from a durable workflow run.
//! A replay never dispatches work or reads a current integration.

use super::{
    Decision, Definition, NodeKind, Revision, Trace,
    actions::RuntimeContext,
    catalog::Catalog,
    execute::pinned_definition,
    native,
    runner::{Runner, Visit},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use tod_store::{
    conversation::{ConversationRepo, Focus},
    fleet::FleetStore,
    interview::{ACTOR_USER, InterviewCommand},
    lessons::Op,
};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CapturedContext {
    pub conversation: Uuid,
    pub turn: i64,
    pub text: String,
}

/// Load the recorded launch context even if the provider has not returned a
/// result. Regenerating it would substitute today's task and workspace data.
pub fn captured_context(fleet: &FleetStore, run: Uuid, visit: u64) -> Result<Vec<CapturedContext>> {
    use crate::conversation::workflow_context::SNAPSHOT_HEADING;
    use tod_store::workflow_run::{self, agent_conversation_id};
    fleet.read(|conn| {
        let run_row = workflow_run::get(conn, run)?.context("Workflow run not found.")?;
        let found: Visit = workflow_run::visit(conn, run, visit)?
            .context("Workflow visit not found.")?
            .try_into()?;
        let native = found
            .dispatch
            .as_ref()
            .is_some_and(|request| native::is_native(&request.action.id));
        let mut references = std::collections::BTreeSet::new();
        if native {
            if let Some(output) = found.output {
                references.extend(
                    output.evidence.iter().filter_map(|item| {
                        Uuid::parse_str(item.strip_prefix("conversation/")?).ok()
                    }),
                );
            }
        } else {
            references.insert(agent_conversation_id(run, visit));
        }
        let repo = ConversationRepo::new(conn);
        let mut captures = Vec::new();
        for conversation in references {
            if !repo
                .get(conversation)?
                .is_some_and(|row| row.focus == Focus::Node(run_row.task))
            {
                continue;
            }
            for turn in repo.turns(conversation)? {
                if let Some(text) = turn.sent_context
                    && (native || text.lines().any(|line| line == SNAPSHOT_HEADING))
                {
                    captures.push(CapturedContext {
                        conversation,
                        turn: turn.seq,
                        text,
                    });
                }
            }
        }
        Ok(captures)
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Comparison {
    pub run: Uuid,
    pub original: Revision,
    pub candidate: Revision,
    pub visits: Vec<ComparedVisit>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ComparedVisit {
    pub sequence: u64,
    pub recorded: Trace,
    /// A removed node has no candidate decision.
    pub replay: Option<Trace>,
    /// Changed bindings and newly referenced fields need fresh evidence.
    pub missing_fields: Vec<String>,
    pub changed: Option<bool>,
}

pub fn compare(fleet: &FleetStore, run_id: Uuid, candidate: &Definition) -> Result<Comparison> {
    let runner = Runner::new(fleet);
    let run = runner.get(run_id)?.context("Workflow run not found.")?;
    let original = pinned_definition(fleet, &run)?;
    let workflow = candidate.validate().map_err(|diagnostics| {
        anyhow::anyhow!(
            "The candidate workflow is invalid. {}",
            diagnostics
                .iter()
                .map(|d| format!("{}: {}", d.location, d.message))
                .collect::<Vec<_>>()
                .join(" ")
        )
    })?;
    let visits = runner
        .traces(run_id)?
        .into_iter()
        .map(|entry| {
            let recorded = entry.trace;
            if matches!(recorded.decision, Decision::OperatorMove { .. }) {
                return ComparedVisit {
                    sequence: entry.sequence,
                    recorded,
                    replay: None,
                    missing_fields: Vec::new(),
                    changed: None,
                };
            }
            let observations = recorded
                .observations
                .iter()
                .filter(|(name, _)| {
                    candidate.fields.get(*name) == original.fields.get(*name)
                        && candidate.fields.contains_key(*name)
                })
                .map(|(name, observation)| (name.clone(), observation.clone()))
                .collect();
            let replay = workflow.decide(&recorded.node, recorded.visit, &observations);
            let missing_fields: Vec<_> = replay
                .iter()
                .flat_map(|trace| trace.observations.keys())
                .filter(|name| !observations.contains_key(*name))
                .cloned()
                .collect();
            let changed = replay.as_ref().and_then(|trace| {
                (missing_fields.is_empty()
                    && !matches!(trace.decision, Decision::Waiting | Decision::Error))
                .then_some(trace.decision != recorded.decision)
            });
            ComparedVisit {
                sequence: entry.sequence,
                recorded,
                replay,
                missing_fields,
                changed,
            }
        })
        .collect();
    Ok(Comparison {
        run: run_id,
        original: original.revision,
        candidate: candidate.revision.clone(),
        visits,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Subject {
    pub key: String,
    pub label: String,
    pub target: String,
    pub revision: Option<Revision>,
}

pub fn subjects(
    fleet: &FleetStore,
    definition: &Definition,
    trace: &Trace,
) -> Result<Vec<Subject>> {
    let node = definition
        .nodes
        .get(&trace.node)
        .context("Workflow node not found.")?;
    let target = format!("workflow:{}/{}", definition.revision.id, trace.node);
    let mut subjects = vec![Subject {
        key: "node".into(),
        label: node.label.clone(),
        target: target.clone(),
        revision: Some(definition.revision.clone()),
    }];
    for route in &trace.routes {
        subjects.push(Subject {
            key: format!("gate:{}", route.route),
            label: format!("Gate · {}", route.route),
            target: format!("{target}/routes/{}", route.route),
            revision: Some(definition.revision.clone()),
        });
    }
    for field in trace.observations.keys() {
        subjects.push(Subject {
            key: format!("field:{field}"),
            label: format!("Data source · {field}"),
            target: format!("workflow:{}/fields/{field}", definition.revision.id),
            revision: Some(definition.revision.clone()),
        });
    }
    if let NodeKind::Action { action } = &node.kind
        && native::is_native(&action.id)
    {
        subjects.push(Subject {
            key: "protocol-context".into(),
            label: "Recorded protocol context".into(),
            target: format!("{target}/protocol-context"),
            revision: Some(action.clone()),
        });
    }
    if let NodeKind::Action { action } = &node.kind
        && let Some(asset) = Catalog::new(fleet).action_for(&definition.revision, action)?
    {
        let root = format!("workflow-action:{}", asset.revision.id);
        for (key, label) in [("prompt", "Prompt"), ("agent", "Agent settings")] {
            subjects.push(Subject {
                key: key.into(),
                label: label.into(),
                target: format!("{root}/{key}"),
                revision: Some(asset.revision.clone()),
            });
        }
        for (part, title, instructions) in [
            ("skill", "Skill", &asset.spec.skills),
            ("context", "Context", &asset.spec.context),
        ] {
            for (index, instruction) in instructions.iter().enumerate() {
                let (target, revision) = match &instruction.source {
                    Some(source) => (format!("{part}:{}", source.id), source.clone()),
                    None => (
                        format!("{root}/{part}/{}", instruction.title),
                        asset.revision.clone(),
                    ),
                };
                subjects.push(Subject {
                    key: format!("{part}:{index}"),
                    label: format!("{title} · {}", instruction.title),
                    target,
                    revision: Some(revision),
                });
            }
        }
        for (index, source) in asset.spec.runtime_context.iter().enumerate() {
            let name = match source {
                RuntimeContext::Task => "Task description".into(),
                RuntimeContext::Obligations { phase } => phase.as_ref().map_or_else(
                    || "Obligations".into(),
                    |phase| format!("{phase} obligations"),
                ),
                RuntimeContext::InheritedContext => "Inherited context".into(),
                RuntimeContext::Plan => "Plan steps".into(),
                RuntimeContext::ObligationVerdicts => "Obligation verdicts".into(),
                RuntimeContext::ReviewFindings => "Review findings".into(),
                RuntimeContext::ReviewerAssignment { reviewer } => reviewer.as_ref().map_or_else(
                    || "Solo reviewer assignment".into(),
                    |reviewer| format!("Assignment for the {}", reviewer.title()),
                ),
                RuntimeContext::IncomingChanges => "Incoming changes".into(),
                RuntimeContext::TriageRequest => "Triage request".into(),
                RuntimeContext::IncidentSamples => "Incident samples".into(),
                RuntimeContext::ProcessReviewLessons => "Process review lessons".into(),
                RuntimeContext::WorkHistory => "Work history".into(),
                RuntimeContext::PullRequests => "Linked pull requests".into(),
                RuntimeContext::WorkingDirectory => "Working directory".into(),
                RuntimeContext::Checks { phase } => format!("{phase} checks"),
                RuntimeContext::ConfiguredSkills { phase, group } => {
                    format!("{phase} skills, group {}", group.saturating_add(1))
                }
                RuntimeContext::NodeOutput { node, field } => field.as_ref().map_or_else(
                    || format!("{node} outputs"),
                    |field| format!("{node} output · {field}"),
                ),
            };
            subjects.push(Subject {
                key: format!("runtime-context:{index}"),
                label: format!("Runtime context · {name}"),
                target: format!("{root}/runtime-context/{}", serde_json::to_string(source)?),
                revision: Some(asset.revision.clone()),
            });
        }
    }
    Ok(subjects)
}

/// Attach a lesson to a component of an immutable decision. Process review can
/// group it with other evidence about the same node, route, source, or asset.
pub fn record_subject_lesson(
    fleet: &FleetStore,
    run_id: Uuid,
    sequence: u64,
    key: &str,
    text: &str,
) -> Result<Uuid> {
    ensure!(!text.trim().is_empty(), "Describe what should improve.");
    let runner = Runner::new(fleet);
    let run = runner.get(run_id)?.context("Workflow run not found.")?;
    let trace = runner
        .traces(run_id)?
        .into_iter()
        .find(|entry| entry.sequence == sequence)
        .context("Workflow decision trace not found.")?;
    let definition = pinned_definition(fleet, &run)?;
    let subject = subjects(fleet, &definition, &trace.trace)?
        .into_iter()
        .find(|subject| subject.key == key)
        .context("This workflow component is absent from the recorded decision.")?;
    let target = subject.target;
    let revision = subject
        .revision
        .map(|revision| format!("{} revision {}", revision.id, revision.number))
        .unwrap_or_default();
    let evidence = format!(
        "{} · {}\nWorkflow {} revision {}, run {}, visit {}, decision {}",
        subject.label,
        revision,
        run.revision.id,
        run.revision.number,
        run_id,
        trace.trace.visit,
        sequence
    );
    let result = fleet.interview(
        ACTOR_USER,
        InterviewCommand::Lessons(Op::Add {
            node: run.task,
            text: format!("{}\n\n{evidence}", text.trim()),
            category: None,
            target,
        }),
    )?;
    serde_json::from_value(result["id"].clone()).context("The lesson write returned no ID.")
}
