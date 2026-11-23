//! Command palette policy shared by desktop frontends.

mod search;
pub use search::{normalize, quality};

use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Node,
    Task,
    AgentRun,
    ProductDefinition,
    ProcessImprovement,
    Collection,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Node => "Node",
            Self::Task => "Task",
            Self::AgentRun => "Agent run",
            Self::ProductDefinition => "Product definition",
            Self::ProcessImprovement => "Process improvement",
            Self::Collection => "Collection",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub kind: Kind,
    pub scope: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    pub focused: Option<String>,
    pub selected: Vec<String>,
    pub scope: Option<String>,
}

impl Context {
    pub fn targets(&self, invoked: Option<&str>) -> Vec<String> {
        if !self.selected.is_empty()
            && invoked.is_none_or(|id| self.selected.iter().any(|selected| selected == id))
        {
            return self.selected.clone();
        }
        invoked
            .or(self.focused.as_deref())
            .map(str::to_owned)
            .into_iter()
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Global,
    One(Kind),
    Many(Kind),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub id: &'static str,
    pub label: &'static str,
    pub plural: Option<&'static str>,
    pub aliases: &'static str,
    pub target: Target,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Open(String),
    Run {
        command: &'static str,
        targets: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultRow {
    pub title: String,
    pub detail: String,
    pub kind: &'static str,
    pub effect: Effect,
    pub offers_actions: bool,
    choose_target: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub start: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode {
    Search,
    Actions(Vec<String>),
    Choose(&'static str, Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Page {
    mode: Mode,
    query: String,
    highlight: Option<Effect>,
    order: [usize; 3],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    items: Arc<[Item]>,
    commands: Arc<[Command]>,
    context: Context,
    frequency: BTreeMap<String, u32>,
    page: Page,
    back: Vec<Page>,
    rows: Vec<ResultRow>,
    sections: Vec<Section>,
    active: Option<usize>,
}

impl Palette {
    pub fn new(
        items: Vec<Item>,
        commands: Vec<Command>,
        context: Context,
        frequency: BTreeMap<String, u32>,
        actions: Option<Vec<String>>,
    ) -> Self {
        let mut palette = Self {
            items: items.into(),
            commands: commands.into(),
            context,
            frequency,
            page: Page {
                mode: actions.map_or(Mode::Search, Mode::Actions),
                query: String::new(),
                highlight: None,
                order: [0, 1, 2],
            },
            back: Vec::new(),
            rows: Vec::new(),
            sections: Vec::new(),
            active: None,
        };
        palette.rebuild();
        palette
    }

    pub fn query(&self) -> &str {
        &self.page.query
    }
    pub fn rows(&self) -> &[ResultRow] {
        &self.rows
    }
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }
    pub fn active(&self) -> Option<usize> {
        self.active
    }
    pub fn title(&self) -> String {
        match &self.page.mode {
            Mode::Search => "Search tod".into(),
            Mode::Actions(ids) => format!("Actions for {}", self.target_label(ids)),
            Mode::Choose(command, _) => {
                let label = self
                    .commands
                    .iter()
                    .find(|entry| entry.id == *command)
                    .map_or("Command", |entry| entry.label);
                format!("Choose a target · {label}")
            }
        }
    }

    pub fn set_query(&mut self, text: String) {
        if self.page.query != text {
            self.page.query = text;
            self.page.highlight = None;
            self.rebuild();
        }
    }

    pub fn move_highlight(&mut self, forward: bool, by_section: bool) {
        let Some(index) = self.active else { return };
        if by_section {
            let current = self
                .sections
                .partition_point(|section| section.start <= index)
                - 1;
            let destination = if forward {
                Some(current + 1)
            } else {
                current.checked_sub(1)
            };
            if let Some(section) = destination.and_then(|index| self.sections.get(index)) {
                self.active = Some(section.start);
            }
        } else {
            self.active = Some(if forward {
                (index + 1).min(self.rows.len() - 1)
            } else {
                index.saturating_sub(1)
            });
        }
    }

    pub fn actions(&mut self) -> bool {
        let Some(row) = self
            .active
            .and_then(|index| self.rows.get(index))
            .filter(|row| row.offers_actions)
        else {
            return false;
        };
        let Effect::Open(id) = &row.effect else {
            return false;
        };
        self.push(Mode::Actions(vec![id.clone()]));
        true
    }

    pub fn activate(&mut self, index: usize) -> Option<Effect> {
        let row = self.rows.get(index)?;
        self.active = Some(index);
        if row.choose_target {
            if let Effect::Run { command, targets } = &row.effect {
                self.push(Mode::Choose(command, targets.clone()));
            }
            return None;
        }
        Some(row.effect.clone())
    }

    /// Returns false when Escape should close the palette.
    pub fn back(&mut self) -> bool {
        let Some(page) = self.back.pop() else {
            return false;
        };
        self.page = page;
        self.rebuild();
        true
    }

    fn push(&mut self, mode: Mode) {
        self.page.highlight = self.active.map(|index| self.rows[index].effect.clone());
        self.back.push(std::mem::replace(
            &mut self.page,
            Page {
                mode,
                query: String::new(),
                highlight: None,
                order: [0, 1, 2],
            },
        ));
        self.rebuild();
    }

    fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }
    fn target_label(&self, ids: &[String]) -> String {
        if let [id] = ids {
            return self
                .item(id)
                .map_or_else(|| "Item".into(), |item| item.title.clone());
        }
        if ids.is_empty() {
            "Here".into()
        } else {
            format!("{} selected tasks", ids.len())
        }
    }

    fn rebuild(&mut self) {
        let query = normalize(&self.page.query);
        let targets = match &self.page.mode {
            Mode::Search => self.context.targets(None),
            Mode::Actions(ids) | Mode::Choose(_, ids) => ids.clone(),
        };
        let mut candidates: [Vec<(u32, ResultRow)>; 3] = Default::default();
        let focused_scope = self
            .context
            .focused
            .as_deref()
            .and_then(|id| self.item(id))
            .and_then(|item| item.scope.as_ref());
        let scope = if targets.len() > 1 {
            let first = targets
                .first()
                .and_then(|id| self.item(id))
                .and_then(|item| item.scope.as_ref());
            first.filter(|scope| {
                targets
                    .iter()
                    .all(|id| self.item(id).and_then(|item| item.scope.as_ref()) == Some(*scope))
            })
        } else {
            self.context.scope.as_ref().or(focused_scope)
        }
        .cloned();
        if let Mode::Choose(command, ids) = &self.page.mode {
            for id in ids {
                if let Some(item) = self.item(id) {
                    candidates[0].push((
                        match_score(&query, &item.title, "", ""),
                        ResultRow {
                            title: item.title.clone(),
                            detail: item.kind.label().into(),
                            kind: "Target",
                            effect: Effect::Run {
                                command,
                                targets: vec![id.clone()],
                            },
                            offers_actions: false,
                            choose_target: false,
                        },
                    ));
                }
            }
        } else {
            let search = self.page.mode == Mode::Search;
            if search {
                for item in self.items.iter() {
                    let group = if targets.contains(&item.id) {
                        0
                    } else if scope.is_some() && item.scope.as_ref() == scope.as_ref() {
                        1
                    } else {
                        2
                    };
                    candidates[group].push((
                        match_score(&query, &item.title, &item.id, ""),
                        ResultRow {
                            title: item.title.clone(),
                            detail: item.scope.clone().unwrap_or_default(),
                            kind: item.kind.label(),
                            effect: Effect::Open(item.id.clone()),
                            offers_actions: true,
                            choose_target: false,
                        },
                    ));
                }
            }
            for command in self.commands.iter() {
                let (group, ids, choose_target) = match command.target {
                    Target::Global if search => (2, Vec::new(), false),
                    Target::Global => continue,
                    Target::One(kind) | Target::Many(kind)
                        if !targets.is_empty()
                            && targets
                                .iter()
                                .all(|id| self.item(id).is_some_and(|item| item.kind == kind)) =>
                    {
                        let choose = targets.len() > 1 && matches!(command.target, Target::One(_));
                        (0, targets.clone(), choose)
                    }
                    _ => continue,
                };
                let label = if ids.len() > 1 {
                    command.plural.unwrap_or(command.label)
                } else {
                    command.label
                };
                let names = ids
                    .iter()
                    .filter_map(|id| self.item(id))
                    .map(|item| item.title.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let score = match_score(&query, label, command.aliases, &names);
                if score == 0 || (search && query.is_empty() && choose_target) {
                    continue;
                }
                let boost = self
                    .frequency
                    .get(command.id)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1)
                    .ilog2()
                    .min(6);
                let detail = if ids.len() > 1 {
                    format!("{} · {names}", self.target_label(&ids))
                } else {
                    names
                };
                candidates[group].push((
                    (score + boost).saturating_sub(if choose_target { 13 } else { 0 }),
                    ResultRow {
                        title: label.into(),
                        detail: if choose_target {
                            format!("Choose one task · {detail}")
                        } else {
                            detail
                        },
                        kind: "Command",
                        effect: Effect::Run {
                            command: command.id,
                            targets: ids,
                        },
                        offers_actions: false,
                        choose_target,
                    },
                ));
            }
        }

        for group in &mut candidates {
            group.retain(|(score, _)| *score > 0);
            group.sort_by(|(left_score, left), (right_score, right)| {
                right_score
                    .cmp(left_score)
                    .then_with(|| left.title.cmp(&right.title))
            });
        }
        if query.is_empty() {
            self.page.order = [0, 1, 2];
        } else {
            let highest = |group: usize| candidates[group].first().map_or(0, |(score, _)| *score);
            for index in 1..self.page.order.len() {
                let mut index = index;
                while index > 0
                    && highest(self.page.order[index]) >= highest(self.page.order[index - 1]) + 13
                {
                    self.page.order.swap(index, index - 1);
                    index -= 1;
                }
            }
        }

        let headings = [
            self.target_label(&targets),
            scope.unwrap_or_else(|| "Workspace".into()),
            "Everywhere".into(),
        ];
        self.rows.clear();
        self.sections.clear();
        for index in self.page.order {
            if !candidates[index].is_empty() {
                self.sections.push(Section {
                    heading: headings[index].clone(),
                    start: self.rows.len(),
                });
                self.rows
                    .extend(candidates[index].drain(..).map(|(_, row)| row));
            }
        }
        self.active = self
            .page
            .highlight
            .as_ref()
            .and_then(|effect| self.rows.iter().position(|row| &row.effect == effect))
            .or_else(|| (!self.rows.is_empty()).then_some(0));
    }
}

fn match_score(query: &str, label: &str, aliases: &str, target: &str) -> u32 {
    if query.is_empty() {
        return 12;
    }
    quality(query, label)
        .max(quality(query, aliases).saturating_sub(16))
        .max(quality(query, &format!("{label} {target}")).saturating_sub(11))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_on_an_unselected_row_overrides_ambient_selection() {
        let context = Context {
            focused: Some("cursor".into()),
            selected: vec!["one".into(), "two".into()],
            scope: None,
        };
        assert_eq!(context.targets(None), ["one", "two"]);
        assert_eq!(context.targets(Some("two")), ["one", "two"]);
        assert_eq!(context.targets(Some("three")), ["three"]);
    }

    #[test]
    fn strong_global_match_outranks_context_and_frequency() {
        let mut palette = Palette::new(
            vec![Item {
                id: "one".into(),
                title: "Shortcut conventions".into(),
                kind: Kind::Task,
                scope: Some("Sync".into()),
            }],
            vec![
                Command {
                    id: "local",
                    label: "Review shortcut conventions",
                    plural: None,
                    aliases: "",
                    target: Target::One(Kind::Task),
                },
                Command {
                    id: "global",
                    label: "Keyboard shortcuts",
                    plural: None,
                    aliases: "",
                    target: Target::Global,
                },
            ],
            Context {
                focused: Some("one".into()),
                ..Context::default()
            },
            BTreeMap::from([("local".into(), 10_000)]),
            None,
        );
        assert_eq!(palette.sections()[0].heading, "Shortcut conventions");
        palette.set_query("keyboard shortcuts".into());
        assert_eq!(palette.sections()[0].heading, "Everywhere");
        assert_eq!(
            palette.activate(0),
            Some(Effect::Run {
                command: "global",
                targets: vec![]
            })
        );
    }
}
