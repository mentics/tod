//! Operator API for drafting, validating, and publishing workflow definitions.

use anyhow::{Context, Result, bail};
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_USER, InterviewCommand};
use tod_store::workflow_actions::{self, AssetRef};
use tod_store::workflow_catalog::{self, Command, DraftRow, PublishedRow};

use super::actions::{PublishedAction, RuntimeContext, read_action};
use super::{Definition, NodeKind, Revision};

pub use tod_store::workflow_catalog::Summary;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    pub generation: u64,
    pub definition: Definition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub title: String,
    pub definition: Definition,
}

pub struct Catalog<'a> {
    fleet: &'a FleetStore,
}

impl<'a> Catalog<'a> {
    pub fn new(fleet: &'a FleetStore) -> Self {
        Self { fleet }
    }

    pub fn list(&self) -> Result<Vec<Summary>> {
        self.fleet.read(workflow_catalog::list)
    }

    pub fn create(&self, title: &str, mut definition: Definition) -> Result<Draft> {
        let id = uuid::Uuid::new_v4().to_string();
        definition.revision.id = id.clone();
        self.draft_write(Command::Create {
            id,
            title: checked_title(title)?,
            definition: serde_json::to_string(&definition)?,
        })
    }

    pub fn draft(&self, id: &str) -> Result<Option<Draft>> {
        self.fleet
            .read(|conn| workflow_catalog::draft(conn, id))?
            .map(read_draft)
            .transpose()
    }

    pub fn save(
        &self,
        id: &str,
        expected_generation: u64,
        title: &str,
        mut definition: Definition,
    ) -> Result<Draft> {
        definition.revision.id = id.into();
        self.draft_write(Command::Save {
            id: id.into(),
            expected_generation,
            title: checked_title(title)?,
            definition: serde_json::to_string(&definition)?,
        })
    }

    pub fn publish(&self, id: &str, expected_generation: u64) -> Result<Published> {
        let draft = self
            .draft(id)?
            .with_context(|| format!("workflow draft {id} does not exist"))?;
        if draft.generation != expected_generation {
            bail!(
                "workflow draft {id} changed from generation {expected_generation} to {}; reload before publishing",
                draft.generation
            );
        }
        if let Err(diagnostics) = draft.definition.validate() {
            let details = diagnostics
                .into_iter()
                .map(|item| format!("{}: {}", item.location, item.message))
                .collect::<Vec<_>>()
                .join("\n");
            bail!("The workflow draft is invalid.\n{details}");
        }
        let action_refs = self.validate_action_sources(&draft.definition)?;
        // The writer checks the same generation again inside its transaction.
        let row = self.fleet.interview(
            ACTOR_USER,
            InterviewCommand::WorkflowCatalog(Command::Publish {
                id: id.into(),
                expected_generation,
                action_refs,
            }),
        )?;
        read_published(serde_json::from_value(row)?)
    }

    fn validate_action_sources(&self, definition: &Definition) -> Result<Vec<AssetRef>> {
        self.fleet.read(|conn| {
            let mut actions = BTreeMap::new();
            for (id, node) in &definition.nodes {
                let NodeKind::Action { action } = &node.kind else {
                    continue;
                };
                let asset = match actions.entry((action.id.as_str(), action.number)) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => entry.insert(
                        workflow_actions::published(conn, &action.id, action.number)?
                            .map(read_action)
                            .transpose()?,
                    ),
                };
                // Runner-supplied adapters have no authored action revision.
                // The writer rejects missing revisions of registered assets.
                let Some(asset) = asset else { continue };
                validate_output_context(definition, id, &asset.spec.runtime_context)?;
            }
            Ok(actions
                .into_keys()
                .map(|(id, revision)| AssetRef {
                    id: id.into(),
                    revision,
                })
                .collect())
        })
    }

    pub fn published(&self, id: &str, revision: u64) -> Result<Option<Published>> {
        self.fleet
            .read(|conn| workflow_catalog::published(conn, id, revision))?
            .map(read_published)
            .transpose()
    }

    /// The exact action captured by this workflow revision.
    pub fn action_for(
        &self,
        workflow: &Revision,
        action: &Revision,
    ) -> Result<Option<PublishedAction>> {
        let reference = AssetRef {
            id: action.id.clone(),
            revision: action.number,
        };
        self.fleet
            .read(|conn| {
                workflow_catalog::action_for(conn, &workflow.id, workflow.number, &reference)
            })?
            .map(read_action)
            .transpose()
    }

    pub fn revisions(&self, id: &str) -> Result<Vec<Published>> {
        self.fleet
            .read(|conn| workflow_catalog::revisions(conn, id))?
            .into_iter()
            .map(read_published)
            .collect()
    }

    fn draft_write(&self, command: Command) -> Result<Draft> {
        let row = self
            .fleet
            .interview(ACTOR_USER, InterviewCommand::WorkflowCatalog(command))?;
        read_draft(serde_json::from_value(row)?)
    }
}

fn validate_output_context(
    definition: &Definition,
    consumer: &str,
    sources: &[RuntimeContext],
) -> Result<()> {
    for (index, source) in sources.iter().enumerate() {
        let RuntimeContext::NodeOutput {
            node: origin,
            field,
        } = source
        else {
            continue;
        };
        let location = format!(
            "Workflow node {consumer:?}, dynamic context source {}",
            index + 1
        );
        let producer = definition
            .nodes
            .get(origin)
            .with_context(|| format!("{location} refers to missing workflow node {origin:?}."))?;
        if !matches!(producer.kind, NodeKind::Action { .. }) || producer.outputs.is_empty() {
            bail!("{location} refers to node {origin:?}, which declares no action outputs.");
        }
        if let Some(field) = field
            && !producer.outputs.contains_key(field)
        {
            bail!("{location} refers to missing output field {field:?} on node {origin:?}.");
        }
        if origin == consumer && !can_revisit(definition, consumer) {
            bail!(
                "{location} can use its own previous output only when the workflow has a route back to {consumer:?}."
            );
        }
    }
    Ok(())
}

fn can_revisit(definition: &Definition, start: &str) -> bool {
    let mut visited = BTreeSet::new();
    let mut next: Vec<&str> = definition.nodes[start]
        .routes
        .iter()
        .map(|route| route.to.as_str())
        .collect();
    while let Some(id) = next.pop() {
        if id == start {
            return true;
        }
        if visited.insert(id) {
            next.extend(
                definition.nodes[id]
                    .routes
                    .iter()
                    .map(|route| route.to.as_str()),
            );
        }
    }
    false
}

fn checked_title(title: &str) -> Result<String> {
    let title = title.trim();
    if title.is_empty() {
        bail!("Give the workflow a title.");
    }
    Ok(title.into())
}

fn read_draft(row: DraftRow) -> Result<Draft> {
    Ok(Draft {
        title: row.title,
        generation: row.generation,
        definition: serde_json::from_str(&row.definition).context("read workflow draft")?,
    })
}

fn read_published(row: PublishedRow) -> Result<Published> {
    Ok(Published {
        title: row.title,
        definition: serde_json::from_str(&row.definition)
            .context("read published workflow revision")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::workflow::{
        Comparison, Condition, FORMAT_VERSION, Field, Node, Operand, Route, Source, Value,
        ValueType,
        actions::{ActionCatalog, AgentAction},
    };
    use tod_agent::AgentPlatform;

    fn example(entry: &str) -> Definition {
        Definition {
            task_phases: BTreeMap::new(),
            adoption_points: BTreeMap::new(),
            format: FORMAT_VERSION,
            revision: Revision {
                id: String::new(),
                number: 0,
            },
            entry: entry.into(),
            nodes: BTreeMap::from([(
                "end".into(),
                Node {
                    label: "Finish task".into(),
                    kind: NodeKind::Finish,
                    outputs: BTreeMap::new(),
                    routes: Vec::new(),
                },
            )]),
            fields: BTreeMap::new(),
        }
    }

    #[test]
    fn draft_edits_and_publishing_use_the_same_generation_across_store_reopens() {
        let root = tempfile::tempdir().unwrap();
        let store = FleetStore::open_without_reattach(root.path()).unwrap();
        let catalog = Catalog::new(&store);
        let draft = catalog.create("  Review  ", example("missing")).unwrap();
        let id = &draft.definition.revision.id;
        assert_eq!(draft.title, "Review");
        assert_eq!(draft.generation, 1);
        assert_eq!(draft.definition.revision.number, 1);
        assert_eq!(catalog.list().unwrap()[0].latest_revision, None);
        assert!(
            catalog
                .publish(id, draft.generation)
                .unwrap_err()
                .to_string()
                .contains("entry")
        );

        let valid = catalog
            .save(id, draft.generation, "Review", example("end"))
            .unwrap();
        assert!(
            catalog
                .save(id, draft.generation, "stale", example("end"))
                .is_err()
        );
        let first = catalog.publish(id, valid.generation).unwrap();
        assert_eq!(first.definition.revision.number, 1);
        assert!(catalog.publish(id, valid.generation).is_err());
        let next = catalog.draft(id).unwrap().unwrap();
        assert_eq!(next.generation, 3);
        assert_eq!(next.definition.revision.number, 2);

        let next = catalog
            .save(id, next.generation, "Review twice", example("end"))
            .unwrap();
        assert_eq!(
            catalog
                .publish(id, next.generation)
                .unwrap()
                .definition
                .revision
                .number,
            2
        );
        drop(store);

        let reopened = FleetStore::open_without_reattach(root.path()).unwrap();
        let catalog = Catalog::new(&reopened);
        assert_eq!(catalog.published(id, 1).unwrap().unwrap(), first);
        assert_eq!(catalog.list().unwrap()[0].latest_revision, Some(2));
        let versions = catalog.revisions(id).unwrap();
        assert_eq!(
            versions
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            ["Review twice", "Review"]
        );
    }

    #[test]
    fn publishing_resolves_dynamic_outputs_in_each_referencing_graph() {
        let directory = tempfile::tempdir().unwrap();
        let fleet = FleetStore::open_without_reattach(directory.path()).unwrap();
        let catalog = Catalog::new(&fleet);
        let actions = ActionCatalog::new(&fleet);
        let action = |runtime_context| {
            actions
                .publish(
                    None,
                    None,
                    AgentAction {
                        title: "Inspect".into(),
                        prompt: "Inspect the output.".into(),
                        provider: AgentPlatform::Claude,
                        model: None,
                        effort: None,
                        skills: Vec::new(),
                        context: Vec::new(),
                        runtime_context,
                    },
                )
                .unwrap()
        };
        let producer = action(Vec::new()).revision;
        let definition = |consumer: Revision| {
            let mut definition = example("source");
            for (id, to, action) in [
                ("source", "consume", producer.clone()),
                ("consume", "end", consumer),
            ] {
                definition.nodes.insert(
                    id.into(),
                    Node {
                        label: id.into(),
                        kind: NodeKind::Action { action },
                        outputs: BTreeMap::from([("result".into(), ValueType::Integer)]),
                        routes: vec![Route {
                            id: format!("{id}-next"),
                            to: to.into(),
                            when: None,
                        }],
                    },
                );
            }
            definition
        };
        for (origin, field, expected_error) in [
            ("missing", None, "missing workflow node"),
            ("source", Some("missing"), "missing output field"),
            ("end", None, "declares no action outputs"),
            (
                "consume",
                Some("result"),
                "only when the workflow has a route back",
            ),
        ] {
            let consumer = action(vec![RuntimeContext::NodeOutput {
                node: origin.into(),
                field: field.map(str::to_owned),
            }]);
            let draft = catalog
                .create("Inspect", definition(consumer.revision))
                .unwrap();
            assert!(
                catalog
                    .publish(&draft.definition.revision.id, draft.generation)
                    .unwrap_err()
                    .to_string()
                    .contains(expected_error)
            );
        }

        let consumer = action(vec![
            RuntimeContext::NodeOutput {
                node: " source ".into(),
                field: Some(" result ".into()),
            },
            RuntimeContext::NodeOutput {
                node: "consume".into(),
                field: None,
            },
        ]);
        assert_eq!(
            consumer.spec.runtime_context[0],
            RuntimeContext::NodeOutput {
                node: "source".into(),
                field: Some("result".into()),
            }
        );
        let mut graph = definition(consumer.revision.clone());
        graph.fields.insert(
            "result".into(),
            Field {
                value_type: ValueType::Integer,
                source: Source::NodeOutput {
                    node: "consume".into(),
                    field: "result".into(),
                },
            },
        );
        graph.nodes.get_mut("consume").unwrap().routes.push(Route {
            id: "try-again".into(),
            to: "consume".into(),
            when: Some(Condition::Compare {
                left: Operand::Field("result".into()),
                op: Comparison::Greater,
                right: Operand::Literal(Value::Integer(0)),
            }),
        });
        let draft = catalog.create("Inspect", graph).unwrap();
        let published = catalog
            .publish(&draft.definition.revision.id, draft.generation)
            .unwrap();
        assert_eq!(
            catalog
                .action_for(&published.definition.revision, &consumer.revision)
                .unwrap(),
            Some(consumer)
        );
        assert!(
            catalog
                .action_for(&published.definition.revision, &producer)
                .unwrap()
                .unwrap()
                .spec
                .runtime_context
                .is_empty()
        );
    }
}
