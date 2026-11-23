//! Adapter for the existing TOD read model. ZGX uses the same store-opening policy as TOD’s CLI.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tod_core::runner_operator::{self, Options, Session, Verb, Who};
use tod_palette::{Effect, Item, Kind};
use tod_store::outline::{
    repos::{ListRepo, NodeRepo, OutlineRepo, TreeLoader},
    types::Capability,
};

use crate::{
    collection::OutlineData,
    workspace::{Record, Workspace},
};

#[derive(Clone)]
pub struct Backend {
    root: PathBuf,
    runtime: Arc<tokio::runtime::Runtime>,
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
            root,
            runtime: Arc::new(runtime),
        })
    }

    pub fn load(
        &self,
        frequency: BTreeMap<String, u32>,
    ) -> Result<Workspace, Box<dyn std::error::Error>> {
        let _entered = self.runtime.enter();
        let fleet = tod_core::direct_store::open(&self.root, false)?;
        Ok(Workspace::live(load_records(&fleet)?, frequency))
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
        let root = self.root.clone();
        let targets = targets.clone();
        let task = self.runtime.spawn_blocking(move || {
            let [node] = targets.as_slice() else {
                return Err("Choose one task to control its runner.".into());
            };
            run_runner(&root, node, verb).map_err(|error| format!("{error:#}"))
        });
        Some((command, task))
    }

    pub fn load_outline(&self, id: String) -> tokio::task::JoinHandle<Result<OutlineData, String>> {
        let root = self.root.clone();
        self.runtime.spawn_blocking(move || {
            let load = || -> Result<OutlineData, Box<dyn std::error::Error>> {
                let id = id.parse()?;
                let fleet = tod_core::direct_store::open(&root, false)?;
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
}

fn run_runner(root: &Path, node: &str, verb: Verb) -> Result<String, Box<dyn std::error::Error>> {
    let session = Session::open(root)?;
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
