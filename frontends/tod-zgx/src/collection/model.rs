use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
    sync::Arc,
};

const COLLECTION_SPEC: &str = include_str!("../../../../doc/redesign/collection-panel.md");
const PRODUCT_SPEC: &str = include_str!("../../../../doc/redesign/product-definitions.md");
const COMPONENT: &str = "collection-panel";
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Item {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub kind: &'static str,
    pub status: &'static str,
    pub phase: Arc<str>,
    pub tags: Arc<[String]>,
    pub cost: Option<u32>,
    pub parent: Option<Arc<str>>,
    pub detail: Option<Arc<str>>,
}

pub fn items() -> Vec<Item> {
    let design: Arc<[String]> = Arc::from(["Design".into()]);
    let task = Item {
        id: "verify-keyboard".into(),
        name: "Verify native keyboard navigation".into(),
        kind: "Task",
        status: "Active",
        phase: "Build".into(),
        tags: design.clone(),
        cost: Some(19),
        parent: Some(COMPONENT.into()),
        detail: None,
    };
    let mut items = vec![
        Item {
            id: COMPONENT.into(),
            name: "Collection panel".into(),
            kind: "Product component",
            status: "Draft",
            phase: "".into(),
            tags: design,
            cost: None,
            parent: None,
            detail: None,
        },
        task.clone(),
        Item {
            id: "check-focus".into(),
            name: "Check focus after cancelling a chip edit".into(),
            status: "Blocked",
            phase: "Review".into(),
            cost: Some(28),
            parent: Some("verify-keyboard".into()),
            ..task.clone()
        },
        Item {
            id: "test-pages".into(),
            name: "Test paging through a collection".into(),
            cost: Some(11),
            ..task.clone()
        },
        Item {
            id: "compare-arrangements".into(),
            name: "Compare grouped and nested collections".into(),
            cost: Some(8),
            ..task
        },
    ];
    items.extend(requirements(COLLECTION_SPEC, Some(COMPONENT)));
    items.extend(requirements(PRODUCT_SPEC, None).filter(|item| item.id.as_ref() == "REQ-SPEC-01"));
    items
}

fn requirements(source: &'static str, parent: Option<&'static str>) -> impl Iterator<Item = Item> {
    source.split("\n## ").filter_map(move |section| {
        let (heading, body) = section.split_once('\n')?;
        let (id, name) = heading.split_once(" · ")?;
        if !id.starts_with("REQ-") {
            return None;
        }
        Some(Item {
            id: id.into(),
            name: name.into(),
            kind: "Requirement",
            status: "Draft",
            phase: "".into(),
            tags: Arc::from(["Design".into(), "Specification".into()]),
            cost: None,
            parent: parent.map(Into::into),
            detail: Some(body.trim().into()),
        })
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Chip {
    Tag(String),
    Kind(String),
    Text(String),
    Filter {
        field: Field,
        operator: Operator,
        value: String,
    },
    Show(Vec<Column>),
    Sort {
        field: SortField,
        descending: bool,
    },
    Group(GroupField),
    Nest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SortField {
    Name,
    Cost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GroupField {
    Kind,
    Status,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Column {
    Kind,
    Status,
    Phase,
    Cost,
    Tags,
}

impl Column {
    pub const ALL: [Self; 5] = [
        Self::Kind,
        Self::Status,
        Self::Phase,
        Self::Cost,
        Self::Tags,
    ];
    pub const DEFAULT: [Self; 3] = [Self::Kind, Self::Status, Self::Phase];

    pub fn id(self) -> &'static str {
        match self {
            Self::Kind => "item-type",
            Self::Status => "status",
            Self::Phase => "lifecycle-phase",
            Self::Cost => "agent-cost",
            Self::Tags => "tags",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Kind => Field::Kind.label(),
            Self::Status => Field::Status.label(),
            Self::Phase => Field::Phase.label(),
            Self::Cost => Field::Cost.label(),
            Self::Tags => "Tags",
        }
    }

    pub fn value(self, item: &Item) -> String {
        match self {
            Self::Kind => item.kind.into(),
            Self::Status => item.status.into(),
            Self::Phase => item.phase.to_string(),
            Self::Cost => item
                .cost
                .map_or_else(String::new, |cost| format!("${cost}")),
            Self::Tags => item.tags.join(", "),
        }
    }

    pub fn parse(label: &str) -> Option<Self> {
        let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
        Self::ALL
            .into_iter()
            .find(|column| column.label().eq_ignore_ascii_case(&label))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Field {
    Tag,
    Kind,
    Status,
    Phase,
    Cost,
    Text,
    Show,
    Sort,
    Group,
    Nest,
}

impl Field {
    pub const ALL: [Self; 10] = [
        Self::Tag,
        Self::Kind,
        Self::Status,
        Self::Phase,
        Self::Cost,
        Self::Text,
        Self::Show,
        Self::Sort,
        Self::Group,
        Self::Nest,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Tag => "Tag",
            Self::Kind => "Item type",
            Self::Status => "Status",
            Self::Phase => "Lifecycle phase",
            Self::Cost => "Agent cost",
            Self::Text => "Text",
            Self::Show => "Show",
            Self::Sort => "Sort",
            Self::Group => "Group",
            Self::Nest => "Nest",
        }
    }

    pub fn alias(self) -> Option<&'static str> {
        match self {
            Self::Sort => Some("Order"),
            Self::Show => Some("Columns"),
            _ => None,
        }
    }

    pub fn operators(self) -> &'static [Operator] {
        use Operator::*;
        match self {
            Self::Sort | Self::Group | Self::Nest => &[By],
            Self::Text => &[Contains, DoesNotContain],
            Self::Cost => &[Greater, Less, AtLeast, AtMost, Is],
            Self::Show => &[],
            _ => &[Is, IsNot],
        }
    }

    pub fn is_arrangement(self) -> bool {
        self.operators() == [Operator::By]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Operator {
    Is,
    IsNot,
    Contains,
    DoesNotContain,
    By,
    Greater,
    Less,
    AtLeast,
    AtMost,
}

impl Operator {
    pub const ALL: [Self; 9] = [
        Self::DoesNotContain,
        Self::IsNot,
        Self::Contains,
        Self::Is,
        Self::By,
        Self::Greater,
        Self::Less,
        Self::AtLeast,
        Self::AtMost,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Is => "is",
            Self::IsNot => "is not",
            Self::Contains => "contains",
            Self::DoesNotContain => "does not contain",
            Self::By => "by",
            Self::Greater => ">",
            Self::Less => "<",
            Self::AtLeast => "≥",
            Self::AtMost => "≤",
        }
    }
}

impl Chip {
    pub fn label(&self) -> String {
        match self {
            Self::Tag(tag) => format!("Tag is {tag}"),
            Self::Kind(kind) => format!("Item type is {kind}"),
            Self::Text(query) => format!("Text contains {query}"),
            Self::Filter {
                field,
                operator,
                value,
            } => format!("{} {} {value}", field.label(), operator.label()),
            Self::Show(columns) if columns.is_empty() => "Show Title".into(),
            Self::Show(columns) => format!(
                "Show {}",
                columns
                    .iter()
                    .map(|column| column.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Sort { field, descending } => format!(
                "Sort by {} {}",
                if *field == SortField::Name {
                    "Name"
                } else {
                    "Agent cost"
                },
                if *descending {
                    "descending"
                } else {
                    "ascending"
                },
            ),
            Self::Group(GroupField::Kind) => "Group by Item type".to_owned(),
            Self::Group(GroupField::Status) => "Group by Status".to_owned(),
            Self::Nest => "Nest by Part of".to_owned(),
        }
    }

    pub fn value_start(&self) -> usize {
        match self {
            Self::Tag(_) => "Tag is ".len(),
            Self::Kind(_) => "Item type is ".len(),
            Self::Text(_) => "Text contains ".len(),
            Self::Filter {
                field, operator, ..
            } => field.label().len() + operator.label().len() + 2,
            Self::Show(_) => "Show ".len(),
            Self::Sort { .. } => "Sort by ".len(),
            Self::Group(_) => "Group by ".len(),
            Self::Nest => "Nest by ".len(),
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Show(_) => 1,
            Self::Sort { .. } => 2,
            Self::Group(_) => 3,
            Self::Nest => 4,
            _ => 0,
        }
    }

    pub fn from_parts(field: Field, operator: Operator, value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() || !field.operators().contains(&operator) {
            return None;
        }
        let normalized = value
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        match (field, operator) {
            (Field::Tag, Operator::Is) => Some(Self::Tag(value.into())),
            (Field::Kind, Operator::Is) => Some(Self::Kind(value.into())),
            (Field::Text, Operator::Contains) => Some(Self::Text(value.into())),
            (Field::Sort, _) => ["name", "agent cost"]
                .into_iter()
                .zip([SortField::Name, SortField::Cost])
                .find_map(|(name, field)| {
                    let direction = normalized.strip_prefix(name)?.trim();
                    let descending = match direction {
                        "" | "asc" | "ascending" => false,
                        "desc" | "descending" => true,
                        _ => return None,
                    };
                    Some(Self::Sort { field, descending })
                }),
            (Field::Group, _) => match normalized.as_str() {
                "item type" | "type" => Some(Self::Group(GroupField::Kind)),
                "status" => Some(Self::Group(GroupField::Status)),
                _ => None,
            },
            (Field::Nest, _) => (normalized == "part of").then_some(Self::Nest),
            (Field::Cost, _) if value.parse::<u32>().is_err() => None,
            _ => Some(Self::Filter {
                field,
                operator,
                value: value.into(),
            }),
        }
    }
}

fn matches_text(item: &Item, query: &str) -> bool {
    let query = query.to_lowercase();
    item.name.to_lowercase().contains(&query) || item.id.to_lowercase().contains(&query)
}

fn matches_filter(item: &Item, field: Field, operator: Operator, value: &str) -> bool {
    if field == Field::Cost {
        return item.cost.zip(value.parse::<u32>().ok()).is_some_and(
            |(cost, value)| match operator {
                Operator::Greater => cost > value,
                Operator::Less => cost < value,
                Operator::AtLeast => cost >= value,
                Operator::AtMost => cost <= value,
                _ => cost == value,
            },
        );
    }
    let matches = match field {
        Field::Tag => item.tags.iter().any(|tag| tag.eq_ignore_ascii_case(value)),
        Field::Kind => item.kind.eq_ignore_ascii_case(value),
        Field::Status => item.status.eq_ignore_ascii_case(value),
        Field::Phase => item.phase.eq_ignore_ascii_case(value),
        Field::Text => matches_text(item, value),
        _ => return true,
    };
    matches ^ matches!(operator, Operator::IsNot | Operator::DoesNotContain)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub name: String,
    pub chips: Vec<Chip>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: "Design work".into(),
            chips: vec![
                Chip::Tag("Design".into()),
                Chip::Sort {
                    field: SortField::Name,
                    descending: false,
                },
                Chip::Group(GroupField::Kind),
            ],
        }
    }
}

impl Config {
    pub fn columns(&self) -> &[Column] {
        self.chips
            .iter()
            .find_map(|chip| match chip {
                Chip::Show(columns) => Some(columns.as_slice()),
                _ => None,
            })
            .unwrap_or(&Column::DEFAULT)
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        save_json(path, self)
    }

    pub fn apply(&mut self, original: Option<usize>, chip: Chip) -> usize {
        let mut index = original.unwrap_or(self.chips.len());
        if let Some(original) = original {
            self.chips.remove(original);
        }
        if chip.rank() > 0
            && let Some(existing) = self
                .chips
                .iter()
                .position(|other| other.rank() == chip.rank())
        {
            self.chips.remove(existing);
            if existing < index {
                index -= 1;
            }
        }
        self.chips.insert(index.min(self.chips.len()), chip.clone());
        self.chips.sort_by_key(Chip::rank);
        if chip.rank() > 0 {
            self.chips.iter().position(|other| *other == chip).unwrap()
        } else {
            index.min(
                self.chips
                    .iter()
                    .take_while(|other| other.rank() == 0)
                    .count()
                    - 1,
            )
        }
    }
}

pub(crate) fn ensure_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub(crate) fn save_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    ensure_parent(path)?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = std::path::PathBuf::from(temporary);
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(&temporary, path)
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum RowKey {
    Group(String),
    Item(Arc<str>),
}

impl RowKey {
    pub fn item(id: impl Into<Arc<str>>) -> Self {
        Self::Item(id.into())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    pub key: RowKey,
    pub name: String,
    pub item: Option<Item>,
    pub depth: usize,
    pub expandable: bool,
}

pub fn rows(items: &[Item], config: &Config, collapsed: &BTreeSet<RowKey>) -> Vec<Row> {
    let matches = |item: &&Item| {
        config.chips.iter().all(|chip| match chip {
            Chip::Tag(tag) => item.tags.iter().any(|name| name.eq_ignore_ascii_case(tag)),
            Chip::Kind(kind) => item.kind.eq_ignore_ascii_case(kind),
            Chip::Text(query) => matches_text(item, query),
            Chip::Filter {
                field,
                operator,
                value,
            } => matches_filter(item, *field, *operator, value),
            _ => true,
        })
    };
    let group_field = config.chips.iter().find_map(|chip| match chip {
        Chip::Group(field) => Some(*field),
        _ => None,
    });
    let (sort_field, descending) = config
        .chips
        .iter()
        .find_map(|chip| match chip {
            Chip::Sort { field, descending } => Some((*field, *descending)),
            _ => None,
        })
        .unwrap_or((SortField::Name, false));
    let nest = config.chips.contains(&Chip::Nest);
    let mut buckets = BTreeMap::<&str, Vec<&Item>>::new();
    for item in items.iter().filter(matches) {
        let group = match group_field {
            Some(GroupField::Kind) => item.kind,
            Some(GroupField::Status) => item.status,
            None => "",
        };
        buckets.entry(group).or_default().push(item);
    }

    let mut output = Vec::new();
    for (group, mut members) in buckets {
        members.sort_by(|a, b| {
            let by_name = a.name.cmp(&b.name);
            let order = match sort_field {
                SortField::Name => by_name,
                SortField::Cost => match (a.cost, b.cost) {
                    (Some(a), Some(b)) => a.cmp(&b),
                    (None, Some(_)) => return std::cmp::Ordering::Greater,
                    (Some(_), None) => return std::cmp::Ordering::Less,
                    _ => by_name,
                },
            };
            (if descending { order.reverse() } else { order }).then(by_name)
        });
        if group_field.is_some() {
            let key = RowKey::Group(group.to_owned());
            let open = !collapsed.contains(&key);
            output.push(Row {
                key,
                name: format!("{group} ({})", members.len()),
                item: None,
                depth: 0,
                expandable: true,
            });
            if !open {
                continue;
            }
        }
        let mut visited = BTreeSet::new();
        for item in members
            .iter()
            .filter(|item| {
                !nest
                    || !members
                        .iter()
                        .any(|parent| item.parent.as_ref() == Some(&parent.id))
            })
            .chain(&members)
        {
            append_row(
                *item,
                if nest { &members } else { &[] },
                0,
                true,
                collapsed,
                &mut visited,
                &mut output,
            );
        }
    }
    output
}

fn append_row(
    item: &Item,
    members: &[&Item],
    depth: usize,
    visible: bool,
    collapsed: &BTreeSet<RowKey>,
    visited: &mut BTreeSet<Arc<str>>,
    output: &mut Vec<Row>,
) {
    if !visited.insert(item.id.clone()) {
        return;
    }
    let key = RowKey::item(item.id.clone());
    let descendants: Vec<_> = members
        .iter()
        .filter(|child| child.parent.as_ref() == Some(&item.id))
        .collect();
    let expanded = visible && !collapsed.contains(&key);
    if visible {
        output.push(Row {
            key,
            name: item.name.to_string(),
            item: Some(item.clone()),
            depth,
            expandable: !descendants.is_empty(),
        });
    }
    for child in descendants {
        append_row(
            *child,
            members,
            depth + 1,
            expanded,
            collapsed,
            visited,
            output,
        );
    }
}

pub(super) fn visible_outline(rows: &[Row], collapsed: &BTreeSet<RowKey>) -> Vec<Row> {
    let mut hidden_below = None;
    rows.iter()
        .filter(|row| {
            if hidden_below.is_some_and(|depth| row.depth > depth) {
                return false;
            }
            hidden_below = collapsed.contains(&row.key).then_some(row.depth);
            true
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_and_arrangement_order() {
        let mut config = Config::default();
        config.apply(None, Chip::Nest);
        let index = config.apply(
            None,
            Chip::from_parts(Field::Sort, Operator::By, "Agent cost desc").unwrap(),
        );
        assert_eq!(index, 1);
        assert_eq!(
            config.chips.iter().map(Chip::label).collect::<Vec<_>>(),
            [
                "Tag is Design",
                "Sort by Agent cost descending",
                "Group by Item type",
                "Nest by Part of"
            ]
        );
    }

    #[test]
    fn spec_items_use_document_ids_and_nest_under_component() {
        let items = items();
        assert!(
            items
                .iter()
                .any(|item| item.id.as_ref() == "REQ-SPEC-01" && item.detail.is_some())
        );
        let config = Config {
            name: "Nested".into(),
            chips: vec![Chip::Nest],
        };
        let expanded = rows(&items, &config, &BTreeSet::new());
        assert!(
            expanded
                .iter()
                .any(|row| row.key == RowKey::item("REQ-COLLECTION-01") && row.depth == 1)
        );
        let collapsed = rows(&items, &config, &BTreeSet::from([RowKey::item(COMPONENT)]));
        assert!(
            !collapsed
                .iter()
                .any(|row| row.key == RowKey::item("REQ-COLLECTION-01"))
        );
    }
}
