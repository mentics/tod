//! What the activity panel lists for a node, and how the list is grouped:
//! agent sessions, gate checks and rounds of the phase's commands, in time
//! order, in one group per stay in a phase.
//!
//! A stay begins at a phase change recorded in the node's timeline
//! (`tod_store::node_timeline`). The stay before the first recorded change is
//! the state that change left; with no change recorded (a node older than the
//! timeline), everything is in the node's current state.

use tod_store::node_timeline::{KIND_CHECKS, KIND_GATE, KIND_PHASE, TimelineEntry};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemKind {
    /// An agent session (a conversation).
    Session,
    /// A gate check; its criteria are in the entry.
    Gate(TimelineEntry),
    /// A round of the phase's configured commands; each is in the entry.
    Commands(TimelineEntry),
}

/// One row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: Uuid,
    pub kind: ItemKind,
    pub label: String,
    /// When it was last active; for a session, when it was last updated (so a
    /// running one stays the latest).
    pub at: i64,
    /// When it began, which decides its stay.
    pub started: i64,
    /// A gate check that did not clear, or commands that did not all pass.
    pub failed: bool,
}

impl Item {
    pub fn entry(&self) -> Option<&TimelineEntry> {
        match &self.kind {
            ItemKind::Gate(e) | ItemKind::Commands(e) => Some(e),
            ItemKind::Session => None,
        }
    }
}

/// A stay in a phase and what happened in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// Stable across loads: when the stay began (`i64::MIN` for the one
    /// before any recorded change).
    pub key: i64,
    pub phase: String,
    /// The user put the node in this phase, rather than the lifecycle.
    pub by_user: bool,
    pub items: Vec<Item>,
}

impl Group {
    /// `Active`, `Active (user)`: the phase and, when the user chose it, who.
    pub fn title(&self) -> String {
        let mut name = self.phase.replace('_', " ");
        if let Some(first) = name.chars().next() {
            name = first.to_uppercase().collect::<String>() + &name[first.len_utf8()..];
        }
        if self.by_user { format!("{name} (user)") } else { name }
    }
}

/// The phase changes among the timeline's entries, oldest first.
fn changes(timeline: &[TimelineEntry]) -> Vec<&TimelineEntry> {
    let mut list: Vec<&TimelineEntry> = timeline.iter().filter(|e| e.kind == KIND_PHASE).collect();
    list.sort_by_key(|e| e.at);
    list
}

/// The list's rows for the node's timeline entries (the phase changes are not
/// rows: they are the groups).
pub fn entry_items(timeline: &[TimelineEntry]) -> Vec<Item> {
    timeline
        .iter()
        .filter_map(|e| {
            let kind = match e.kind.as_str() {
                KIND_GATE => ItemKind::Gate(e.clone()),
                KIND_CHECKS => ItemKind::Commands(e.clone()),
                _ => return None,
            };
            Some(Item { id: e.id, kind, label: e.title.clone(), at: e.at, started: e.at, failed: e.failed })
        })
        .collect()
}

/// Group `items` into stays, oldest first, each holding its items oldest
/// first. `current` is the node's state now.
pub fn build_groups(mut items: Vec<Item>, timeline: &[TimelineEntry], current: &str) -> Vec<Group> {
    items.sort_by_key(|i| (i.started, i.at));
    let changes = changes(timeline);
    let mut groups: Vec<Group> = Vec::with_capacity(changes.len() + 1);
    let first_phase = changes
        .first()
        .and_then(|c| c.phase_change())
        .map(|p| p.from)
        .unwrap_or_else(|| current.to_string());
    groups.push(Group { key: i64::MIN, phase: first_phase, by_user: false, items: Vec::new() });
    for change in &changes {
        groups.push(Group { key: change.at, phase: change.phase.clone(), by_user: change.by_user, items: Vec::new() });
    }
    for item in items {
        // The last stay that began at or before it.
        let ix = groups.iter().rposition(|g| g.key <= item.started).unwrap_or(0);
        groups[ix].items.push(item);
    }
    // The stay before the first change is only worth a header when something
    // happened in it.
    if groups.len() > 1 && groups[0].items.is_empty() {
        groups.remove(0);
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(at: i64) -> Item {
        Item { id: Uuid::new_v4(), kind: ItemKind::Session, label: "s".into(), at, started: at, failed: false }
    }

    fn change(node: Uuid, at: i64, from: &str, to: &str, by_user: bool) -> TimelineEntry {
        let mut e = TimelineEntry::new(node, KIND_PHASE, to, "x", false, format!("{{\"from\":\"{from}\"}}"));
        e.at = at;
        e.by_user = by_user;
        e
    }

    #[test]
    fn without_recorded_changes_everything_is_in_the_current_phase() {
        let groups = build_groups(vec![session(5), session(1)], &[], "active");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].title(), "Active");
        assert_eq!(groups[0].items.iter().map(|i| i.at).collect::<Vec<_>>(), vec![1, 5]);
    }

    #[test]
    fn stays_split_at_phase_changes_and_a_user_move_is_marked() {
        let node = Uuid::new_v4();
        let timeline = [change(node, 10, "planning", "active", false), change(node, 20, "active", "planning", true)];
        let groups = build_groups(vec![session(5), session(12), session(25)], &timeline, "planning");
        let titles: Vec<_> = groups.iter().map(Group::title).collect();
        assert_eq!(titles, vec!["Planning", "Active", "Planning (user)"]);
        assert_eq!(groups.iter().map(|g| g.items.len()).collect::<Vec<_>>(), vec![1, 1, 1]);
    }

    #[test]
    fn an_empty_stay_before_the_first_change_has_no_header() {
        let node = Uuid::new_v4();
        let timeline = [change(node, 10, "design", "planning", false)];
        let groups = build_groups(vec![session(12)], &timeline, "planning");
        assert_eq!(groups.iter().map(Group::title).collect::<Vec<_>>(), vec!["Planning"]);
    }

    #[test]
    fn a_pr_draft_phase_reads_as_words() {
        let g = Group { key: 0, phase: "pr_draft".into(), by_user: false, items: Vec::new() };
        assert_eq!(g.title(), "Pr draft");
    }
}
