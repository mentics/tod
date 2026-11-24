//! User-authored action instructions and agent settings pinned to a revision.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use tod_agent::{AgentPlatform, agent_launch};
use tod_store::{
    fleet::FleetStore,
    interview::{ACTOR_USER, InterviewCommand},
    workflow_actions::{self, AssetRow, Publish},
};

use super::Revision;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instruction {
    pub title: String,
    pub text: String,
    /// Optional provenance for captured text from another versioned asset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Revision>,
}

/// Ordered dynamic sources resolved for an individual workflow visit. The
/// descriptors travel with the pinned action revision. The runner records the
/// resolved inputs and their evidence separately from the authored definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeContext {
    Task,
    Obligations {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    InheritedContext,
    Plan,
    ObligationVerdicts,
    ReviewFindings,
    ReviewerAssignment {
        /// Omitted for a solo reviewer across all lenses.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reviewer: Option<tod_store::review::Reviewer>,
    },
    IncomingChanges,
    /// The oldest decision still waiting for triage on the task.
    TriageRequest,
    /// Samples from the active, non-notice incident claimed for the task.
    IncidentSamples,
    /// Lessons grouped into this task for a process review.
    ProcessReviewLessons,
    WorkHistory,
    PullRequests,
    WorkingDirectory,
    Checks {
        phase: String,
    },
    ConfiguredSkills {
        phase: String,
        /// Zero-based group in the task's effective lifecycle skill recipe.
        #[serde(default)]
        group: usize,
    },
    NodeOutput {
        node: String,
        /// Omitted to include all outputs from the applicable earlier visit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAction {
    pub title: String,
    pub prompt: String,
    #[serde(default)]
    pub skills: Vec<Instruction>,
    #[serde(default)]
    pub context: Vec<Instruction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_context: Vec<RuntimeContext>,
    pub provider: AgentPlatform,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedAction {
    pub revision: Revision,
    pub spec: AgentAction,
}

pub struct ActionCatalog<'a> {
    fleet: &'a FleetStore,
}

impl<'a> ActionCatalog<'a> {
    pub fn new(fleet: &'a FleetStore) -> Self {
        Self { fleet }
    }

    pub fn list(&self) -> Result<Vec<PublishedAction>> {
        self.fleet
            .read(workflow_actions::list)?
            .into_iter()
            .map(read_action)
            .collect()
    }

    pub fn published(&self, action: &Revision) -> Result<Option<PublishedAction>> {
        self.fleet
            .read(|conn| workflow_actions::published(conn, &action.id, action.number))?
            .map(read_action)
            .transpose()
    }

    pub fn revisions(&self, id: &str) -> Result<Vec<PublishedAction>> {
        self.fleet
            .read(|conn| workflow_actions::revisions(conn, id))?
            .into_iter()
            .map(read_action)
            .collect()
    }

    /// Publish a new asset, or append to an existing asset after checking the
    /// last revision read by the editor. Each publication pins the current
    /// model/effort defaults and captures the ordered instruction text.
    pub fn publish(
        &self,
        id: Option<&str>,
        expected_latest: Option<u64>,
        spec: AgentAction,
    ) -> Result<PublishedAction> {
        let id = match id {
            Some(id) => {
                ensure!(
                    !id.trim().is_empty() && expected_latest.is_some_and(|n| n > 0),
                    "Choose an existing action and its latest revision."
                );
                id.into()
            }
            None => {
                ensure!(
                    expected_latest.is_none(),
                    "A new action has no prior revision."
                );
                uuid::Uuid::new_v4().to_string()
            }
        };
        let spec = checked_spec(spec)?;
        let request = Publish {
            id,
            expected_latest,
            spec: serde_json::to_string(&spec)?,
        };
        let response = self.fleet.interview(
            ACTOR_USER,
            InterviewCommand::PublishWorkflowAction { request },
        )?;
        read_action(serde_json::from_value(response)?)
    }
}

fn checked_spec(mut spec: AgentAction) -> Result<AgentAction> {
    spec.title = spec.title.trim().into();
    ensure!(!spec.title.is_empty(), "Give the action a title.");
    ensure!(!spec.prompt.trim().is_empty(), "Give the action a prompt.");
    for (kind, instructions) in [("Skill", &spec.skills), ("Context", &spec.context)] {
        for (index, instruction) in instructions.iter().enumerate() {
            let n = index + 1;
            ensure!(
                !instruction.title.trim().is_empty(),
                "{kind} {n} needs a title."
            );
            ensure!(
                !instruction.text.trim().is_empty(),
                "{kind} {n} needs instruction text."
            );
            if let Some(source) = &instruction.source {
                ensure!(
                    !source.id.trim().is_empty() && source.number > 0,
                    "{kind} {n} needs a valid source revision."
                );
            }
        }
    }
    for (index, source) in spec.runtime_context.iter_mut().enumerate() {
        let n = index + 1;
        match source {
            RuntimeContext::Obligations { phase: Some(phase) } => {
                *phase = phase.trim().into();
                ensure!(
                    !phase.is_empty(),
                    "Dynamic context source {n} needs an obligation phase or all phases."
                );
            }
            RuntimeContext::ConfiguredSkills { phase, .. } => {
                *phase = phase.trim().into();
                ensure!(
                    tod_store::lifecycle_config::is_phase(phase)
                        && tod_store::lifecycle_config::takes_skills(phase),
                    "Dynamic context source {n} needs a lifecycle phase with configured skills."
                );
            }
            RuntimeContext::Checks { phase } => {
                *phase = phase.trim().into();
                ensure!(
                    crate::checks::phase_key(phase)
                        .is_some_and(tod_store::lifecycle_config::takes_checks),
                    "Dynamic context source {n} needs a lifecycle phase with configured checks."
                );
            }
            RuntimeContext::NodeOutput { node, field } => {
                *node = node.trim().into();
                ensure!(
                    !node.is_empty(),
                    "Dynamic context source {n} needs a workflow node ID."
                );
                if let Some(field) = field {
                    *field = field.trim().into();
                    ensure!(
                        !field.is_empty(),
                        "Dynamic context source {n} needs an output field ID or all fields."
                    );
                }
            }
            _ => {}
        }
    }
    let defaults = agent_launch::AgentLaunchOptions::for_platform(spec.provider);
    let model = setting(spec.model.as_deref(), &defaults.model);
    let effort = setting(spec.effort.as_deref(), &defaults.effort);
    ensure!(
        agent_launch::coerce_model(spec.provider, model) == model,
        "Model {model} is unavailable for {}.",
        spec.provider.label()
    );
    ensure!(
        agent_launch::coerce_effort(spec.provider, effort) == effort,
        "Effort {effort} is unavailable for {}.",
        spec.provider.label()
    );
    spec.model = Some(model.into());
    spec.effort = Some(effort.into());
    Ok(spec)
}

fn setting<'a>(chosen: Option<&'a str>, default: &'a str) -> &'a str {
    chosen
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
}

pub(crate) fn read_action(row: AssetRow) -> Result<PublishedAction> {
    Ok(PublishedAction {
        revision: Revision {
            id: row.id,
            number: row.revision,
        },
        spec: serde_json::from_str(&row.spec)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::workflow::{
        Definition, FORMAT_VERSION, Node, NodeKind, Route, catalog::Catalog,
    };
    use std::collections::BTreeMap;

    fn sample() -> AgentAction {
        AgentAction {
            title: "Review the change".into(),
            prompt: "Review this change and report what you found.".into(),
            skills: vec![
                Instruction {
                    title: "Check evidence".into(),
                    text: "Review the accompanying evidence.".into(),
                    source: Some(Revision {
                        id: "evidence-skill".into(),
                        number: 4,
                    }),
                },
                Instruction {
                    title: "Report findings".into(),
                    text: "Give a clear finding.".into(),
                    source: None,
                },
            ],
            context: vec![Instruction {
                title: "Local standard".into(),
                text: "Use the repository's terminology.".into(),
                source: None,
            }],
            runtime_context: vec![
                RuntimeContext::Task,
                RuntimeContext::ConfiguredSkills {
                    phase: "review".into(),
                    group: 1,
                },
                RuntimeContext::Checks {
                    phase: "review".into(),
                },
            ],
            provider: AgentPlatform::Claude,
            model: None,
            effort: None,
        }
    }

    #[test]
    fn published_workflows_retain_action_text_and_settings_across_action_revisions() {
        let directory = tempfile::tempdir().unwrap();
        let fleet = FleetStore::open_without_reattach(directory.path()).unwrap();
        let actions = ActionCatalog::new(&fleet);
        let mut invalid = sample();
        invalid.prompt.clear();
        assert!(actions.publish(None, None, invalid).is_err());

        let first = actions.publish(None, None, sample()).unwrap();
        assert_eq!(
            first.spec.model.as_deref(),
            Some(agent_launch::default_model_for(AgentPlatform::Claude))
        );
        assert_eq!(
            first.spec.effort.as_deref(),
            Some(agent_launch::DEFAULT_EFFORT)
        );
        let catalog = Catalog::new(&fleet);
        let draft = catalog
            .create(
                "Review",
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
                                    action: first.revision.clone(),
                                },
                                outputs: BTreeMap::new(),
                                routes: vec![Route {
                                    id: "finish".into(),
                                    to: "done".into(),
                                    when: None,
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
                    fields: BTreeMap::new(),
                },
            )
            .unwrap();
        let published = catalog
            .publish(&draft.definition.revision.id, draft.generation)
            .unwrap();
        let mut changed = first.spec.clone();
        changed.skills.reverse();
        changed.runtime_context.reverse();
        changed.prompt = "Review just the tests.".into();
        let newer = actions
            .publish(Some(&first.revision.id), Some(1), changed.clone())
            .unwrap();
        assert_eq!(newer.revision.number, 2);
        assert!(
            actions
                .publish(Some(&first.revision.id), Some(1), changed)
                .is_err()
        );
        assert_eq!(actions.list().unwrap(), [newer.clone()]);
        assert_eq!(
            actions.revisions(&first.revision.id).unwrap(),
            [newer, first.clone()]
        );
        assert_eq!(
            catalog
                .action_for(&published.definition.revision, &first.revision)
                .unwrap(),
            Some(first)
        );
    }

    #[test]
    fn older_actions_load_without_dynamic_inputs_and_invalid_sources_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let fleet = FleetStore::open_without_reattach(directory.path()).unwrap();
        let actions = ActionCatalog::new(&fleet);
        let mut older = sample();
        older.runtime_context.clear();
        let legacy_revision = actions.publish(None, None, older).unwrap().revision;
        let raw = fleet
            .read(|conn| workflow_actions::published(conn, &legacy_revision.id, 1))
            .unwrap()
            .unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&raw.spec)
                .unwrap()
                .get("runtime_context")
                .is_none()
        );
        assert!(
            actions
                .published(&legacy_revision)
                .unwrap()
                .unwrap()
                .spec
                .runtime_context
                .is_empty()
        );

        let mut invalid = sample();
        invalid.runtime_context = vec![RuntimeContext::NodeOutput {
            node: String::new(),
            field: None,
        }];
        assert!(
            actions
                .publish(None, None, invalid)
                .unwrap_err()
                .to_string()
                .contains("workflow node")
        );

        let mut invalid = sample();
        invalid.runtime_context = vec![RuntimeContext::ConfiguredSkills {
            phase: "invented".into(),
            group: 0,
        }];
        assert!(
            actions
                .publish(None, None, invalid)
                .unwrap_err()
                .to_string()
                .contains("configured skills")
        );
    }
}
