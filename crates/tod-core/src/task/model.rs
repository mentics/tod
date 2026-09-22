use chrono::{DateTime, Utc};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

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
    /// Agent or Files resolves (on this node or an ancestor), so the Action
    /// chip shows.
    pub has_actions: bool,
    /// Files resolves (on this node or an ancestor).
    pub has_files: bool,
    /// Agent runs on this node that haven't ended.
    pub live_run_count: usize,
    pub shells: Vec<ShellInfo>,
    pub interaction_timestamp: DateTime<Utc>,
    /// Stable display order from outline flatten (preserved when sort = Tree).
    pub tree_ordinal: usize,
    pub parent_id: Option<String>,
    pub depth: usize,
    pub collapsed: bool,
    pub is_work_node: bool,
    pub has_spec: bool,
    /// Agent resolves (on this node or an ancestor).
    pub has_agent: bool,
    pub requirement_count: usize,
    pub constraint_count: usize,
    /// Pending incoming-change entries (`doc/conversation/incoming-changes.md` §6).
    pub incoming_count: usize,
    pub has_children: bool,
    /// Short status text when a gate-check or on-entry agent turn is
    /// currently running against this node (e.g. "Running gate check…").
    /// Sourced from `LifecyclePanelView::in_flight_activity` — these turns
    /// aren't recorded as agent runs, so without this they'd be invisible in
    /// the task list while running.
    pub in_flight_activity: Option<String>,
    /// True when this node was produced/is owned by a generator ancestor.
    pub managed: bool,
    /// The data-source external id, for managed nodes.
    pub external_id: Option<String>,
    /// The data-source type (e.g. "linear"), for managed nodes.
    pub source_type: Option<String>,
    /// Direct/nested managed node count, for nodes with the Generator capability.
    pub managed_count: Option<usize>,
    /// `last_refresh_status` ("in_progress" | "success" | "error"), for generator nodes.
    pub generator_status: Option<String>,
    /// `last_refresh_error`, for generator nodes whose last refresh failed.
    pub generator_error: Option<String>,
}

impl TaskItem {
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

/// Independent sort/filter state for a single generator node's managed subtree.
/// Operates only on already-fetched managed nodes — never touches the data-source query.
#[derive(Debug, Clone, PartialEq)]
pub struct GeneratorSubtreeSort {
    pub sort_key: SortKey,
    pub sort_direction: SortDirection,
    pub filter_query: String,
}

impl Default for GeneratorSubtreeSort {
    fn default() -> Self {
        Self {
            sort_key: SortKey::TreeOrder,
            sort_direction: SortDirection::Asc,
            filter_query: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ListWorkingSet {
    pub sort_key: SortKey,
    pub sort_direction: SortDirection,
    pub tag_filter: Option<String>,
    pub selected_id: Option<String>,
    pub active_list_id: Option<String>,
    /// Per-generator-node sort/filter overrides, keyed by the generator node's id.
    pub generator_sorts: HashMap<String, GeneratorSubtreeSort>,
    /// Show only nodes with pending incoming changes, plus their ancestors.
    pub pending_changes_only: bool,
}

impl ListWorkingSet {
    pub fn default_sort() -> Self {
        Self {
            sort_key: SortKey::TreeOrder,
            sort_direction: SortDirection::Asc,
            tag_filter: None,
            selected_id: None,
            active_list_id: None,
            generator_sorts: HashMap::new(),
            pending_changes_only: false,
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

/// Ordered lifecycle states, indexed by `lifecycle_rank`.
pub const LIFECYCLE_STATES: [&str; 12] = [
    "proposed",
    "design",
    "planning",
    "ready",
    "active",
    "verifying",
    "review",
    "approved",
    "merged",
    "released",
    "learn",
    "done",
];

/// The state one step ahead of `lifecycle` in `LIFECYCLE_STATES`, if any.
/// Returns `None` for the last state or an unrecognized one.
pub fn next_lifecycle(lifecycle: &str) -> Option<&'static str> {
    let rank = lifecycle_rank(lifecycle);
    LIFECYCLE_STATES.get(rank + 1).copied()
}

/// The state one step behind `lifecycle` in `LIFECYCLE_STATES`, if any.
/// Returns `None` for the first state or an unrecognized one.
pub fn previous_lifecycle(lifecycle: &str) -> Option<&'static str> {
    let rank = lifecycle_rank(lifecycle);
    if rank == 0 || rank >= LIFECYCLE_STATES.len() {
        return None;
    }
    LIFECYCLE_STATES.get(rank - 1).copied()
}

/// Whether `lifecycle` has a state agent at all (see `assets/process/agents/state/base.md`:
/// "States `ready` and `done` have no agent"). Gates whether an on-entry or
/// gate-check turn should ever be fired for it.
pub fn state_has_agent(lifecycle: &str) -> bool {
    !matches!(lifecycle, "ready" | "done")
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn next_lifecycle_advances_through_known_states() {
        assert_eq!(next_lifecycle("proposed"), Some("design"));
        assert_eq!(next_lifecycle("learn"), Some("done"));
    }

    #[test]
    fn next_lifecycle_none_at_end_or_unknown() {
        assert_eq!(next_lifecycle("done"), None);
        assert_eq!(next_lifecycle("bogus"), None);
    }

    #[test]
    fn previous_lifecycle_steps_back_through_known_states() {
        assert_eq!(previous_lifecycle("planning"), Some("design"));
        assert_eq!(previous_lifecycle("done"), Some("learn"));
    }

    #[test]
    fn previous_lifecycle_none_at_start_or_unknown() {
        assert_eq!(previous_lifecycle("proposed"), None);
        assert_eq!(previous_lifecycle("bogus"), None);
    }
}

/// Fuzzy match used by the node tree search box: delegates to the shared
/// implementation also used by `obligations list --search` and
/// `tod-cli nodes search`.
pub fn fuzzy_matches(query: &str, text: &str) -> bool {
    crate::fuzzy::fuzzy_matches(query, text)
}

pub fn task_matches_search(task: &TaskItem, query: &str) -> bool {
    // Each whitespace-separated term must match at least one field, but the
    // terms may land in different fields ("ENG-12 login" finds the ticket
    // ENG-12 titled "Fix login"). Managed nodes carry their ticket id as
    // `external_id`.
    query.split_whitespace().all(|term| {
        fuzzy_matches(term, &task.title)
            || task.ticket_id.as_deref().is_some_and(|t| fuzzy_matches(term, t))
            || task.external_id.as_deref().is_some_and(|t| fuzzy_matches(term, t))
            || fuzzy_matches(term, &task.lifecycle)
            || task.tags.iter().any(|tag| fuzzy_matches(term, tag))
    })
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

/// Walks up from `start_id` (inclusive) to find the nearest node with the Generator
/// capability, returning its id. `start_id` itself counts if it is a generator.
fn owning_generator_id(by_id: &HashMap<&str, &TaskItem>, start_id: &str) -> Option<String> {
    let mut current = *by_id.get(start_id)?;
    if current.managed_count.is_some() {
        return Some(current.id.clone());
    }
    while let Some(parent_id) = &current.parent_id {
        let parent = *by_id.get(parent_id.as_str())?;
        if parent.managed_count.is_some() {
            return Some(parent.id.clone());
        }
        current = parent;
    }
    None
}

/// Ids of the nodes with pending incoming changes and every ancestor of
/// one, so the filtered tree keeps its context.
fn pending_changes_with_ancestors<'a>(
    tasks: &'a [TaskItem],
    by_id: &HashMap<&str, &'a TaskItem>,
) -> HashSet<&'a str> {
    let mut ids = HashSet::new();
    for task in tasks.iter().filter(|t| t.incoming_count > 0) {
        let mut cur = Some(task);
        while let Some(t) = cur {
            if !ids.insert(t.id.as_str()) {
                break;
            }
            cur = t.parent_id.as_deref().and_then(|p| by_id.get(p).copied());
        }
    }
    ids
}

fn task_matches_generator_filter(
    task: &TaskItem,
    by_id: &HashMap<&str, &TaskItem>,
    working_set: &ListWorkingSet,
) -> bool {
    if working_set.generator_sorts.is_empty() || !task.managed {
        return true;
    }
    let Some(generator_id) = owning_generator_id(by_id, &task.id) else {
        return true;
    };
    let Some(sort) = working_set.generator_sorts.get(&generator_id) else {
        return true;
    };
    if sort.filter_query.trim().is_empty() {
        return true;
    }
    fuzzy_matches(&sort.filter_query, &task.title)
        || task
            .tags
            .iter()
            .any(|tag| fuzzy_matches(&sort.filter_query, tag))
}

fn sort_sibling_groups(
    children: &mut HashMap<Option<String>, Vec<usize>>,
    tasks: &[TaskItem],
    by_id: &HashMap<&str, &TaskItem>,
    working_set: &ListWorkingSet,
) {
    for (parent_key, indices) in children.iter_mut() {
        let (key, dir) = parent_key
            .as_deref()
            .and_then(|pid| owning_generator_id(by_id, pid))
            .and_then(|generator_id| working_set.generator_sorts.get(&generator_id))
            .map(|sort| (sort.sort_key, sort.sort_direction))
            .unwrap_or((working_set.sort_key, working_set.sort_direction));
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
    let by_id: HashMap<&str, &TaskItem> = tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    let pending = working_set
        .pending_changes_only
        .then(|| pending_changes_with_ancestors(tasks, &by_id));
    let filtered: Vec<TaskItem> = tasks
        .iter()
        .filter(|t| {
            pending
                .as_ref()
                .is_none_or(|ids| ids.contains(t.id.as_str()))
                && task_matches_tag_filter(t, working_set.tag_filter.as_deref())
                && task_matches_search(t, search_query)
                && task_matches_generator_filter(t, &by_id, working_set)
        })
        .cloned()
        .collect();
    if working_set.sort_key == SortKey::TreeOrder && working_set.generator_sorts.is_empty() {
        return filtered;
    }
    let visible_ids: HashSet<&str> = filtered.iter().map(|t| t.id.as_str()).collect();
    let filtered_by_id: HashMap<&str, &TaskItem> =
        filtered.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut children = build_children_map(&filtered, &visible_ids);
    sort_sibling_groups(&mut children, &filtered, &filtered_by_id, working_set);
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
            generator_sorts: working_set.generator_sorts.clone(),
            pending_changes_only: working_set.pending_changes_only,
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
            has_actions: false,
            has_files: false,
            live_run_count: 0,
            shells: Vec::new(),
            interaction_timestamp: Utc::now(),
            tree_ordinal: 0,
            parent_id: None,
            depth: 0,
            collapsed: false,
            is_work_node: !lifecycle.is_empty(),
            has_spec: false,
            has_agent: false,
            requirement_count: 0,
            constraint_count: 0,
            incoming_count: 0,
            has_children: false,
            in_flight_activity: None,
            managed: false,
            external_id: None,
            source_type: None,
            managed_count: None,
            generator_status: None,
            generator_error: None,
        }
    }

    #[test]
    fn fuzzy_match_subsequence() {
        assert!(fuzzy_matches("flt", "fleet persistence"));
        assert!(!fuzzy_matches("xyz", "fleet"));
    }

    #[test]
    fn pending_changes_filter_keeps_pending_nodes_and_their_ancestors() {
        let root = sample("root", "Root", "ready", &[]);
        let mut mid = sample("mid", "Mid", "ready", &[]);
        mid.parent_id = Some("root".into());
        let mut leaf = sample("leaf", "Leaf", "ready", &[]);
        leaf.parent_id = Some("mid".into());
        leaf.incoming_count = 2;
        let mut sibling = sample("sib", "Sibling", "ready", &[]);
        sibling.parent_id = Some("root".into());
        let other = sample("other", "Other", "ready", &[]);
        let tasks = vec![root, mid, leaf, sibling, other];
        let ws = ListWorkingSet {
            pending_changes_only: true,
            ..Default::default()
        };
        let ids: Vec<String> = filter_and_sort_tasks(&tasks, "", &ws)
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec!["root", "mid", "leaf"]);
        assert_eq!(filter_and_sort_tasks(&tasks, "", &ListWorkingSet::default()).len(), 5);
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
    fn main_tree_search_matches_managed_node_title() {
        let mut managed = managed_sample("m1", "Fix login bug", "gen", 1);
        managed.tags = vec!["backend".into()];
        let tasks = vec![generator_sample("gen", "Generator"), managed];
        let ws = ListWorkingSet::default_sort();
        let visible = filter_and_sort_tasks(&tasks, "login", &ws);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "m1");
    }

    #[test]
    fn main_tree_search_matches_ticket_ids_per_term() {
        let mut managed = managed_sample("m1", "Fix login bug", "gen", 1);
        managed.external_id = Some("ENG-123".into());
        let mut local = sample("l1", "Dark mode", "ready", &[]);
        local.ticket_id = Some("ENG-456".into());
        let tasks = vec![generator_sample("gen", "Generator"), managed, local];
        let ws = ListWorkingSet::default_sort();
        let ids = |q: &str| -> Vec<String> {
            filter_and_sort_tasks(&tasks, q, &ws)
                .into_iter()
                .map(|t| t.id)
                .collect()
        };
        assert_eq!(ids("ENG-123"), vec!["m1"]);
        assert_eq!(ids("eng-123 login"), vec!["m1"]);
        assert_eq!(ids("ENG-456 dark"), vec!["l1"]);
        assert!(ids("ENG-456 login").is_empty());
    }

    #[test]
    fn main_tree_tag_filter_includes_matching_managed_nodes() {
        let mut matching = managed_sample("m1", "Fix login bug", "gen", 1);
        matching.tags = vec!["backend".into()];
        let mut other = managed_sample("m2", "Add dark mode", "gen", 2);
        other.tags = vec!["ui".into()];
        let tasks = vec![generator_sample("gen", "Generator"), matching, other];
        let ws = ListWorkingSet {
            tag_filter: Some("backend".into()),
            ..ListWorkingSet::default_sort()
        };
        let visible = filter_and_sort_tasks(&tasks, "", &ws);
        let ids: Vec<&str> = visible.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"m1"));
        assert!(!ids.contains(&"m2"));
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

    fn managed_sample(id: &str, title: &str, parent_id: &str, tree_ordinal: usize) -> TaskItem {
        let mut t = sample(id, title, "", &[]);
        t.managed = true;
        t.parent_id = Some(parent_id.into());
        t.depth = 1;
        t.tree_ordinal = tree_ordinal;
        t
    }

    fn generator_sample(id: &str, title: &str) -> TaskItem {
        let mut t = sample(id, title, "", &[]);
        t.managed_count = Some(2);
        t
    }

    #[test]
    fn generator_local_sort_does_not_affect_main_tree_order() {
        let tasks = vec![
            generator_sample("gen", "Generator"),
            managed_sample("m2", "Zeta", "gen", 1),
            managed_sample("m1", "Alpha", "gen", 2),
        ];
        let mut ws = ListWorkingSet::default_sort();
        ws.generator_sorts.insert(
            "gen".into(),
            GeneratorSubtreeSort {
                sort_key: SortKey::Title,
                sort_direction: SortDirection::Asc,
                filter_query: String::new(),
            },
        );
        let visible = filter_and_sort_tasks(&tasks, "", &ws);
        assert_eq!(visible.len(), 3);
        assert_eq!(visible[0].id, "gen");
        assert_eq!(visible[1].title, "Alpha");
        assert_eq!(visible[2].title, "Zeta");
    }

    #[test]
    fn generator_local_filter_hides_non_matching_managed_nodes_only() {
        let tasks = vec![
            generator_sample("gen", "Generator"),
            managed_sample("m1", "Fix login bug", "gen", 1),
            managed_sample("m2", "Add dark mode", "gen", 2),
        ];
        let mut ws = ListWorkingSet::default_sort();
        ws.generator_sorts.insert(
            "gen".into(),
            GeneratorSubtreeSort {
                sort_key: SortKey::TreeOrder,
                sort_direction: SortDirection::Asc,
                filter_query: "login".into(),
            },
        );
        let visible = filter_and_sort_tasks(&tasks, "", &ws);
        let ids: Vec<&str> = visible.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["gen", "m1"]);
    }

    #[test]
    fn generator_local_sort_is_independent_across_generators() {
        let tasks = vec![
            generator_sample("gen-a", "Gen A"),
            managed_sample("a2", "Zeta", "gen-a", 1),
            managed_sample("a1", "Alpha", "gen-a", 2),
            generator_sample("gen-b", "Gen B"),
            managed_sample("b1", "One", "gen-b", 1),
            managed_sample("b2", "Two", "gen-b", 2),
        ];
        let mut ws = ListWorkingSet::default_sort();
        ws.generator_sorts.insert(
            "gen-a".into(),
            GeneratorSubtreeSort {
                sort_key: SortKey::Title,
                sort_direction: SortDirection::Asc,
                filter_query: String::new(),
            },
        );
        let visible = filter_and_sort_tasks(&tasks, "", &ws);
        let a_children: Vec<&str> = visible
            .iter()
            .filter(|t| t.parent_id.as_deref() == Some("gen-a"))
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(a_children, vec!["Alpha", "Zeta"]);
        let b_children: Vec<&str> = visible
            .iter()
            .filter(|t| t.parent_id.as_deref() == Some("gen-b"))
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(b_children, vec!["One", "Two"]);
    }
}
