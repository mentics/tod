use chrono::{DateTime, Utc};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Associated agent summary for row menus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentInfo {
    pub id: String,
    pub label: String,
    pub status: String,
    /// True when this config is inherited from an ancestor node.
    pub inherited: bool,
}

/// Open shell session summary for row menus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellInfo {
    pub id: String,
    pub label: String,
}

/// Task row for the task list / tree pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskItem {
    pub id: String,
    pub ticket_id: Option<String>,
    pub title: String,
    pub lifecycle: String,
    pub entity_path: PathBuf,
    pub tags: Vec<String>,
    pub agents: Vec<AgentInfo>,
    pub shells: Vec<ShellInfo>,
    pub interaction_timestamp: DateTime<Utc>,
    /// Stable display order from outline flatten (preserved when sort = Tree).
    pub tree_ordinal: usize,
    pub parent_id: Option<String>,
    pub depth: usize,
    pub collapsed: bool,
    pub is_work_node: bool,
    pub has_spec: bool,
    pub requirement_count: usize,
    pub constraint_count: usize,
    pub has_children: bool,
}

impl TaskItem {
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    pub fn sorted_tags(&self) -> Vec<String> {
        let mut tags = self.tags.clone();
        tags.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        tags
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    TreeOrder,
    InteractionTimestamp,
    Title,
    Lifecycle,
    TicketId,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::TreeOrder => "Tree",
            Self::InteractionTimestamp => "Recent",
            Self::Title => "Title",
            Self::Lifecycle => "Lifecycle",
            Self::TicketId => "Ticket",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::TreeOrder => Self::Title,
            Self::InteractionTimestamp => Self::Title,
            Self::Title => Self::Lifecycle,
            Self::Lifecycle => Self::TicketId,
            Self::TicketId => Self::TreeOrder,
        }
    }

    pub const ALL: [Self; 5] = [
        Self::TreeOrder,
        Self::InteractionTimestamp,
        Self::Title,
        Self::Lifecycle,
        Self::TicketId,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    pub fn toggle(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }

    pub fn arrow(self) -> &'static str {
        match self {
            Self::Asc => "↑",
            Self::Desc => "↓",
        }
    }
}

impl Default for SortDirection {
    fn default() -> Self {
        Self::Desc
    }
}

#[derive(Debug, Clone, Default)]
pub struct ListWorkingSet {
    pub sort_key: SortKey,
    pub sort_direction: SortDirection,
    pub tag_filter: Option<String>,
    pub selected_id: Option<String>,
    pub active_list_id: Option<String>,
}

impl ListWorkingSet {
    pub fn default_sort() -> Self {
        Self {
            sort_key: SortKey::TreeOrder,
            sort_direction: SortDirection::Asc,
            tag_filter: None,
            selected_id: None,
            active_list_id: None,
        }
    }

    pub fn initial_direction_for_key(key: SortKey) -> SortDirection {
        match key {
            SortKey::TreeOrder | SortKey::Title => SortDirection::Asc,
            SortKey::InteractionTimestamp => SortDirection::Desc,
            SortKey::Lifecycle => SortDirection::Desc,
            SortKey::TicketId => SortDirection::Desc,
        }
    }

    pub fn set_sort_key(&mut self, key: SortKey) {
        if self.sort_key != key {
            self.sort_key = key;
            self.sort_direction = Self::initial_direction_for_key(key);
        } else {
            self.sort_direction = self.sort_direction.toggle();
        }
    }
}

pub fn lifecycle_rank(lifecycle: &str) -> usize {
    match lifecycle {
        "proposed" => 0,
        "design" => 1,
        "planning" => 2,
        "ready" => 3,
        "active" => 4,
        "verifying" => 5,
        "review" => 6,
        "approved" => 7,
        "merged" => 8,
        "released" => 9,
        "learn" => 10,
        "done" => 11,
        _ => 99,
    }
}

/// Simple fuzzy match: query chars must appear in order in text (case-insensitive).
pub fn fuzzy_matches(query: &str, text: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    let t = text.to_lowercase();
    let mut qi = q.chars();
    let mut current = qi.next();
    for ch in t.chars() {
        if Some(ch) == current {
            current = qi.next();
            if current.is_none() {
                return true;
            }
        }
    }
    false
}

pub fn task_matches_search(task: &TaskItem, query: &str) -> bool {
    if query.trim().is_empty() {
        return true;
    }
    if fuzzy_matches(query, &task.title) {
        return true;
    }
    if let Some(ticket) = &task.ticket_id {
        if fuzzy_matches(query, ticket) {
            return true;
        }
    }
    if fuzzy_matches(query, &task.lifecycle) {
        return true;
    }
    task.tags.iter().any(|tag| fuzzy_matches(query, tag))
}

pub fn task_matches_tag_filter(task: &TaskItem, tag_filter: Option<&str>) -> bool {
    match tag_filter {
        None => true,
        Some(tag) => task.tags.iter().any(|t| t.eq_ignore_ascii_case(tag)),
    }
}

pub fn compare_tasks(a: &TaskItem, b: &TaskItem, key: SortKey, dir: SortDirection) -> Ordering {
    let ord = match key {
        SortKey::TreeOrder => a.tree_ordinal.cmp(&b.tree_ordinal),
        SortKey::InteractionTimestamp => a.interaction_timestamp.cmp(&b.interaction_timestamp),
        SortKey::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
        SortKey::Lifecycle => lifecycle_rank(&a.lifecycle).cmp(&lifecycle_rank(&b.lifecycle)),
        SortKey::TicketId => compare_ticket_id(a.ticket_id.as_deref(), b.ticket_id.as_deref()),
    };
    match dir {
        SortDirection::Asc => ord,
        SortDirection::Desc => ord.reverse(),
    }
}

fn compare_ticket_id(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn effective_parent_id(task: &TaskItem, visible_ids: &HashSet<&str>) -> Option<String> {
    match task.parent_id.as_deref() {
        None => None,
        Some(pid) if visible_ids.contains(pid) => Some(pid.to_string()),
        Some(_) => None,
    }
}

fn build_children_map(
    tasks: &[TaskItem],
    visible_ids: &HashSet<&str>,
) -> HashMap<Option<String>, Vec<usize>> {
    let mut children: HashMap<Option<String>, Vec<usize>> = HashMap::new();
    for (idx, task) in tasks.iter().enumerate() {
        children
            .entry(effective_parent_id(task, visible_ids))
            .or_default()
            .push(idx);
    }
    children
}

fn sort_sibling_groups(
    children: &mut HashMap<Option<String>, Vec<usize>>,
    tasks: &[TaskItem],
    key: SortKey,
    dir: SortDirection,
) {
    for indices in children.values_mut() {
        indices.sort_by(|&a, &b| compare_tasks(&tasks[a], &tasks[b], key, dir));
    }
}

fn flatten_sorted_tree(
    children: &HashMap<Option<String>, Vec<usize>>,
    tasks: &[TaskItem],
    parent_key: Option<&str>,
    out: &mut Vec<TaskItem>,
) {
    let key = parent_key.map(String::from);
    if let Some(indices) = children.get(&key) {
        for &idx in indices {
            let task = tasks[idx].clone();
            out.push(task.clone());
            if !task.collapsed {
                flatten_sorted_tree(children, tasks, Some(&task.id), out);
            }
        }
    }
}

pub fn filter_and_sort_tasks(
    tasks: &[TaskItem],
    search_query: &str,
    working_set: &ListWorkingSet,
) -> Vec<TaskItem> {
    let filtered: Vec<TaskItem> = tasks
        .iter()
        .filter(|t| {
            task_matches_tag_filter(t, working_set.tag_filter.as_deref())
                && task_matches_search(t, search_query)
        })
        .cloned()
        .collect();
    if working_set.sort_key == SortKey::TreeOrder {
        return filtered;
    }
    let visible_ids: HashSet<&str> = filtered.iter().map(|t| t.id.as_str()).collect();
    let mut children = build_children_map(&filtered, &visible_ids);
    sort_sibling_groups(
        &mut children,
        &filtered,
        working_set.sort_key,
        working_set.sort_direction,
    );
    let mut visible = Vec::new();
    flatten_sorted_tree(&children, &filtered, None, &mut visible);
    visible
}

pub fn selection_after_delete(
    visible_before: &[TaskItem],
    visible_after: &[TaskItem],
    selected_id: Option<&str>,
    deleted_id: &str,
) -> Option<String> {
    if visible_after.is_empty() {
        return None;
    }
    if let Some(id) = selected_id.filter(|id| *id != deleted_id) {
        if visible_after.iter().any(|t| t.id == id) {
            return Some(id.to_string());
        }
    }
    if let Some(del_ix) = visible_before.iter().position(|t| t.id == deleted_id) {
        let ix = del_ix.min(visible_after.len().saturating_sub(1));
        return Some(visible_after[ix].id.clone());
    }
    visible_after.first().map(|t| t.id.clone())
}

pub fn nearest_visible_id(
    tasks: &[TaskItem],
    search_query: &str,
    working_set: &ListWorkingSet,
    previous_id: &str,
) -> Option<String> {
    let visible = filter_and_sort_tasks(tasks, search_query, working_set);
    if visible.is_empty() {
        return None;
    }
    if visible.iter().any(|t| t.id == previous_id) {
        return Some(previous_id.to_string());
    }
    let all = filter_and_sort_tasks(
        tasks,
        "",
        &ListWorkingSet {
            tag_filter: None,
            sort_key: working_set.sort_key,
            sort_direction: working_set.sort_direction,
            selected_id: None,
            active_list_id: working_set.active_list_id.clone(),
        },
    );
    let prev_ix = match all.iter().position(|t| t.id == previous_id) {
        Some(ix) => ix,
        None => return visible.first().map(|t| t.id.clone()),
    };
    visible
        .iter()
        .min_by_key(|t| {
            all.iter()
                .position(|a| a.id == t.id)
                .map(|ix| ix.abs_diff(prev_ix))
                .unwrap_or(usize::MAX)
        })
        .map(|t| t.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str, title: &str, lifecycle: &str, tags: &[&str]) -> TaskItem {
        TaskItem {
            id: id.into(),
            ticket_id: None,
            title: title.into(),
            lifecycle: lifecycle.into(),
            entity_path: PathBuf::from(id),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            agents: Vec::new(),
            shells: Vec::new(),
            interaction_timestamp: Utc::now(),
            tree_ordinal: 0,
            parent_id: None,
            depth: 0,
            collapsed: false,
            is_work_node: !lifecycle.is_empty(),
            has_spec: false,
            requirement_count: 0,
            constraint_count: 0,
            has_children: false,
        }
    }

    #[test]
    fn fuzzy_match_subsequence() {
        assert!(fuzzy_matches("flt", "fleet persistence"));
        assert!(!fuzzy_matches("xyz", "fleet"));
    }

    #[test]
    fn tag_filter_and_search_stack() {
        let tasks = vec![
            sample("a", "Alpha", "ready", &["ui"]),
            sample("b", "Beta", "active", &["backend"]),
        ];
        let ws = ListWorkingSet {
            tag_filter: Some("ui".into()),
            ..Default::default()
        };
        let visible = filter_and_sort_tasks(&tasks, "alp", &ws);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "a");
    }

    #[test]
    fn title_sort_preserves_tree_hierarchy() {
        let mut parent = sample("parent-id", "Parent", "active", &[]);
        parent.depth = 0;
        let mut child_a = sample("child-a", "Alpha child", "active", &[]);
        child_a.depth = 1;
        child_a.parent_id = Some("parent-id".into());
        child_a.tree_ordinal = 1;
        let mut child_b = sample("child-b", "Beta child", "active", &[]);
        child_b.depth = 1;
        child_b.parent_id = Some("parent-id".into());
        child_b.tree_ordinal = 2;
        let mut root_other = sample("root-other", "Zeta root", "active", &[]);
        root_other.depth = 0;
        let ws = ListWorkingSet {
            sort_key: SortKey::Title,
            sort_direction: SortDirection::Asc,
            ..ListWorkingSet::default_sort()
        };
        let visible = filter_and_sort_tasks(&[parent, child_b, child_a, root_other], "", &ws);
        assert_eq!(visible.len(), 4);
        assert_eq!(visible[0].title, "Parent");
        assert_eq!(visible[1].title, "Alpha child");
        assert_eq!(visible[2].title, "Beta child");
        assert_eq!(visible[3].title, "Zeta root");
        assert_eq!(visible[1].depth, 1);
        assert_eq!(visible[1].parent_id.as_deref(), Some("parent-id"));
    }
}
