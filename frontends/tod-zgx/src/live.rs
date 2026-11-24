//! Adapter for the existing TOD read model. ZGX uses the same store-opening policy as TOD’s CLI.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};
use tod_core::{
    lifecycle::flow,
    runner_operator::{self, Options, Session, Verb, Who},
};
use tod_palette::{Effect, Item, Kind};
use tod_store::outline::{
    OutlineMutation,
    repos::gate::GateRepo,
    repos::{ListRepo, NodeRepo, OutlineRepo, TreeLoader},
    types::Capability,
};
use tod_store::{fleet::FleetStore, lifecycle_config};

use crate::{
    collection::OutlineData,
    lifecycle::{Agent, Criterion, Data},
    workspace::{Record, Workspace},
};

#[derive(Clone)]
pub struct Backend {
    access: Arc<StoreAccess>,
    runtime: Arc<tokio::runtime::Runtime>,
}

/// Serialize local store opens from this window. Without a daemon, overlapping
/// background operations would otherwise compete for the same process lock.
struct StoreAccess {
    root: PathBuf,
    opening: Mutex<()>,
}

impl StoreAccess {
    fn open(&self) -> Result<(MutexGuard<'_, ()>, Session), Box<dyn std::error::Error>> {
        let opening = self
            .opening
            .lock()
            .map_err(|_| "The workspace store lock was poisoned.")?;
        let session = Session::open(&self.root)?;
        Ok((opening, session))
    }
}

impl Backend {
    pub fn open(root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let root = root.canonicalize()?;
        if !root.join("tod.db").is_file() {
            return Err(format!("No TOD data root at {}", root.display()).into());
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        tod_store::set_data_root(root.clone());
        Ok(Self {
            access: Arc::new(StoreAccess {
                root,
                opening: Mutex::new(()),
            }),
            runtime: Arc::new(runtime),
        })
    }

    pub fn load(
        &self,
        frequency: BTreeMap<String, u32>,
    ) -> Result<Workspace, Box<dyn std::error::Error>> {
        let _entered = self.runtime.enter();
        let (_opening, session) = self.access.open()?;
        Ok(Workspace::live(load_records(&session.fleet)?, frequency))
    }

    pub fn run(
        &self,
        effect: &Effect,
    ) -> Option<(
        &'static str,
        tokio::task::JoinHandle<Result<String, String>>,
    )> {
        let Effect::Run { command, targets } = effect else {
            return None;
        };
        let verb = match *command {
            "pause" => Verb::Pause,
            "resume" => Verb::Resume,
            _ => return None,
        };
        let access = self.access.clone();
        let targets = targets.clone();
        let task = self.runtime.spawn_blocking(move || {
            let [node] = targets.as_slice() else {
                return Err("Choose one task to control its runner.".into());
            };
            let (_opening, session) = access.open().map_err(|error| format!("{error:#}"))?;
            run_runner(&session, node, verb).map_err(|error| format!("{error:#}"))
        });
        Some((command, task))
    }

    pub fn load_outline(&self, id: String) -> tokio::task::JoinHandle<Result<OutlineData, String>> {
        let access = self.access.clone();
        self.runtime.spawn_blocking(move || {
            let load = || -> Result<OutlineData, Box<dyn std::error::Error>> {
                let id = id.parse()?;
                let (_opening, session) = access.open()?;
                let fleet = &session.fleet;
                let outline = fleet.read(|conn| {
                    let Some(list) = ListRepo::new(conn).get(id)? else {
                        return Ok(None);
                    };
                    let mut outline = OutlineData::new(list.title);
                    for row in TreeLoader::new(conn).flatten_all(id)? {
                        let task = row.capabilities.contains(&Capability::Lifecycle)
                            && tod_store::fleet::node_actions::is_task_node(conn, row.node.id)?;
                        let kind = row_kind(task, row.capabilities.contains(&Capability::Spec));
                        outline.push(row, kind);
                    }
                    Ok(Some(outline))
                })?;
                outline.ok_or_else(|| "This saved outline is no longer in the workspace.".into())
            };
            load().map_err(|error| format!("Could not open saved outline. {error}"))
        })
    }

    pub fn load_lifecycle(&self, id: String) -> tokio::task::JoinHandle<Result<Data, String>> {
        let access = self.access.clone();
        self.runtime.spawn_blocking(move || {
            let load = || -> Result<Data, Box<dyn std::error::Error>> {
                let (_opening, session) = access.open()?;
                let fleet = &session.fleet;
                read_lifecycle(fleet, &id)
            };
            load().map_err(|error| format!("Could not load lifecycle details. {error}"))
        })
    }

    pub fn save_lifecycle_agent(
        &self,
        id: String,
        key: &'static str,
        skills: Option<String>,
        model: String,
        effort: String,
    ) -> tokio::task::JoinHandle<Result<Data, String>> {
        let access = self.access.clone();
        self.runtime.spawn_blocking(move || {
            let save = || -> Result<Data, Box<dyn std::error::Error>> {
                let node = id.parse()?;
                let (_opening, session) = access.open()?;
                let fleet = &session.fleet;
                let (enabled, mut skill_map, mut agents) = fleet.read(|conn| {
                    Ok((
                        NodeRepo::new(conn)
                            .list_capabilities(node)?
                            .contains(&Capability::LifecycleConfig),
                        lifecycle_config::skills(conn, node)?,
                        lifecycle_config::agents(conn, node)?,
                    ))
                })?;
                if !enabled || !lifecycle_config::is_phase(key) {
                    return Err(
                        "Lifecycle configuration is unavailable for this task or phase.".into(),
                    );
                }
                let original_skills = skill_map.clone();
                if lifecycle_config::takes_skills(key) {
                    if let Some(skills) = &skills {
                        skill_map.insert(key.into(), lifecycle_config::parse_list(skills)?);
                    } else {
                        skill_map.remove(key);
                    }
                }
                let original_agents = agents.clone();
                let agent = agents.entry(key.into()).or_default();
                agent.model = non_empty(model);
                agent.effort = non_empty(effort);
                if agent.is_empty() {
                    agents.remove(key);
                }
                if original_skills != skill_map {
                    fleet.enqueue_outline(OutlineMutation::SetNodeLifecycleConfig {
                        node_id: node,
                        skills: skill_map,
                    })?;
                }
                if original_agents != agents {
                    fleet.enqueue_outline(OutlineMutation::SetNodeLifecycleAgents {
                        node_id: node,
                        agents,
                    })?;
                }
                fleet.writer().flush()?;
                fleet.reload_if_stale()?;
                read_lifecycle(fleet, &id)
            };
            save().map_err(|error| format!("Could not save lifecycle settings. {error}"))
        })
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn read_lifecycle(fleet: &FleetStore, id: &str) -> Result<Data, Box<dyn std::error::Error>> {
    let node = id.parse()?;
    let data = fleet.read(|conn| {
        let repo = NodeRepo::new(conn);
        let Some(task) = repo.get(node)? else {
            return Ok(None);
        };
        let caps = repo.list_capabilities(node)?;
        let own_skills = lifecycle_config::skills(conn, node)?;
        let own_agents = lifecycle_config::agents(conn, node)?;
        let mut agents = BTreeMap::new();
        for key in lifecycle_config::PHASES {
            let skills = lifecycle_config::resolve(conn, node, key)?;
            let agent = lifecycle_config::resolve_agent(conn, node, key)?;
            let checks = lifecycle_config::resolve_checks(conn, node, key)?;
            let mut effective = Vec::new();
            if let Some((platform, _)) = agent.platform {
                effective.push(platform.label().to_owned());
            }
            if let Some((model, _)) = agent.model {
                effective.push(model);
            }
            if let Some((effort, _)) = agent.effort {
                effective.push(effort);
            }
            agents.insert(
                key,
                Agent {
                    takes_skills: lifecycle_config::takes_skills(key),
                    own_skills: own_skills
                        .get(key)
                        .map(|skills| lifecycle_config::format_list(skills)),
                    own_model: own_agents
                        .get(key)
                        .and_then(|agent| agent.model.clone())
                        .unwrap_or_default(),
                    own_effort: own_agents
                        .get(key)
                        .and_then(|agent| agent.effort.clone())
                        .unwrap_or_default(),
                    effective_skills: skills.map_or_else(String::new, |skills| {
                        if skills.skills.is_empty() {
                            "No extra skills".into()
                        } else {
                            lifecycle_config::format_list(&skills.skills)
                        }
                    }),
                    effective_agent: effective.join(" · "),
                    checks: checks.map_or_else(Vec::new, |checks| {
                        checks.checks.iter().map(|check| check.display()).collect()
                    }),
                },
            );
        }
        let gate_repo = GateRepo::new(conn);
        let mut gates = BTreeMap::new();
        for pair in flow::PHASES.windows(2) {
            let criteria = gate_repo
                .list_evaluations_for_transition(node, pair[0].id, pair[1].id)?
                .into_iter()
                .map(|(criterion, evaluation)| {
                    let (outcome, detail) = evaluation.map_or_else(
                        || ("Unchecked".into(), None),
                        |stored| (format!("Last check · {}", stored.outcome), stored.detail),
                    );
                    Criterion {
                        label: criterion.label,
                        outcome,
                        detail,
                    }
                })
                .collect();
            gates.insert(pair[0].id, criteria);
        }
        Ok(Some(Data {
            node: Some(id.into()),
            title: task.title,
            phase: repo.get_lifecycle(node)?.unwrap_or_default(),
            editable: caps.contains(&Capability::LifecycleConfig),
            review_pr_changes: lifecycle_config::resolve_pr_draft_review(conn, node)?.enabled,
            delete_sandbox: Some(lifecycle_config::resolve_delete_sandbox(conn, node)?.enabled),
            gates,
            agents,
        }))
    })?;
    data.ok_or_else(|| "This task is no longer in the workspace.".into())
}

fn run_runner(
    session: &Session,
    node: &str,
    verb: Verb,
) -> Result<String, Box<dyn std::error::Error>> {
    let done = runner_operator::perform(
        &session.ctx(),
        node.parse()?,
        verb,
        &Options::default(),
        &Who::Operator,
        &mut |_| {},
    )?;
    if done.failed {
        Err(done.message.into())
    } else {
        Ok(done.message)
    }
}

fn run_workflow_agent(
    session: &Session,
    task: uuid::Uuid,
) -> Result<String, Box<dyn std::error::Error>> {
    let context = session.ctx();
    let menu = runner_operator::menu_for(&context, task)?;
    let verb = menu
        .default
        .iter()
        .chain(menu.entries.iter())
        .find_map(|entry| match entry.verb {
            "start" => Some(Verb::Start),
            "resume" => Some(Verb::Resume),
            _ => None,
        })
        .ok_or("This task cannot start or resume its runner. Check its agent and working files.")?;
    let result = runner_operator::perform(
        &context,
        task,
        verb,
        &Options::default(),
        &Who::Operator,
        &mut |_| {},
    )?;
    if result.failed {
        Err(result.message.into())
    } else {
        Ok(result.message)
    }
}

fn load_records(
    fleet: &tod_store::fleet::FleetStore,
) -> Result<Vec<Record>, Box<dyn std::error::Error>> {
    let records = fleet.read(|conn| {
        let lists = ListRepo::new(conn).list_all()?;
        let placements = OutlineRepo::new(conn);
        let nodes = NodeRepo::new(conn);
        let mut locations = BTreeMap::new();
        let mut records = Vec::new();
        for list in lists {
            for placement in placements.list_for_list(list.id)? {
                locations.insert(placement.node_id, (list.id, list.title.clone()));
            }
            records.push(Record {
                item: Item {
                    id: list.id.to_string(),
                    title: list.title,
                    kind: Kind::Collection,
                    scope: None,
                },
                outline: None,
                description: format!("Saved outline · {}", list.slug),
                status: String::new(),
            });
        }
        for node in nodes.list_all()? {
            let task = tod_store::fleet::node_actions::is_task_node(conn, node.id)?;
            let kind = row_kind(
                task,
                !task
                    && nodes
                        .list_capabilities(node.id)?
                        .contains(&Capability::Spec),
            );
            let (outline, scope) = locations.remove(&node.id).unzip();
            records.push(Record {
                item: Item {
                    id: node.id.to_string(),
                    title: node.title,
                    kind,
                    scope,
                },
                outline: outline.map(|id| id.to_string()),
                description: format!("Node · {}", node.slug),
                status: String::new(),
            });
        }
        Ok(records)
    })?;
    Ok(records)
}

fn row_kind(task: bool, specification: bool) -> Kind {
    if task {
        Kind::Task
    } else if specification {
        Kind::ProductDefinition
    } else {
        Kind::Node
    }
}

pub(crate) struct WorkflowCatalogResult {
    pub draft: Option<tod_core::lifecycle::workflow::catalog::Draft>,
    pub library: Vec<crate::lifecycle_editor::LibraryEntry>,
    pub notice: String,
    pub actions: Vec<(String, tod_core::lifecycle::workflow::Revision)>,
}

impl Backend {
    pub(crate) fn workflow_catalog(
        &self,
        event: crate::lifecycle_editor::Event,
        generation: Option<u64>,
    ) -> tokio::task::JoinHandle<Result<WorkflowCatalogResult, String>> {
        use crate::lifecycle_editor::{Event, LibraryEntry};
        use tod_core::lifecycle::workflow::catalog::Catalog;
        let access = self.access.clone();
        self.runtime.spawn_blocking(move || {
            let operation = || -> Result<WorkflowCatalogResult, Box<dyn std::error::Error>> {
                let (_opening, session) = access.open()?;
                let fleet = &session.fleet;
                let catalog = Catalog::new(fleet);
                let save = |title: &str, definition: tod_core::lifecycle::workflow::Definition| {
                    if definition.revision.id.is_empty() {
                        catalog.create(title, definition)
                    } else {
                        let id = definition.revision.id.clone();
                        let generation = generation.ok_or_else(|| {
                            std::io::Error::other("Reload this draft before saving.")
                        })?;
                        catalog.save(&id, generation, title, definition)
                    }
                };
                let (draft, notice) = match event {
                    Event::ManageActions => unreachable!("handled by the app"),
                    Event::Refresh => (None, String::new()),
                    Event::Load(id) => {
                        let loaded = catalog
                            .draft(&id)?
                            .ok_or("Workflow draft no longer exists.")?;
                        (Some(loaded), String::new())
                    }
                    Event::Save { title, definition } => {
                        (Some(save(&title, definition)?), "Draft saved.".into())
                    }
                    Event::Publish { title, definition } => {
                        let draft = save(&title, definition)?;
                        let id = &draft.definition.revision.id;
                        match catalog.publish(id, draft.generation) {
                            Ok(revision) => {
                                let next = catalog
                                    .draft(id)?
                                    .ok_or("Published workflow draft disappeared.")?;
                                (
                                    Some(next),
                                    format!(
                                        "Published revision {}.",
                                        revision.definition.revision.number
                                    ),
                                )
                            }
                            Err(error) => (Some(draft), format!("Draft saved. {error:#}")),
                        }
                    }
                };
                let library = catalog
                    .list()?
                    .into_iter()
                    .map(|row| LibraryEntry {
                        id: row.id,
                        title: row.title,
                        revision: row.latest_revision,
                    })
                    .collect();
                let actions = tod_core::lifecycle::workflow::actions::ActionCatalog::new(fleet)
                    .list()?
                    .into_iter()
                    .map(|asset| (asset.spec.title, asset.revision))
                    .collect();
                Ok(WorkflowCatalogResult {
                    draft,
                    library,
                    notice,
                    actions,
                })
            };
            operation().map_err(|error| format!("{error:#}"))
        })
    }
}

impl Backend {
    pub(crate) fn workflow_run(
        &self,
        task: String,
        title: String,
        command: crate::workflow_runs::Command,
    ) -> tokio::task::JoinHandle<Result<(crate::workflow_runs::Snapshot, String), String>> {
        use crate::workflow_runs::{Command, Snapshot};
        use tod_core::lifecycle::workflow::{
            NodeKind, Observations, Source,
            builtin::{ACTIONS, MANUAL},
            catalog::Catalog,
            execute, improvement, metrics, migration,
            runner::Runner,
        };
        let access = self.access.clone();
        self.runtime.spawn_blocking(move || {
            let operation = || -> Result<(Snapshot, String), Box<dyn std::error::Error>> {
                let task = task.parse()?;
                let (_opening, session) = access.open()?;
                let fleet = &session.fleet;
                let cloud_location = !fleet.read(|conn| tod_store::workflow_run::allows_local_execution(conn, task))?;
                let (command, mut notice) = if let Command::RunAgent(run) = command {
                    {
                        if cloud_location {
                            return Err("Choose a local devcontainer location before running a workflow on this task.".into());
                        }
                        let runner = Runner::new(fleet);
                        let current = runner.current(task)?.ok_or("Workflow run not found.")?;
                        if current != run || run.status.is_terminal() {
                            return Err("The workflow run changed. Refresh it before starting the agent.".into());
                        }
                        let visit = runner.current_visit(&run)?;
                        let definition = execute::pinned_definition(fleet, &run)?;
                        if visit.output.is_some() || !matches!(
                            &definition.nodes[&run.node].kind,
                            NodeKind::Action { action }
                                if ACTIONS.iter().all(|(builtin, _)| action.id != *builtin)
                        ) {
                            return Err("This workflow visit is not waiting for an agent.".into());
                        }
                    }
                    (Command::Load, run_workflow_agent(&session, task)?)
                } else {
                    (command, String::new())
                };
                let catalog = Catalog::new(fleet);
                let runner = Runner::new(fleet);
                let current = runner.current(task)?;
                let run_id = || current.as_ref().map(|run| run.id);
                let own_run = |id| -> Result<_, Box<dyn std::error::Error>> {
                    Ok(runner.get(id)?.filter(|run| run.task == task).ok_or("This run does not belong to the selected task.")?)
                };
                let mut replay = None;
                let mut captured_context = None;
                let mut migration_preview = None;
                let mut adoption_preview = None;
                let mut detach_preview = None;
                let mut inspected = None;
                match command {
                    Command::Load => {}
                    Command::Inspect(run) => {
                        inspected = Some(own_run(run)?);
                    }
                    Command::InspectContext { run, visit } => {
                        inspected = Some(own_run(run)?);
                        captured_context = Some((visit, improvement::captured_context(fleet, run, visit)?));
                    }
                    Command::PreviewMigration { run, target } if run_id() == Some(run) => {
                        migration_preview = Some(migration::preview(fleet, run, &target)?);
                    }
                    Command::Migrate(preview) if run_id() == Some(preview.source.id) => {
                        let new_run = migration::apply(fleet, &preview)?;
                        notice = format!("Restarted on workflow revision {}.", new_run.revision.number);
                    }
                    Command::PreviewAdoption(target) => {
                        adoption_preview = Some(migration::preview_adoption(fleet, task, &target)?);
                    }
                    Command::Adopt(preview) if preview.task == task => {
                        let new_run = migration::adopt(fleet, &preview, 100)?;
                        notice = format!("Adopted workflow revision {} at {}.", new_run.revision.number, preview.stay.phase);
                    }
                    Command::PreviewDetach(run) if run_id() == Some(run) => {
                        detach_preview = Some(migration::preview_detach(fleet, run)?);
                    }
                    Command::Detach(preview) if run_id() == Some(preview.source.id) => {
                        migration::detach(fleet, &preview)?;
                        notice = "Task returned to the legacy runner.".into();
                    }
                    Command::Assign(revision) => {
                        let run = runner.assign(task, &revision, 100)?;
                        notice = execute::resume(fleet, run, &Observations::new())?.stopped.unwrap_or_default();
                    }
                    Command::Resume { run, observations } if run_id() == Some(run.id) => {
                        notice = execute::resume(fleet, run, &observations)?.stopped.unwrap_or_default();
                    }
                    Command::Complete { request, values, evidence } if run_id() == Some(request.run) => {
                        let evidence = (!evidence.trim().is_empty()).then(|| evidence.trim().to_owned());
                        execute::record_manual(fleet, &request, values, evidence.into_iter().collect())?;
                        let run = runner.get(request.run)?.ok_or("Workflow run not found.")?;
                        notice = execute::resume(fleet, run, &Observations::new())?.stopped.unwrap_or_default();
                    }
                    Command::Cancel(run) if run_id() == Some(run.id) => {
                        runner.cancel(&run)?;
                        notice = "Run cancelled.".into();
                    }
                    Command::Replay { run } => {
                        let known = own_run(run)?;
                        let candidate = catalog.draft(&known.revision.id)?.ok_or("Workflow draft not found.")?;
                        replay = Some(improvement::compare(fleet, run, &candidate.definition)?);
                        inspected = Some(known);
                    }
                    Command::Lesson { run, sequence, subject, text } => {
                        inspected = Some(own_run(run)?);
                        let lesson = improvement::record_subject_lesson(fleet, run, sequence, &subject, &text)?;
                        notice = format!("Recorded lesson {lesson}.");
                    }
                    _ => return Err("The task's workflow run changed. Reopen it to refresh.".into()),
                }
                let choices = catalog.list()?.into_iter().filter_map(|item| {
                    item.latest_revision.map(|number| (
                        item.title, tod_core::lifecycle::workflow::Revision { id: item.id, number },
                    ))
                }).collect();
                let mut snapshot = Snapshot { title, choices, replay, cloud_location, ..Snapshot::default() };
                for (_, revision) in &snapshot.choices {
                    if catalog.published(&revision.id, revision.number)?
                        .is_some_and(|published| !published.definition.task_phases.is_empty())
                    {
                        snapshot.mapped_workflows.insert(revision.id.clone());
                    }
                }
                snapshot.captured_context = captured_context;
                snapshot.migration = migration_preview;
                snapshot.adoption = adoption_preview;
                snapshot.detach = detach_preview;
                snapshot.history = runner.history(task)?;
                snapshot.metrics = metrics::for_task(fleet, task)?;
                let current = runner.current(task)?;
                snapshot.active_run = current.as_ref().map(|run| run.id);
                if let Some(run) = inspected.or(current) {
                    let definition = execute::pinned_definition(fleet, &run)?;
                    let node = definition.nodes.get(&run.node).ok_or("Workflow node not found.")?;
                    let visit = runner.current_visit(&run)?;
                    snapshot.agent_pending = visit.output.is_none() && matches!(
                        &node.kind, NodeKind::Action { action }
                            if ACTIONS.iter().all(|(builtin, _)| action.id != *builtin)
                    );
                    snapshot.node_label = node.label.clone();
                    snapshot.request = visit.dispatch.filter(|request| visit.output.is_none() && request.action.id == MANUAL);
                    snapshot.action_fields = node.outputs.clone();
                    snapshot.manual_fields = definition.fields.iter().filter_map(|(name, field)| {
                        matches!(&field.source, Source::Provider { binding, .. } if binding == MANUAL)
                            .then(|| (name.clone(), field.value_type))
                    }).collect();
                    snapshot.traces = runner.traces(run.id)?;
                    for entry in &snapshot.traces {
                        snapshot.subjects.insert(
                            entry.sequence,
                            improvement::subjects(fleet, &definition, &entry.trace)?,
                        );
                    }
                    snapshot.visits = runner.visits(run.id)?;
                    snapshot.definition = Some(definition);
                    snapshot.run = Some(run);
                }
                Ok((snapshot, notice))
            };
            operation().map_err(|error| format!("{error:#}"))
        })
    }
}

pub(crate) struct WorkflowAssetsResult {
    pub library: Vec<tod_core::lifecycle::workflow::actions::PublishedAction>,
    pub history: Option<Vec<tod_core::lifecycle::workflow::actions::PublishedAction>>,
    pub saved: Option<tod_core::lifecycle::workflow::actions::PublishedAction>,
}

impl Backend {
    pub(crate) fn workflow_assets(
        &self,
        command: crate::workflow_assets::Command,
    ) -> tokio::task::JoinHandle<Result<WorkflowAssetsResult, String>> {
        use crate::workflow_assets::Command;
        use tod_core::lifecycle::workflow::actions::ActionCatalog;
        let access = self.access.clone();
        self.runtime.spawn_blocking(move || {
            let operation = || -> Result<WorkflowAssetsResult, Box<dyn std::error::Error>> {
                let (_opening, session) = access.open()?;
                let fleet = &session.fleet;
                let catalog = ActionCatalog::new(fleet);
                let (history, saved) = match command {
                    Command::Load => (None, None),
                    Command::History(id) => (Some(catalog.revisions(&id)?), None),
                    Command::Publish { base, spec } => {
                        let asset = catalog.publish(
                            base.as_ref().map(|base| base.id.as_str()),
                            base.as_ref().map(|base| base.number),
                            spec,
                        )?;
                        let history = catalog.revisions(&asset.revision.id)?;
                        (Some(history), Some(asset))
                    }
                };
                Ok(WorkflowAssetsResult {
                    library: catalog.list()?,
                    history,
                    saved,
                })
            };
            operation().map_err(|error| format!("{error:#}"))
        })
    }
}
