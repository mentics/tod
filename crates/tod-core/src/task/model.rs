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
    /// True for a managed ticket whose generator has a quick-accept
    /// destination configured — the Accept action is inert otherwise.
    pub accept_ready: bool,
    /// True for a copied-out (non-managed) node still linked to its
    /// data-source item.
    pub linked_copy: bool,
    /// True for a managed node that has at least one linked copy elsewhere.
    pub has_copies: bool,
    /// True when the node's ticket is done or canceled (shown struck through).
    pub ticket_terminal: bool,
    /// The node's linked pull requests whose state is known. The row's own
    /// cell, apart from the status icon, shows the earliest of them
    /// ([`TaskItem::earliest_pr_state`]); its hover text lists each.
    pub pull_requests: Vec<PrMark>,
    /// Pending decisions waiting on the user for this node (fed by the
    /// host via `TaskListView::set_attention`; zero until it calls in).
    pub needs_you_count: usize,
    /// When this node started waiting on the user, for "waiting longest"
    /// sort. `None` when `needs_you_count` is zero.
    pub waiting_since: Option<DateTime<Utc>>,
    /// The unified view's status label for this node (`unified::status_label`,
    /// e.g. `"verifying"` / `"verifying →"` / `"→ verifying"`), fed by the
    /// host via `TaskListView::set_status_overrides`; `None` in the existing
    /// Tasks view, which never calls it. Precomputed there (not read live
    /// per row) since building it touches `AgentRuns::runs_for_node`, a
    /// `fleet.read`.
    pub status_override: Option<String>,
    /// The chat panel is working on this node right now (fed by the host via
    /// `TaskListView::set_running_nodes`). Who is next for the node itself
    /// is `NodeStanding`, not this.
    pub chat_running: bool,
    /// A chat stopped on this node since the user last selected it.
    pub chat_finished: bool,
    /// A ticket's synced metadata (priority, state, dates, ...), which a
    /// generator's sort reads.
    pub metadata: Option<std::sync::Arc<serde_json::Value>>,
    /// The sort text of a generator node's config (`sort_query`); empty or
    /// absent means none.
    pub generator_sort: Option<String>,
    /// The node groups its children by lifecycle state: the tree shows a
    /// header row per state, see [`group_children_by_lifecycle`].
    pub groups_by_lifecycle: bool,
    /// Set on a fake header row (not a node, never selectable): the
    /// lifecycle state whose children follow it.
    pub group_header: Option<String>,
}

/// One linked pull request and the state last read for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrMark {
    /// `owner/repo#number`.
    pub label: String,
    pub state: tod_store::github::PullState,
}

impl TaskItem {
    /// The earliest state among the node's pull requests (draft, then open,
    /// merged, closed): the one that says whether any still needs attention.
    pub fn earliest_pr_state(&self) -> Option<tod_store::github::PullState> {
        use tod_store::github::PullState;
        self.pull_requests.iter().map(|pr| pr.state).min_by_key(|state| match state {
            PullState::Draft => 0,
            PullState::Open => 1,
            PullState::Merged => 2,
            PullState::Closed => 3,
        })
    }
}

impl TaskItem {
    pub fn sorted_tags(&self) -> Vec<String> {
        let mut tags = self.tags.clone();
        tags.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        tags
    }
}

/// The summary strip's four buckets of "who is next" (see `next_actor`).
/// A node that is done, or has no standing, is in none. Persisted by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorBucket {
    /// `You`, not deferred.
    OnMe,
    /// `Others`.
    OnOthers,
    /// `Working`.
    Running,
    /// `You`, and the user deferred it.
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    TreeOrder,
    InteractionTimestamp,
    Title,
    Lifecycle,
    TicketId,
    /// Nodes with `needs_you_count > 0` first, oldest `waiting_since` first;
    /// everything else keeps tree order after them.
    WaitingLongest,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::TreeOrder => "Tree",
            Self::InteractionTimestamp => "Recent",
            Self::Title => "Title",
            Self::Lifecycle => "Lifecycle",
            Self::TicketId => "Ticket",
            Self::WaitingLongest => "Waiting longest",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::TreeOrder => Self::Title,
            Self::InteractionTimestamp => Self::Title,
            Self::Title => Self::Lifecycle,
            Self::Lifecycle => Self::TicketId,
            Self::TicketId => Self::WaitingLongest,
            Self::WaitingLongest => Self::TreeOrder,
        }
    }

    pub const ALL: [Self; 6] = [
        Self::TreeOrder,
        Self::InteractionTimestamp,
        Self::Title,
        Self::Lifecycle,
        Self::TicketId,
        Self::WaitingLongest,
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
    /// The summary strip's filter: only the nodes in this bucket (and their
    /// ancestors). Persisted.
    pub actor_filter: Option<ActorBucket>,
    /// Group header rows (see [`group_children_by_lifecycle`]) the user has
    /// collapsed, by header id. Persisted.
    pub collapsed_groups: HashSet<String>,
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
            actor_filter: None,
            collapsed_groups: HashSet::new(),
        }
    }

    pub fn initial_direction_for_key(key: SortKey) -> SortDirection {
        match key {
            SortKey::TreeOrder | SortKey::Title => SortDirection::Asc,
            SortKey::InteractionTimestamp => SortDirection::Desc,
            SortKey::Lifecycle => SortDirection::Desc,
            SortKey::TicketId => SortDirection::Desc,
            // Ascending already puts the longest-waiting node first (see
            // `compare_tasks`); no need to reverse it up front.
            SortKey::WaitingLongest => SortDirection::Asc,
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
        "pr" => 7,
        "pr_review" => 8,
        "approved" => 9,
        "merged" => 10,
        "released" => 11,
        "learn" => 12,
        "done" => 13,
        _ => 99,
    }
}

/// Ordered lifecycle states, indexed by `lifecycle_rank`.
pub const LIFECYCLE_STATES: [&str; 14] = [
    "proposed",
    "design",
    "planning",
    "ready",
    "active",
    "verifying",
    "review",
    "pr",
    "pr_review",
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
/// gate-check turn should ever be fired for it. `approved` is also agent-less:
/// it is a thin holding state between the PR becoming mergeable (the
/// `pr_review` state's job) and the user clicking merge — both its gates are
/// app-checked against GitHub, not agent-decided.
pub fn state_has_agent(lifecycle: &str) -> bool {
    !matches!(lifecycle, "ready" | "done" | "approved")
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
    fn store_settings_accept_every_lifecycle_state() {
        assert_eq!(tod_store::settings::VALID_LIFECYCLE_STATES, LIFECYCLE_STATES);
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
        SortKey::TicketId => compare_ticket_id(ticket_of(a), ticket_of(b)),
        SortKey::WaitingLongest => {
            let a_waiting = a.needs_you_count > 0;
            let b_waiting = b.needs_you_count > 0;
            match b_waiting.cmp(&a_waiting) {
                Ordering::Equal if a_waiting => a.waiting_since.cmp(&b.waiting_since),
                Ordering::Equal => a.tree_ordinal.cmp(&b.tree_ordinal),
                other => other,
            }
        }
    };
    match dir {
        SortDirection::Asc => ord,
        SortDirection::Desc => ord.reverse(),
    }
}

/// The ticket a row is: a managed node's external id, else the ticket a
/// node was given.
fn ticket_of(task: &TaskItem) -> Option<&str> {
    task.external_id.as_deref().or(task.ticket_id.as_deref())
}

fn compare_ticket_id(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => tod_integration::linear_sort::compare_ticket_ids(a, b),
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

/// Ids of the nodes matching `matches` and every ancestor of one, so the
/// filtered tree keeps its context.
fn matching_with_ancestors<'a>(
    tasks: &'a [TaskItem],
    by_id: &HashMap<&str, &'a TaskItem>,
    matches: impl Fn(&TaskItem) -> bool,
) -> HashSet<&'a str> {
    let mut ids = HashSet::new();
    for task in tasks.iter().filter(|t| matches(t)) {
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

/// Ids of the nodes with a collapsed ancestor.
fn hidden_by_collapsed_ancestor<'a>(
    tasks: &'a [TaskItem],
    by_id: &HashMap<&str, &'a TaskItem>,
) -> HashSet<&'a str> {
    let mut hidden = HashSet::new();
    for task in tasks {
        let mut cur = task.parent_id.as_deref().and_then(|p| by_id.get(p).copied());
        while let Some(parent) = cur {
            if parent.collapsed {
                hidden.insert(task.id.as_str());
                break;
            }
            cur = parent.parent_id.as_deref().and_then(|p| by_id.get(p).copied());
        }
    }
    hidden
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

/// Whether any generator in `tasks` has a sort of its own configured.
fn any_generator_sort(tasks: &[TaskItem]) -> bool {
    tasks
        .iter()
        .any(|t| t.generator_sort.as_deref().is_some_and(|s| !s.trim().is_empty()))
}

fn sort_row(task: &TaskItem) -> tod_integration::linear_sort::SortRow<'_> {
    tod_integration::linear_sort::SortRow {
        external_id: ticket_of(task),
        title: &task.title,
        tags: &task.tags,
        metadata: task.metadata.as_deref(),
    }
}

fn sort_sibling_groups(
    children: &mut HashMap<Option<String>, Vec<usize>>,
    tasks: &[TaskItem],
    by_id: &HashMap<&str, &TaskItem>,
    working_set: &ListWorkingSet,
) {
    use tod_integration::linear_sort;
    let mut configured: HashMap<String, Vec<linear_sort::SortTerm>> = HashMap::new();
    for (parent_key, indices) in children.iter_mut() {
        let generator_id = parent_key
            .as_deref()
            .and_then(|pid| owning_generator_id(by_id, pid));
        let own = generator_id
            .as_deref()
            .and_then(|id| working_set.generator_sorts.get(id));
        // The sort the generator's config names, which the row's sort button
        // only flips: Asc keeps it as written, Desc runs every term backwards.
        let terms = generator_id.as_deref().and_then(|id| {
            let text = by_id.get(id)?.generator_sort.as_deref()?;
            let terms = configured
                .entry(id.to_string())
                .or_insert_with(|| linear_sort::parse(text).unwrap_or_default());
            (!terms.is_empty()).then(|| terms.clone())
        });
        if let Some(terms) = terms {
            let flipped = own.is_some_and(|o| o.sort_direction == SortDirection::Desc);
            indices.sort_by(|&a, &b| {
                linear_sort::compare(&terms, flipped, &sort_row(&tasks[a]), &sort_row(&tasks[b]))
            });
            continue;
        }
        let (key, dir) = own
            .map(|sort| (sort.sort_key, sort.sort_direction))
            .unwrap_or((working_set.sort_key, working_set.sort_direction));
        indices.sort_by(|&a, &b| compare_tasks(&tasks[a], &tasks[b], key, dir));
    }
}

fn flatten_sorted_tree(
    children: &HashMap<Option<String>, Vec<usize>>,
    tasks: &[TaskItem],
    parent_key: Option<&str>,
    searching: bool,
    out: &mut Vec<TaskItem>,
) {
    let key = parent_key.map(String::from);
    if let Some(indices) = children.get(&key) {
        for &idx in indices {
            let task = tasks[idx].clone();
            out.push(task.clone());
            if searching || !task.collapsed {
                flatten_sorted_tree(children, tasks, Some(&task.id), searching, out);
            }
        }
    }
}

pub fn filter_and_sort_tasks(
    tasks: &[TaskItem],
    search_query: &str,
    working_set: &ListWorkingSet,
) -> Vec<TaskItem> {
    group_children_by_lifecycle(
        filter_and_sort_flat(tasks, search_query, working_set),
        &working_set.collapsed_groups,
    )
}

/// The id of the header row for `state` under the node `parent_id`.
pub fn group_header_id(parent_id: &str, state: &str) -> String {
    format!("group:{parent_id}:{state}")
}

/// Present each node that groups its children by lifecycle state: its
/// children are ordered by state (the order within a state is kept), each run
/// under a header row for that state, indented one level. A header is a fake
/// row (`group_header` set, not a node, never selectable) made only while a
/// child is in that state, so no header is ever empty. `collapsed` holds
/// the ids of headers whose children are hidden.
pub fn group_children_by_lifecycle(
    visible: Vec<TaskItem>,
    collapsed: &HashSet<String>,
) -> Vec<TaskItem> {
    if !visible.iter().any(|t| t.groups_by_lifecycle) {
        return visible;
    }
    let mut out = Vec::with_capacity(visible.len() + 8);
    emit_grouped(&visible, 0, collapsed, &mut out);
    out
}

/// `rows` are sibling subtrees in tree order; `bump` is how many levels the
/// headers above them add to their depth.
fn emit_grouped(rows: &[TaskItem], bump: usize, collapsed: &HashSet<String>, out: &mut Vec<TaskItem>) {
    let mut idx = 0;
    while idx < rows.len() {
        let row = &rows[idx];
        let mut end = idx + 1;
        while end < rows.len() && rows[end].depth > row.depth {
            end += 1;
        }
        let mut shown = row.clone();
        shown.depth += bump;
        out.push(shown);
        let children = &rows[idx + 1..end];
        if row.groups_by_lifecycle && !children.is_empty() {
            let child_depth = children.iter().map(|c| c.depth).min().unwrap_or(row.depth + 1);
            // One block per child: the child and everything below it.
            let mut blocks: Vec<&[TaskItem]> = Vec::new();
            let mut start = 0;
            for at in 1..=children.len() {
                if at == children.len() || children[at].depth == child_depth {
                    blocks.push(&children[start..at]);
                    start = at;
                }
            }
            blocks.sort_by_key(|b| lifecycle_rank(&b[0].lifecycle));
            let mut run = 0;
            while run < blocks.len() {
                let state = blocks[run][0].lifecycle.clone();
                let mut run_end = run + 1;
                while run_end < blocks.len() && blocks[run_end][0].lifecycle == state {
                    run_end += 1;
                }
                if state.is_empty() {
                    // Cannot be grouped (a move to a valid place is pending).
                    for block in &blocks[run..run_end] {
                        emit_grouped(block, bump, collapsed, out);
                    }
                } else {
                    let mut header = group_header_row(row, &state, row.depth + bump + 1);
                    header.collapsed = collapsed.contains(&header.id);
                    let hide = header.collapsed;
                    out.push(header);
                    if !hide {
                        for block in &blocks[run..run_end] {
                            emit_grouped(block, bump + 1, collapsed, out);
                        }
                    }
                }
                run = run_end;
            }
        } else {
            emit_grouped(children, bump, collapsed, out);
        }
        idx = end;
    }
}

/// A header row under `parent`, made from it so every field has a value.
fn group_header_row(parent: &TaskItem, state: &str, depth: usize) -> TaskItem {
    let mut header = parent.clone();
    header.id = group_header_id(&parent.id, state);
    header.title = state.to_string();
    header.group_header = Some(state.to_string());
    header.groups_by_lifecycle = false;
    header.ticket_id = None;
    header.lifecycle = String::new();
    header.tags = Vec::new();
    header.parent_id = Some(parent.id.clone());
    header.depth = depth;
    header.collapsed = false;
    header.has_children = true;
    header.has_actions = false;
    header.has_files = false;
    header.has_spec = false;
    header.has_agent = false;
    header.is_work_node = false;
    header.live_run_count = 0;
    header.shells = Vec::new();
    header.requirement_count = 0;
    header.constraint_count = 0;
    header.incoming_count = 0;
    header.managed = false;
    header.external_id = None;
    header.source_type = None;
    header.managed_count = None;
    header.generator_status = None;
    header.generator_error = None;
    header.accept_ready = false;
    header.linked_copy = false;
    header.has_copies = false;
    header.ticket_terminal = false;
    header.pull_requests.clear();
    header.needs_you_count = 0;
    header.waiting_since = None;
    header.status_override = None;
    header.chat_running = false;
    header.chat_finished = false;
    header.metadata = None;
    header.generator_sort = None;
    header
}

fn filter_and_sort_flat(
    tasks: &[TaskItem],
    search_query: &str,
    working_set: &ListWorkingSet,
) -> Vec<TaskItem> {
    let by_id: HashMap<&str, &TaskItem> = tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    // Searching looks at every node, whether or not a collapsed ancestor hides
    // it in the tree; without a query the collapsed subtrees stay hidden.
    let searching = !search_query.trim().is_empty();
    let hidden = if searching {
        HashSet::new()
    } else {
        hidden_by_collapsed_ancestor(tasks, &by_id)
    };
    let pending = working_set
        .pending_changes_only
        .then(|| matching_with_ancestors(tasks, &by_id, |t| t.incoming_count > 0));
    let filtered: Vec<TaskItem> = tasks
        .iter()
        .filter(|t| {
            !hidden.contains(t.id.as_str())
                && pending
                    .as_ref()
                .is_none_or(|ids| ids.contains(t.id.as_str()))
                && task_matches_tag_filter(t, working_set.tag_filter.as_deref())
                && task_matches_search(t, search_query)
                && task_matches_generator_filter(t, &by_id, working_set)
        })
        .cloned()
        .collect();
    if working_set.sort_key == SortKey::TreeOrder
        && working_set.generator_sorts.is_empty()
        && !any_generator_sort(&filtered)
    {
        return filtered;
    }
    let visible_ids: HashSet<&str> = filtered.iter().map(|t| t.id.as_str()).collect();
    let filtered_by_id: HashMap<&str, &TaskItem> =
        filtered.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut children = build_children_map(&filtered, &visible_ids);
    sort_sibling_groups(&mut children, &filtered, &filtered_by_id, working_set);
    let mut visible = Vec::new();
    flatten_sorted_tree(&children, &filtered, None, searching, &mut visible);
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
        // A group header is not selectable: take the nearest node instead.
        let node = visible_after[ix..]
            .iter()
            .chain(visible_after[..ix].iter().rev())
            .find(|t| t.group_header.is_none());
        return node.map(|t| t.id.clone());
    }
    visible_after.iter().find(|t| t.group_header.is_none()).map(|t| t.id.clone())
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
            actor_filter: working_set.actor_filter,
            collapsed_groups: working_set.collapsed_groups.clone(),
        },
    );
    let prev_ix = match all.iter().position(|t| t.id == previous_id) {
        Some(ix) => ix,
        None => return visible.iter().find(|t| t.group_header.is_none()).map(|t| t.id.clone()),
    };
    visible
        .iter()
        .filter(|t| t.group_header.is_none())
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
            managed: false,
            external_id: None,
            source_type: None,
            managed_count: None,
            generator_status: None,
            generator_error: None,
            accept_ready: false,
            linked_copy: false,
            has_copies: false,
            ticket_terminal: false,
            pull_requests: Vec::new(),
            metadata: None,
            generator_sort: None,
            needs_you_count: 0,
            waiting_since: None,
            status_override: None,
            chat_running: false,
            chat_finished: false,
            groups_by_lifecycle: false,
            group_header: None,
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
    fn waiting_longest_sorts_needs_you_nodes_first_by_oldest_wait() {
        use chrono::Duration;
        let now = Utc::now();
        let mut a = sample("a", "A", "ready", &[]);
        a.needs_you_count = 1;
        a.waiting_since = Some(now - Duration::minutes(5));
        let mut b = sample("b", "B", "ready", &[]);
        b.needs_you_count = 1;
        b.waiting_since = Some(now - Duration::minutes(30));
        let c = sample("c", "C", "ready", &[]);
        let tasks = vec![a, b, c];
        let ws = ListWorkingSet {
            sort_key: SortKey::WaitingLongest,
            sort_direction: SortDirection::Asc,
            ..Default::default()
        };
        let ids: Vec<String> = filter_and_sort_tasks(&tasks, "", &ws)
            .into_iter()
            .map(|t| t.id)
            .collect();
        // "b" waited longer than "a"; "c" isn't waiting at all.
        assert_eq!(ids, vec!["b", "a", "c"]);
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
    fn search_reaches_nodes_under_collapsed_parents() {
        let mut generator = generator_sample("gen", "Generator");
        generator.collapsed = true;
        let managed = managed_sample("m1", "Fix login bug", "gen", 1);
        let tasks = vec![generator, managed];
        for ws in [ListWorkingSet::default_sort(), ListWorkingSet::default()] {
            let ids = |q: &str| -> Vec<String> {
                filter_and_sort_tasks(&tasks, q, &ws)
                    .into_iter()
                    .map(|t| t.id)
                    .collect()
            };
            assert_eq!(ids(""), vec!["gen"]);
            assert_eq!(ids("login"), vec!["m1"]);
        }
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

    fn ticket(id: &str, ext: &str, meta: serde_json::Value, ordinal: usize) -> TaskItem {
        let mut t = managed_sample(id, id, "gen", ordinal);
        t.external_id = Some(ext.into());
        t.metadata = Some(std::sync::Arc::new(meta));
        t
    }

    fn sorted_ids(tasks: &[TaskItem], ws: &ListWorkingSet) -> Vec<String> {
        filter_and_sort_tasks(tasks, "", ws).into_iter().map(|t| t.id).collect()
    }

    #[test]
    fn a_generators_configured_sort_orders_its_tickets_and_the_button_flips_it() {
        let mut generator = generator_sample("gen", "Generator");
        generator.generator_sort = Some("priority, ticket desc".into());
        let tasks = vec![
            generator,
            ticket("a", "TOD-9", serde_json::json!({"priority": "High"}), 1),
            ticket("b", "TOD-10", serde_json::json!({"priority": "High"}), 2),
            ticket("c", "TOD-2", serde_json::json!({"priority": "Urgent"}), 3),
            ticket("d", "TOD-1", serde_json::json!({"priority": null}), 4),
        ];
        let mut ws = ListWorkingSet::default_sort();
        assert_eq!(sorted_ids(&tasks, &ws), ["gen", "c", "b", "a", "d"]);
        ws.generator_sorts.insert(
            "gen".into(),
            GeneratorSubtreeSort {
                sort_direction: SortDirection::Desc,
                ..GeneratorSubtreeSort::default()
            },
        );
        // Every term runs backwards; the ticket with no priority stays last.
        assert_eq!(sorted_ids(&tasks, &ws), ["gen", "a", "b", "c", "d"]);
    }

    #[test]
    fn a_configured_sort_leaves_other_lists_and_sub_issues_alone() {
        let mut generator = generator_sample("gen", "Generator");
        generator.generator_sort = Some("ticket".into());
        let mut child = ticket("child", "TOD-1", serde_json::json!({}), 3);
        child.parent_id = Some("b".into());
        child.depth = 2;
        let mut other_child = ticket("child2", "TOD-0", serde_json::json!({}), 4);
        other_child.parent_id = Some("b".into());
        other_child.depth = 2;
        let tasks = vec![
            sample("z", "Zed", "ready", &[]),
            sample("y", "Why", "ready", &[]),
            generator,
            ticket("b", "TOD-20", serde_json::json!({}), 1),
            ticket("a", "TOD-3", serde_json::json!({}), 2),
            child,
            other_child,
        ];
        let ws = ListWorkingSet::default_sort();
        // Outside the generator the tree order stands; below it, tickets
        // sort at every level.
        assert_eq!(
            sorted_ids(&tasks, &ws),
            ["z", "y", "gen", "a", "b", "child2", "child"]
        );
    }

    #[test]
    fn ticket_sort_reads_managed_nodes_and_orders_by_number() {
        let mut a = managed_sample("a", "A", "gen", 1);
        a.external_id = Some("TOD-10".into());
        let mut b = managed_sample("b", "B", "gen", 2);
        b.external_id = Some("TOD-9".into());
        let ws = ListWorkingSet {
            sort_key: SortKey::TicketId,
            sort_direction: SortDirection::Asc,
            ..ListWorkingSet::default_sort()
        };
        assert_eq!(sorted_ids(&[generator_sample("gen", "G"), a, b], &ws), ["gen", "b", "a"]);
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

    #[test]
    fn lifecycle_groups_order_children_and_make_headers_only_for_present_states() {
        let mut parent = sample("p", "Parent", "ready", &[]);
        parent.groups_by_lifecycle = true;
        parent.has_children = true;
        let child = |id: &str, state: &str| {
            let mut c = sample(id, id, state, &[]);
            c.parent_id = Some("p".into());
            c.depth = 1;
            c
        };
        let mut grandchild = sample("g", "g", "active", &[]);
        grandchild.parent_id = Some("c1".into());
        grandchild.depth = 2;
        let tasks = vec![
            parent,
            child("c1", "active"),
            grandchild,
            child("c2", "design"),
            child("c3", "active"),
        ];
        let mut ws = ListWorkingSet::default_sort();
        let rows = filter_and_sort_tasks(&tasks, "", &ws);
        let shape: Vec<(String, usize)> = rows.iter().map(|t| (t.id.clone(), t.depth)).collect();
        let expect: Vec<(String, usize)> = [
            ("p", 0),
            ("group:p:design", 1),
            ("c2", 2),
            ("group:p:active", 1),
            ("c1", 2),
            ("g", 3),
            ("c3", 2),
        ]
        .iter()
        .map(|(id, d)| (id.to_string(), *d))
        .collect();
        assert_eq!(shape, expect);
        assert!(rows[1].group_header.as_deref() == Some("design") && rows[0].group_header.is_none());

        // Collapsing a header hides its children only.
        ws.collapsed_groups.insert("group:p:active".into());
        let ids: Vec<String> =
            filter_and_sort_tasks(&tasks, "", &ws).into_iter().map(|t| t.id).collect();
        assert_eq!(ids, ["p", "group:p:design", "c2", "group:p:active"]);

        // Deleting the only child in a state leaves no header for it.
        let without: Vec<TaskItem> = tasks.iter().filter(|t| t.id != "c2").cloned().collect();
        let ids: Vec<String> = filter_and_sort_tasks(&without, "", &ListWorkingSet::default_sort())
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, ["p", "group:p:active", "c1", "g", "c3"]);
    }
}
