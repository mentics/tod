//! Explicit revision changes for in-flight workflow runs. A migration starts a
//! fresh visit at the entry for the task's current stay. Adoption and detach
//! switch runtime ownership without changing the task's lifecycle stay.

use super::{
    Revision,
    catalog::Catalog,
    runner::{Run, Runner},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tod_store::{
    fleet::FleetStore,
    interview::{ACTOR_USER, InterviewCommand},
    workflow_migration as store,
    workflow_run::{self, Mutation},
};
use uuid::Uuid;

pub use tod_store::workflow_migration::MigrationRecord;
pub use tod_store::workflow_run::LegacyStay;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    pub source: Run,
    pub target: Revision,
    pub target_entry: String,
    pub current_node_preserved: bool,
    pub current_node_changed: bool,
    pub outputs: Changes,
    pub fields: Changes,
    pub blocked_by_live_work: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by_phase: Option<String>,
    pub warning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_stay: Option<LegacyStay>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptionPreview {
    pub task: Uuid,
    pub target: Revision,
    pub target_entry: String,
    pub stay: LegacyStay,
    pub blocked_by_live_work: bool,
    pub warning: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetachPreview {
    pub source: Run,
    pub stay: Option<LegacyStay>,
    pub blocked_by_live_work: bool,
    pub warning: String,
}

pub fn preview_adoption(
    fleet: &FleetStore,
    task: Uuid,
    target: &Revision,
) -> Result<AdoptionPreview> {
    let after = target_definition(fleet, target)?;
    let (stay, blocked_by_live_work) = fleet.read(|conn| {
        workflow_run::require_local_execution(conn, task)?;
        ensure!(
            workflow_run::current(conn, task)?.is_none(),
            "Detach the task's existing workflow before adoption."
        );
        handoff_state(conn, task)
    })?;
    let stay = stay.context("This task has no lifecycle stay to adopt.")?;
    let target_entry = store::entry_for_phase(&serde_json::to_value(&after)?, &stay.phase)?
        .with_context(|| {
            format!(
                "This published workflow has no adoption point for {}.",
                stay.phase
            )
        })?;
    let warning = format!(
        "This task will start a new workflow run at {} in its current {} stay. Existing work and evidence remain on the task. The workflow will evaluate the entry's work and gates in a new visit.",
        after.nodes[&target_entry].label, stay.phase
    );
    Ok(AdoptionPreview {
        task,
        target: target.clone(),
        target_entry,
        stay,
        blocked_by_live_work,
        warning,
    })
}

pub fn adopt(fleet: &FleetStore, prepared: &AdoptionPreview, max_visits: u32) -> Result<Run> {
    let run = Uuid::new_v4();
    fleet.interview(
        ACTOR_USER,
        InterviewCommand::WorkflowRun {
            mutation: Mutation::Adopt {
                run,
                task: prepared.task,
                workflow_id: prepared.target.id.clone(),
                revision: prepared.target.number,
                entry: prepared.target_entry.clone(),
                max_visits,
                stay: prepared.stay.clone(),
            },
        },
    )?;
    Runner::new(fleet)
        .get(run)?
        .context("The adopted workflow run is missing.")
}

pub fn preview_detach(fleet: &FleetStore, run: Uuid) -> Result<DetachPreview> {
    let (source, stay, blocked_by_live_work) = fleet.read(|conn| {
        let source = workflow_run::get(conn, run)?.context("The workflow run no longer exists.")?;
        ensure!(
            workflow_run::current(conn, source.task)?.is_some_and(|current| current.id == run),
            "This run is no longer the task's attached workflow."
        );
        let (stay, blocked_by_live_work) = handoff_state(conn, source.task)?;
        Ok((source.into(), stay, blocked_by_live_work))
    })?;
    let warning = if let Some(stay) = &stay {
        format!(
            "The existing lifecycle will own the task again in {}. Workflow visits, outputs and evidence stay in history, and external changes remain in effect.",
            stay.phase
        )
    } else {
        "The workflow will be detached. The task has no lifecycle stay. Workflow visits, outputs and evidence stay in history, and external changes remain in effect.".into()
    };
    Ok(DetachPreview {
        source,
        stay,
        blocked_by_live_work,
        warning,
    })
}

pub fn detach(fleet: &FleetStore, prepared: &DetachPreview) -> Result<()> {
    fleet.interview(
        ACTOR_USER,
        InterviewCommand::WorkflowRun {
            mutation: Mutation::Detach {
                checkpoint: prepared.source.checkpoint(),
                stay: prepared.stay.clone(),
            },
        },
    )?;
    Ok(())
}

fn handoff_state(conn: &rusqlite::Connection, task: Uuid) -> Result<(Option<LegacyStay>, bool)> {
    Ok((
        workflow_run::legacy_stay(conn, task)?,
        store::has_live_work(conn, task)?,
    ))
}

pub fn preview(fleet: &FleetStore, run: Uuid, target: &Revision) -> Result<Preview> {
    let source = Runner::new(fleet)
        .get(run)?
        .context("The workflow run no longer exists.")?;
    ensure!(
        !source.status.is_terminal(),
        "Only an active run can change workflow revisions."
    );
    ensure!(
        source.revision.id == target.id && &source.revision != target,
        "Choose a different published revision of the same workflow."
    );
    let catalog = Catalog::new(fleet);
    let before = catalog
        .published(&source.revision.id, source.revision.number)?
        .context("The current workflow revision is unavailable.")?
        .definition;
    let after = target_definition(fleet, target)?;
    let current = before
        .nodes
        .get(&source.node)
        .context("The active workflow node is missing.")?;
    let target_node = after.nodes.get(&source.node);
    let empty = BTreeMap::new();
    let outputs = changes(
        &current.outputs,
        target_node.map_or(&empty, |node| &node.outputs),
    );
    let fields = changes(&before.fields, &after.fields);
    let (stay, blocked_by_live_work) = fleet.read(|conn| handoff_state(conn, source.task))?;
    let (target_entry, target_stay, blocked_by_phase) = match stay {
        Some(stay) if !after.task_phases.is_empty() => {
            let entry = store::entry_for_phase(&serde_json::to_value(&after)?, &stay.phase)?;
            if let Some(entry) = entry {
                (entry, Some(stay), None)
            } else {
                let reason = format!(
                    "The replacement starts in {}, and has no adoption point for {}.",
                    after.task_phases[&after.entry], stay.phase
                );
                (after.entry.clone(), None, Some(reason))
            }
        }
        None if !after.task_phases.is_empty() => (
            after.entry.clone(),
            None,
            Some("The task has no lifecycle stay for the replacement to adopt.".into()),
        ),
        _ => (after.entry.clone(), None, None),
    };
    let warning = format!(
        "This task will start a new run at {} in the target revision. Earlier visits, outputs and evidence remain in the original run. External changes made by that run remain in effect.",
        after.nodes[&target_entry].label
    );
    Ok(Preview {
        source,
        target: target.clone(),
        target_entry,
        current_node_preserved: target_node.is_some(),
        current_node_changed: target_node != Some(current),
        outputs,
        fields,
        blocked_by_live_work,
        blocked_by_phase,
        warning,
        target_stay,
    })
}

fn target_definition(fleet: &FleetStore, target: &Revision) -> Result<super::Definition> {
    let after = Catalog::new(fleet)
        .published(&target.id, target.number)?
        .context("The target workflow revision is unavailable.")?
        .definition;
    if let Err(errors) = after.validate() {
        let details = errors
            .into_iter()
            .map(|item| format!("{}: {}", item.location, item.message))
            .collect::<Vec<_>>()
            .join("\n");
        bail!("The target workflow revision is invalid.\n{details}");
    }
    Ok(after)
}

/// Apply an explicitly reviewed preview. The writer checks the preview's
/// generation and live work again before replacing the run.
pub fn apply(fleet: &FleetStore, prepared: &Preview) -> Result<Run> {
    if let Some(reason) = &prepared.blocked_by_phase {
        bail!("{reason}");
    }
    let new_run = Uuid::new_v4();
    fleet.interview(
        ACTOR_USER,
        InterviewCommand::MigrateWorkflow {
            request: store::Request {
                previous: prepared.source.checkpoint(),
                new_run,
                workflow_id: prepared.target.id.clone(),
                revision: prepared.target.number,
                entry: prepared.target_entry.clone(),
                stay: prepared.target_stay.clone(),
            },
        },
    )?;
    Runner::new(fleet)
        .get(new_run)?
        .context("The replacement workflow run is missing.")
}

pub fn history(fleet: &FleetStore, task: Uuid) -> Result<Vec<MigrationRecord>> {
    fleet.read(|conn| store::history(conn, task))
}

fn changes<T: PartialEq>(before: &BTreeMap<String, T>, after: &BTreeMap<String, T>) -> Changes {
    let mut changes = Changes::default();
    for (id, previous) in before {
        match after.get(id) {
            None => changes.removed.push(id.clone()),
            Some(current) if current != previous => changes.changed.push(id.clone()),
            Some(_) => {}
        }
    }
    changes
        .added
        .extend(after.keys().filter(|id| !before.contains_key(*id)).cloned());
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::test_support::fixture;
    use crate::lifecycle::workflow::{
        Definition, FORMAT_VERSION, Field, Node, NodeKind, Route, Source, Value, ValueType,
        runner::{ActionOutput, RunStatus},
    };
    use tod_store::activity::{ActivityLease, KIND_GATE_CHECK};

    fn definition() -> Definition {
        Definition {
            task_phases: BTreeMap::new(),
            adoption_points: BTreeMap::new(),
            format: FORMAT_VERSION,
            revision: Revision {
                id: "".into(),
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
                                id: "manual".into(),
                                number: 1,
                            },
                        },
                        outputs: BTreeMap::from([("count".into(), ValueType::Integer)]),
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
            fields: BTreeMap::from([(
                "count".into(),
                Field {
                    value_type: ValueType::Integer,
                    source: Source::NodeOutput {
                        node: "review".into(),
                        field: "count".into(),
                    },
                },
            )]),
        }
    }

    fn published(fleet: &FleetStore) -> (Revision, Revision) {
        let catalog = Catalog::new(fleet);
        let original = catalog.create("Review", definition()).unwrap();
        let id = &original.definition.revision.id;
        let first = catalog.publish(id, original.generation).unwrap();
        let mut newer = catalog.draft(id).unwrap().unwrap();
        newer.definition.nodes.get_mut("review").unwrap().outputs =
            BTreeMap::from([("comment".into(), ValueType::Text)]);
        newer.definition.fields.clear();
        newer.definition.fields.insert(
            "comment".into(),
            Field {
                value_type: ValueType::Text,
                source: Source::NodeOutput {
                    node: "review".into(),
                    field: "comment".into(),
                },
            },
        );
        newer.definition.entry = "prepare".into();
        newer.definition.nodes.insert(
            "prepare".into(),
            Node {
                label: "Prepare the review".into(),
                kind: NodeKind::Decision,
                outputs: BTreeMap::new(),
                routes: vec![Route {
                    id: "start_review".into(),
                    to: "review".into(),
                    when: None,
                }],
            },
        );
        let newer = catalog
            .save(
                id,
                newer.generation,
                "Review with preparation",
                newer.definition,
            )
            .unwrap();
        let last = catalog.publish(id, newer.generation).unwrap();
        (first.definition.revision, last.definition.revision)
    }

    #[test]
    fn adopts_review_in_its_existing_stay_and_detaches_with_evidence() {
        use tod_store::decisions::{DecisionRepo, NewDecision};
        use tod_store::outline::OutlineMutation;
        let task = fixture();
        let move_to = |state: &str| {
            task.outline(OutlineMutation::SetLifecycle {
                node_id: task.node,
                state: state.into(),
            });
            task.fleet.writer().flush().unwrap();
        };
        let fleet = &task.fleet;
        let runner = Runner::new(fleet);
        let catalog = Catalog::new(fleet);
        let draft = catalog
            .create("Default", super::super::builtin::automated_default())
            .unwrap();
        let id = &draft.definition.revision.id;
        let revision = catalog
            .publish(id, draft.generation)
            .unwrap()
            .definition
            .revision;

        move_to("review");
        let stale = preview_adoption(fleet, task.node, &revision).unwrap();
        assert_eq!(stale.target_entry, "capture_review");
        move_to("verifying");
        move_to("review");
        // The timestamp can be identical if both phase changes happened in
        // one clock tick. The persisted stay generation still distinguishes it.
        rusqlite::Connection::open(fleet.paths().db())
            .unwrap()
            .execute(
                "UPDATE node_lifecycle SET updated_at = ?2 WHERE node_id = ?1",
                rusqlite::params![task.node.as_bytes(), stale.stay.entered_at_ms],
            )
            .unwrap();
        fleet.reload_if_stale().unwrap();
        let prepared = preview_adoption(fleet, task.node, &revision).unwrap();
        assert_eq!(stale.stay.entered_at_ms, prepared.stay.entered_at_ms);
        assert_ne!(stale.stay.generation, prepared.stay.generation);
        assert!(adopt(fleet, &stale, 16).is_err());
        let lease = ActivityLease::start(fleet, task.node, KIND_GATE_CHECK, "test").unwrap();
        assert!(
            preview_adoption(fleet, task.node, &revision)
                .unwrap()
                .blocked_by_live_work
        );
        assert!(adopt(fleet, &prepared, 16).is_err());
        lease.end(fleet).unwrap();
        fleet
            .interview(
                ACTOR_USER,
                InterviewCommand::AskDecision {
                    node_id: task.node,
                    conversation_id: None,
                    protocol: None,
                    decision: NewDecision {
                        question: "May this review use the revised requirements?".into(),
                        options: vec![
                            "Use revised requirements".into(),
                            "Keep existing requirements".into(),
                        ],
                        ..NewDecision::default()
                    },
                },
            )
            .unwrap();
        let pending = || {
            fleet
                .read(|conn| DecisionRepo::new(conn).list_open_for_node(task.node))
                .unwrap()
        };
        let existing_decisions = pending();
        assert_eq!(existing_decisions.len(), 1);
        let active = adopt(fleet, &prepared, 16).unwrap();
        assert_eq!(pending(), existing_decisions);
        assert_eq!(active.adopted_from, Some(prepared.stay.clone()));
        assert_eq!(active.node, prepared.target_entry);
        assert!(preview_adoption(fleet, task.node, &revision).is_err());
        let stale_detach = preview_detach(fleet, active.id).unwrap();
        let request = runner.dispatch(&active).unwrap();
        let output = ActionOutput {
            values: BTreeMap::from([
                ("revision".into(), Value::Text("test-revision".into())),
                ("redirect".into(), Value::Text(String::new())),
            ]),
            evidence: vec!["review-capture".into()],
            ..Default::default()
        };
        runner.complete(&request, output.clone()).unwrap();
        runner
            .evaluate(&runner.get(active.id).unwrap().unwrap(), BTreeMap::new())
            .unwrap();
        assert!(detach(fleet, &stale_detach).is_err());

        let draft = catalog.draft(id).unwrap().unwrap();
        let revised = catalog
            .publish(id, draft.generation)
            .unwrap()
            .definition
            .revision;
        let change = preview(fleet, active.id, &revised).unwrap();
        assert_eq!(change.target_entry, "capture_review");
        assert_eq!(change.target_stay, Some(prepared.stay.clone()));
        let replacement = apply(fleet, &change).unwrap();
        assert_eq!(replacement.adopted_from, Some(prepared.stay.clone()));
        let ready = preview_detach(fleet, replacement.id).unwrap();
        assert_eq!(ready.stay.as_ref(), Some(&prepared.stay));
        let lease = ActivityLease::start(fleet, task.node, KIND_GATE_CHECK, "test").unwrap();
        assert!(
            preview_detach(fleet, replacement.id)
                .unwrap()
                .blocked_by_live_work
        );
        assert!(detach(fleet, &ready).is_err());
        lease.end(fleet).unwrap();
        detach(fleet, &ready).unwrap();
        assert!(runner.current(task.node).unwrap().is_none());
        assert_eq!(pending(), existing_decisions);
        assert!(detach(fleet, &ready).is_err());
        assert_eq!(
            fleet
                .read(|conn| workflow_run::legacy_stay(conn, task.node))
                .unwrap(),
            Some(prepared.stay)
        );
        assert_eq!(runner.visits(active.id).unwrap()[0].output, Some(output));
        assert_eq!(runner.traces(active.id).unwrap().len(), 1);
        let history = runner.history(task.node).unwrap();
        assert_eq!(
            (history[0].status, history[0].id),
            (RunStatus::Cancelled, replacement.id)
        );
        assert!(history[0].detached_at.is_some());
        assert_eq!(history[1].id, active.id);
    }

    #[test]
    fn detaching_a_release_wait_restores_the_deadline_in_the_same_legacy_stay() {
        use tod_store::{outline::OutlineMutation, waits::WaitRepo};
        let task = fixture();
        task.outline(OutlineMutation::SetLifecycle {
            node_id: task.node,
            state: "merged".into(),
        });
        let fleet = &task.fleet;
        fleet.writer().flush().unwrap();
        let catalog = Catalog::new(fleet);
        let draft = catalog
            .create("Default", super::super::builtin::automated_default())
            .unwrap();
        let revision = catalog
            .publish(&draft.definition.revision.id, draft.generation)
            .unwrap()
            .definition
            .revision;
        let prepared = preview_adoption(fleet, task.node, &revision).unwrap();
        assert_eq!(prepared.target_entry, "release_evidence");
        let run = adopt(fleet, &prepared, 16).unwrap();
        let runner = Runner::new(fleet);
        let deadline = chrono::Utc::now().timestamp_millis() + 60_000;
        runner
            .evaluate_with_wait(&run, BTreeMap::new(), |_| {
                Ok(Some(workflow_run::WaitSchedule {
                    what: "a published release".into(),
                    due_at_ms: deadline,
                }))
            })
            .unwrap();

        detach(fleet, &preview_detach(fleet, run.id).unwrap()).unwrap();
        assert!(
            fleet
                .read(|conn| workflow_run::scheduled_wait(conn, task.node))
                .unwrap()
                .is_none()
        );
        let legacy = fleet
            .read(|conn| WaitRepo::new(conn).list_pending_for_node(task.node))
            .unwrap();
        let wait = legacy
            .iter()
            .find(|wait| wait.match_spec == "github:release:published")
            .unwrap();
        assert_eq!(
            (wait.stay, wait.due_at),
            (Some(prepared.stay.entered_at_ms), deadline)
        );
        assert_eq!(
            runner.traces(run.id).unwrap()[0].resulting_status,
            RunStatus::Waiting
        );
    }

    #[test]
    fn a_completed_workflow_detaches_without_reopening_the_done_stay() {
        use tod_store::outline::OutlineMutation;
        let task = fixture();
        task.outline(OutlineMutation::SetLifecycle {
            node_id: task.node,
            state: "done".into(),
        });
        let fleet = &task.fleet;
        fleet.writer().flush().unwrap();
        let catalog = Catalog::new(fleet);
        let draft = catalog
            .create("Default", super::super::builtin::automated_default())
            .unwrap();
        let revision = catalog
            .publish(&draft.definition.revision.id, draft.generation)
            .unwrap()
            .definition
            .revision;
        let preview = preview_adoption(fleet, task.node, &revision).unwrap();
        let run = adopt(fleet, &preview, 16).unwrap();
        let runner = Runner::new(fleet);
        assert_eq!(
            runner.evaluate(&run, BTreeMap::new()).unwrap().decision,
            super::super::Decision::Complete
        );
        let detach_preview = preview_detach(fleet, run.id).unwrap();
        detach(fleet, &detach_preview).unwrap();
        assert!(runner.current(task.node).unwrap().is_none());
        let retained = runner.get(run.id).unwrap().unwrap();
        assert_eq!(retained.status, RunStatus::Complete);
        assert!(retained.detached_at.is_some());
        assert_eq!(
            runner.traces(run.id).unwrap()[0].resulting_status,
            RunStatus::Complete
        );
        assert_eq!(
            fleet
                .read(|conn| workflow_run::legacy_stay(conn, task.node))
                .unwrap(),
            Some(preview.stay)
        );
    }

    #[test]
    fn migration_preview_holds_when_the_target_entry_is_in_another_task_phase() {
        let task = fixture();
        task.outline(tod_store::outline::OutlineMutation::SetLifecycle {
            node_id: task.node,
            state: "proposed".into(),
        });
        task.fleet.writer().flush().unwrap();
        let catalog = Catalog::new(&task.fleet);
        let original = catalog.create("Review", definition()).unwrap();
        let id = &original.definition.revision.id;
        let first = catalog.publish(id, original.generation).unwrap();
        let runner = Runner::new(&task.fleet);
        let run = runner
            .assign(task.node, &first.definition.revision, 10)
            .unwrap();

        let mut draft = catalog.draft(id).unwrap().unwrap();
        draft.definition.task_phases = BTreeMap::from([
            ("review".into(), "review".into()),
            ("done".into(), "done".into()),
        ]);
        let saved = catalog
            .save(id, draft.generation, &draft.title, draft.definition)
            .unwrap();
        let target = catalog.publish(id, saved.generation).unwrap();
        let mut prepared = preview(&task.fleet, run.id, &target.definition.revision).unwrap();
        assert!(
            prepared
                .blocked_by_phase
                .as_deref()
                .is_some_and(|reason| reason.contains("review") && reason.contains("proposed"))
        );
        assert!(apply(&task.fleet, &prepared).is_err());
        prepared.blocked_by_phase = None;
        assert!(
            apply(&task.fleet, &prepared).is_err(),
            "the writer rechecks the task phase"
        );
        assert_eq!(runner.current(task.node).unwrap().unwrap(), run);
    }

    #[test]
    fn preview_reports_changes_and_keeps_evidence_in_the_previous_run() {
        let fx = fixture();
        let runner = Runner::new(&fx.fleet);
        let (first, new) = published(&fx.fleet);
        let old = runner.assign(fx.node, &first, 16).unwrap();
        let request = runner.dispatch(&old).unwrap();
        runner
            .complete(
                &request,
                ActionOutput {
                    values: BTreeMap::from([("count".into(), Value::Integer(2))]),
                    evidence: vec!["review-evidence".into()],
                    ..Default::default()
                },
            )
            .unwrap();
        let info = preview(&fx.fleet, old.id, &new).unwrap();
        assert!(info.current_node_preserved && info.current_node_changed);
        assert_eq!(info.outputs.added, ["comment"]);
        assert_eq!(info.outputs.removed, ["count"]);
        assert_eq!(info.fields, info.outputs);
        assert!(
            info.warning.contains("Prepare the review") && info.warning.contains("original run")
        );
        let replacement = apply(&fx.fleet, &info).unwrap();
        assert_eq!(replacement.revision, new);
        assert_eq!(replacement.node, "prepare");
        assert_eq!(replacement.visit, 1);
        assert_eq!(replacement.max_visits, old.max_visits);
        assert_eq!(
            runner.get(old.id).unwrap().unwrap().status,
            RunStatus::Cancelled
        );
        let visits = runner.visits(old.id).unwrap();
        assert!(visits[0].ended_at.is_some());
        assert_eq!(
            visits[0].output.as_ref().unwrap().evidence,
            ["review-evidence"]
        );
        let recorded = history(&fx.fleet, fx.node).unwrap();
        assert_eq!(recorded[0].previous_run, old.id);
        assert_eq!(recorded[0].new_run, replacement.id);
    }

    #[test]
    fn stale_preview_and_live_work_leave_the_original_run_in_place() {
        let fx = fixture();
        let runner = Runner::new(&fx.fleet);
        let (old_revision, new) = published(&fx.fleet);
        let old = runner.assign(fx.node, &old_revision, 16).unwrap();
        let stale = preview(&fx.fleet, old.id, &new).unwrap();
        runner.dispatch(&old).unwrap();
        assert!(
            apply(&fx.fleet, &stale)
                .unwrap_err()
                .to_string()
                .contains("changed")
        );

        let lease = ActivityLease::start(&fx.fleet, fx.node, KIND_GATE_CHECK, "test").unwrap();
        let held = preview(&fx.fleet, old.id, &new).unwrap();
        assert!(held.blocked_by_live_work);
        assert!(apply(&fx.fleet, &held).is_err());
        lease.end(&fx.fleet).unwrap();

        // Assignment fails after cancellation is staged when the target
        // entry disagrees. The writer must roll both operations back.
        let mut wrong_entry = preview(&fx.fleet, old.id, &new).unwrap();
        wrong_entry.target_entry = "missing".into();
        assert!(apply(&fx.fleet, &wrong_entry).is_err());
        let current = runner.current(fx.node).unwrap().unwrap();
        assert_eq!(current.id, old.id);
        assert!(!current.status.is_terminal());
        assert!(history(&fx.fleet, fx.node).unwrap().is_empty());
    }
}
