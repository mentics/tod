//! A local fixture that exercises the workspace effects without running an agent.

use std::collections::{BTreeMap, BTreeSet};
use tod_palette::{Command, Context, Effect, Item, Kind, Palette, Target};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub item: Item,
    pub outline: Option<String>,
    pub description: String,
    pub status: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Membership {
    All,
    Status(&'static str),
    Pinned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    records: std::sync::Arc<[Record]>,
    demo_collections: BTreeMap<&'static str, Membership>,
    commands: Vec<Command>,
    live: bool,
    pub cursor: String,
    pub detail: String,
    pub collection: String,
    pub selected: BTreeSet<String>,
    pub pinned: BTreeSet<String>,
    pub message: String,
    pub frequency: BTreeMap<String, u32>,
}

const COMMANDS: &[Command] = &[
    Command {
        id: "pin",
        label: "Pin task",
        plural: Some("Pin tasks"),
        aliases: "bookmark",
        target: Target::Many(Kind::Task),
    },
    Command {
        id: "pause",
        label: "Pause runner",
        plural: None,
        aliases: "suspend",
        target: Target::One(Kind::Task),
    },
    Command {
        id: "resume",
        label: "Resume runner",
        plural: None,
        aliases: "continue",
        target: Target::One(Kind::Task),
    },
    Command {
        id: "conversation",
        label: "Chat with an agent about this",
        plural: None,
        aliases: "chat discuss open agent conversation",
        target: Target::One(Kind::Task),
    },
    Command {
        id: "edit-collection",
        label: "Edit collection",
        plural: None,
        aliases: "configure sort group nest",
        target: Target::One(Kind::Collection),
    },
    Command {
        id: "shortcuts",
        label: "Keyboard shortcuts",
        plural: None,
        aliases: "hotkeys help key bindings",
        target: Target::Global,
    },
];

impl Workspace {
    pub fn demo(frequency: BTreeMap<String, u32>) -> Self {
        use Kind::*;
        let records: std::sync::Arc<[Record]> = [
            (
                "sync-retries",
                "Fix sync retries",
                Task,
                Some("Sync"),
                "Retry transient failures without creating duplicate jobs.",
                "Running",
            ),
            (
                "retry-policy",
                "Review retry policy",
                Task,
                Some("Sync"),
                "Decide which failure types can be retried automatically.",
                "Needs a decision",
            ),
            (
                "webhooks",
                "Trace failed webhooks",
                Task,
                Some("Sync"),
                "Find out why delivery stopped after reconnecting.",
                "Error",
            ),
            (
                "imports",
                "Validate catalog imports",
                Task,
                Some("Catalog"),
                "Check imported records before publishing them.",
                "Running",
            ),
            (
                "search-index",
                "Rebuild search index",
                Task,
                Some("Catalog"),
                "Recover records missing from the search index.",
                "Error",
            ),
            (
                "offline-contract",
                "Offline sync contract",
                ProductDefinition,
                Some("Sync"),
                "Define changes users can make while offline and how to reconcile them.",
                "",
            ),
            (
                "retry-worker",
                "Retry worker",
                AgentRun,
                Some("Sync"),
                "Agent run for Fix sync retries.",
                "Active",
            ),
            (
                "recurring-failures",
                "Review recurring failures",
                ProcessImprovement,
                Some("Catalog"),
                "Review which failures could be prevented by a shared check.",
                "",
            ),
            (
                "all-tasks",
                "All tasks",
                Collection,
                None,
                "All tasks, grouped by project.",
                "",
            ),
            (
                "needs-my-decision",
                "Needs my decision",
                Collection,
                None,
                "Tasks where the agent has requested a decision.",
                "",
            ),
            (
                "errors",
                "Errors",
                Collection,
                None,
                "Tasks whose last run ended with an error.",
                "",
            ),
            (
                "pinned",
                "Pinned tasks",
                Collection,
                None,
                "Tasks manually pinned from anywhere in the workspace.",
                "",
            ),
        ]
        .into_iter()
        .map(|(id, title, kind, scope, description, status)| Record {
            item: Item {
                id: id.into(),
                title: title.into(),
                kind,
                scope: scope.map(str::to_owned),
            },
            outline: None,
            description: description.into(),
            status: status.into(),
        })
        .collect();
        Self {
            records,
            demo_collections: BTreeMap::from([
                ("all-tasks", Membership::All),
                ("needs-my-decision", Membership::Status("Needs a decision")),
                ("errors", Membership::Status("Error")),
                ("pinned", Membership::Pinned),
            ]),
            commands: COMMANDS.to_vec(),
            live: false,
            cursor: "sync-retries".into(),
            detail: "sync-retries".into(),
            collection: "all-tasks".into(),
            selected: BTreeSet::new(),
            pinned: BTreeSet::new(),
            message: String::new(),
            frequency,
        }
    }

    #[cfg(feature = "live")]
    pub fn live(records: Vec<Record>, frequency: BTreeMap<String, u32>) -> Self {
        let collection = records
            .iter()
            .find(|record| record.item.kind == Kind::Collection)
            .map_or_else(String::new, |record| record.item.id.clone());
        let mut workspace = Self {
            records: records.into(),
            demo_collections: BTreeMap::new(),
            commands: COMMANDS
                .iter()
                .filter(|command| matches!(command.id, "shortcuts" | "pause" | "resume" | "conversation"))
                .cloned()
                .collect(),
            live: true,
            collection,
            cursor: String::new(),
            detail: String::new(),
            selected: BTreeSet::new(),
            pinned: BTreeSet::new(),
            message: String::new(),
            frequency,
        };
        if let Some(task) = workspace.visible_tasks().first() {
            workspace.cursor = task.item.id.clone();
            workspace.detail = task.item.id.clone();
        }
        workspace
    }

    pub fn label(&self) -> &'static str {
        if self.live {
            "Command palette"
        } else {
            "Command palette demo"
        }
    }

    pub fn record(&self, id: &str) -> Option<&Record> {
        self.records.iter().find(|record| record.item.id == id)
    }

    pub fn collections(&self) -> impl Iterator<Item = &Record> {
        self.records
            .iter()
            .filter(|record| record.item.kind == Kind::Collection)
    }

    pub fn visible_tasks(&self) -> Vec<Record> {
        let mut records: Vec<_> = self
            .records
            .iter()
            .filter(|record| {
                record.item.kind == Kind::Task
                    || (self.live && record.item.kind != Kind::Collection)
            })
            .filter(|record| {
                if self.live {
                    record.outline.as_deref() == Some(self.collection.as_str())
                } else {
                    match self.demo_collections.get(self.collection.as_str()) {
                        Some(Membership::All) => true,
                        Some(Membership::Status(status)) => record.status == *status,
                        Some(Membership::Pinned) => self.pinned.contains(&record.item.id),
                        None => false,
                    }
                }
            })
            .cloned()
            .collect();
        records.sort_by(|left, right| {
            left.item
                .scope
                .cmp(&right.item.scope)
                .then_with(|| left.item.title.cmp(&right.item.title))
        });
        records
    }

    pub fn palette(
        &self,
        invoked: Option<&str>,
        actions_only: bool,
        extra: Vec<Item>,
        context: Option<Context>,
    ) -> Palette {
        let focused = self.record(&self.cursor).map(|record| &record.item);
        let context = context.unwrap_or_else(|| Context {
            focused: focused.map(|item| item.id.clone()),
            selected: self.selected.iter().cloned().collect(),
            scope: focused.and_then(|item| item.scope.clone()),
        });
        let actions = actions_only.then(|| context.targets(invoked));
        Palette::new(
            self.records
                .iter()
                .map(|record| record.item.clone())
                .chain(extra)
                .collect(),
            self.commands.clone(),
            context,
            self.frequency.clone(),
            actions,
        )
    }

    pub fn move_cursor(&mut self, forward: bool, by_group: bool) {
        let rows = self.visible_tasks();
        let Some(index) = rows.iter().position(|record| record.item.id == self.cursor) else {
            if let Some(first) = rows.first() {
                self.cursor = first.item.id.clone();
            }
            return;
        };
        let next = if !by_group {
            if forward {
                (index + 1 < rows.len()).then_some(index + 1)
            } else {
                index.checked_sub(1)
            }
        } else if forward {
            (index + 1..rows.len()).find(|next| rows[*next].item.scope != rows[index].item.scope)
        } else {
            (0..index)
                .rev()
                .find(|previous| rows[*previous].item.scope != rows[index].item.scope)
                .and_then(|previous| {
                    (0..=previous)
                        .find(|first| rows[*first].item.scope == rows[previous].item.scope)
                })
        };
        if let Some(next) = next {
            self.cursor = rows[next].item.id.clone();
        }
    }

    pub fn toggle_mark(&mut self, id: &str) {
        if !self.selected.remove(id) {
            self.selected.insert(id.into());
        }
    }

    pub fn record_use(&mut self, command: &str) {
        let uses = self.frequency.entry(command.into()).or_default();
        *uses = uses.saturating_add(1);
    }

    pub fn apply(&mut self, effect: Effect) -> Result<(), String> {
        match effect {
            Effect::Open(id) => {
                let item = &self
                    .record(&id)
                    .ok_or("This item is no longer in the workspace.")?
                    .item;
                if item.kind == Kind::Collection {
                    self.collection = id.clone();
                    self.selected.clear();
                }
                self.cursor = id.clone();
                self.detail = id;
            }
            Effect::Run { command, targets } => {
                let names = targets
                    .iter()
                    .map(|id| {
                        self.record(id)
                            .map(|record| record.item.title.clone())
                            .ok_or("A target is no longer in the workspace.")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.message = match command {
                    "shortcuts" => {
                        self.detail = "shortcuts".into();
                        String::new()
                    }
                    "pin" => {
                        self.pinned.extend(targets);
                        format!("Pinned {} to Pinned tasks.", names.join(", "))
                    }
                    "edit-collection" => {
                        self.detail = targets[0].clone();
                        "Opened the collection config preview.".into()
                    }
                    "pause" | "resume" => {
                        let task = std::sync::Arc::make_mut(&mut self.records)
                            .iter_mut()
                            .find(|record| record.item.id == targets[0])
                            .ok_or("This task is no longer in the workspace.")?;
                        let (required, next) = if command == "pause" {
                            ("Running", "Paused")
                        } else {
                            ("Paused", "Running")
                        };
                        if task.status != required {
                            return Err(format!(
                                "{} must have a {} runner.",
                                task.item.title,
                                required.to_lowercase()
                            ));
                        }
                        task.status = next.into();
                        format!("{next} the runner for {}.", task.item.title)
                    }
                    _ => return Err(format!("Unknown command {command}.")),
                };
                self.record_use(command);
            }
        }
        Ok(())
    }
}
