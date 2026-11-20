//! What the activity panel lists for a node, and how the list is grouped:
//! agent sessions, gate checks and rounds of the phase's commands, in
//! order of last activity, in one group per stay in a phase. A session cut by
//! other activity is several rows ([`Piece`]).
//!
//! A stay begins at a phase change recorded in the node's timeline
//! (`tod_store::node_timeline`). The stay before the first recorded change is
//! the state that change left; with no change recorded (a node older than the
//! timeline), everything is in the node's current state.

use tod_store::conversation_usage::Spent;
use tod_store::node_timeline::{KIND_CHECKS, KIND_GATE, KIND_NOTE, KIND_PHASE, KIND_SEND_BACK, TimelineEntry};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemKind {
    /// An agent session (a conversation).
    Session,
    /// A gate check; its criteria are in the entry.
    Gate(TimelineEntry),
    /// A round of the phase's configured commands; each is in the entry.
    Commands(TimelineEntry),
    /// Something the app did by itself and says why (`KIND_NOTE`), or an
    /// agent's send-back (`KIND_SEND_BACK`); the entry's detail is the reason.
    Note(TimelineEntry),
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
    /// The phase it worked in, when it says (a session records the state it
    /// was started for). Only used where no phase change was recorded.
    pub phase: Option<String>,
    /// Set when the row shows only part of a session: the turns between two
    /// cuts (see [`cut_pieces`]).
    pub piece: Option<Piece>,
    /// What a session spent (a piece: the turns it shows), at list prices;
    /// empty for anything that is not a session and for a session whose
    /// agent reported no tokens.
    pub cost: Spent,
}

impl Item {
    pub fn entry(&self) -> Option<&TimelineEntry> {
        match &self.kind {
            ItemKind::Gate(e) | ItemKind::Commands(e) | ItemKind::Note(e) => Some(e),
            ItemKind::Session => None,
        }
    }
}

/// Part of a session that is a row of its own: a long-lived conversation (the
/// PR agent keeps one for a whole stay) is listed where its work happened, so
/// it is cut wherever something else in the activity happened between two of
/// its turns, and each piece is placed at its own time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub conversation: Uuid,
    /// The turns shown, by `seq`, inclusive.
    pub from_seq: i64,
    pub to_seq: i64,
    /// Which piece of how many, counting from 1.
    pub number: usize,
    pub of: usize,
    /// The session's last piece, where a streaming reply belongs.
    pub last: bool,
}

/// The row id of the piece of `conversation` that starts at `from_seq`:
/// stable across loads, and never the conversation's own id.
pub fn piece_id(conversation: Uuid, from_seq: i64) -> Uuid {
    let mut bytes = *conversation.as_bytes();
    for (b, s) in bytes[8..].iter_mut().zip((from_seq as u64).wrapping_add(0x9e37_79b9_7f4a_7c15).to_be_bytes()) {
        *b ^= s;
    }
    Uuid::from_bytes(bytes)
}

impl Item {
    /// The conversation a session row shows (the whole of it, or a piece).
    pub fn conversation(&self) -> Option<Uuid> {
        match (&self.kind, &self.piece) {
            (_, Some(p)) => Some(p.conversation),
            (ItemKind::Session, None) => Some(self.id),
            _ => None,
        }
    }
}

/// One run of a conversation's turns between cuts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub from_seq: i64,
    pub to_seq: i64,
    pub first_at: i64,
    pub last_at: i64,
}

/// Cut a conversation's turns (`(seq, created_at)`, in order) into pieces:
/// a cut falls between two consecutive turns when some time in `others` (the
/// time of anything else in the activity: another session's turn, a gate
/// check, a round of commands, a phase change) lies strictly between them.
/// With nothing in between, the whole conversation is one span.
pub fn cut_pieces(turns: &[(i64, i64)], others: &[i64]) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for &(seq, at) in turns {
        let cut = spans.last().is_none_or(|s| others.iter().any(|&x| x > s.last_at && x < at));
        if cut {
            spans.push(Span { from_seq: seq, to_seq: seq, first_at: at, last_at: at });
        } else if let Some(s) = spans.last_mut() {
            s.to_seq = seq;
            s.last_at = at;
        }
    }
    spans
}

/// The rows for one session: a single row when it is not cut, else one per
/// piece, each labelled `label · part N`. `updated_at` is when the session
/// was last active (it keeps a running session's last piece the latest).
pub fn session_items(
    id: Uuid,
    label: &str,
    phase: Option<String>,
    created_at: i64,
    updated_at: i64,
    turns: &[(i64, i64)],
    others: &[i64],
    costs: &[(i64, Option<u64>)],
) -> Vec<Item> {
    let spans = cut_pieces(turns, others);
    let whole_cost = {
        let mut spent = Spent::default();
        for &(_, micros) in costs {
            spent.add_turn(micros);
        }
        spent
    };
    let whole = |piece| Item {
        cost: whole_cost,
        id,
        kind: ItemKind::Session,
        label: label.to_string(),
        at: updated_at,
        started: created_at,
        failed: false,
        phase: phase.clone(),
        piece,
    };
    if spans.len() < 2 {
        return vec![whole(None)];
    }
    let of = spans.len();
    spans
        .iter()
        .enumerate()
        .map(|(ix, s)| {
            let last = ix + 1 == of;
            let mut cost = Spent::default();
            for &(at, micros) in costs {
                if piece_of(&spans, turns, at) == ix {
                    cost.add_turn(micros);
                }
            }
            Item {
                cost,
                id: piece_id(id, s.from_seq),
                kind: ItemKind::Session,
                label: format!("{label} \u{b7} part {}", ix + 1),
                at: if last { s.last_at.max(updated_at) } else { s.last_at },
                started: s.first_at,
                failed: false,
                phase: phase.clone(),
                piece: Some(Piece { conversation: id, from_seq: s.from_seq, to_seq: s.to_seq, number: ix + 1, of, last }),
            }
        })
        .collect()
}

/// Which span (by index) a turn's recorded usage belongs to. The usage is
/// recorded as the turn ends, a moment before the agent's reply is stored as
/// the turn's row, so it belongs to the first row stored at or after it (a
/// second of slack for the clocks); after every row it is the last span's.
fn piece_of(spans: &[Span], turns: &[(i64, i64)], usage_at: i64) -> usize {
    let seq = turns.iter().find(|&&(_, at)| at >= usage_at - 1000).or(turns.last()).map(|t| t.0);
    seq.and_then(|seq| spans.iter().position(|s| s.from_seq <= seq && seq <= s.to_seq))
        .unwrap_or(spans.len().saturating_sub(1))
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
    /// What the stay's sessions spent.
    pub fn cost(&self) -> Spent {
        let mut spent = Spent::default();
        for item in &self.items {
            spent.add(&item.cost);
        }
        spent
    }

    /// `Active`, `Active (user)`: the phase and, when the user chose it, who.
    pub fn title(&self) -> String {
        let name = tod_store::settings::lifecycle_display_name(&self.phase);
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
                KIND_NOTE | KIND_SEND_BACK => ItemKind::Note(e.clone()),
                _ => return None,
            };
            Some(Item {
                id: e.id,
                kind,
                label: e.title.clone(),
                at: e.at,
                started: e.at,
                failed: e.failed,
                phase: Some(e.phase.clone()),
                piece: None,
                cost: Spent::default(),
            })
        })
        .collect()
}

/// Group `items` into stays, oldest first, each holding its items oldest
/// first. `current` is the node's state now.
///
/// From the first recorded phase change on, a stay is what the timeline says.
/// Before it (or everywhere, for a node older than the timeline) nothing
/// recorded the changes, so a stay is a run of items that worked in the same
/// phase ([`Item::phase`]); one that does not say belongs with its
/// neighbours.
pub fn build_groups(mut items: Vec<Item>, timeline: &[TimelineEntry], current: &str) -> Vec<Group> {
    items.sort_by_key(|i| (i.at, i.started));
    let changes = changes(timeline);
    let first_change = changes.first().map(|c| c.at);
    let (recorded, legacy): (Vec<Item>, Vec<Item>) =
        items.into_iter().partition(|i| first_change.is_some_and(|at| i.started >= at));

    let mut groups: Vec<Group> = Vec::new();
    // The phase before the first recorded change, when nothing says otherwise.
    let fallback = changes.first().and_then(|c| c.phase_change()).map(|p| p.from).unwrap_or_else(|| current.to_string());
    let known: Vec<Option<&str>> = legacy.iter().map(|i| i.phase.as_deref()).collect();
    let first_known = known.iter().flatten().next().map(|p| p.to_string());
    let mut run_phase: Option<String> = None;
    for item in legacy {
        let phase = item
            .phase
            .clone()
            .or_else(|| run_phase.clone())
            .or_else(|| first_known.clone())
            .unwrap_or_else(|| fallback.clone());
        if run_phase.as_deref() != Some(phase.as_str()) {
            groups.push(Group { key: item.started, phase: phase.clone(), by_user: false, items: Vec::new() });
            run_phase = Some(phase);
        }
        groups.last_mut().expect("a run was started").items.push(item);
    }

    let first_recorded = groups.len();
    for change in &changes {
        groups.push(Group { key: change.at, phase: change.phase.clone(), by_user: change.by_user, items: Vec::new() });
    }
    for item in recorded {
        // The last recorded stay that began at or before it.
        let ix = groups[first_recorded..]
            .iter()
            .rposition(|g| g.key <= item.started)
            .map_or(first_recorded, |i| i + first_recorded);
        groups[ix].items.push(item);
    }
    if groups.is_empty() {
        groups.push(Group { key: i64::MIN, phase: fallback, by_user: false, items: Vec::new() });
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(at: i64) -> Item {
        Item { id: Uuid::new_v4(), kind: ItemKind::Session, label: "s".into(), at, started: at, failed: false, phase: None, piece: None, cost: Spent::default() }
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

    fn in_phase(at: i64, phase: &str) -> Item {
        Item { phase: Some(phase.into()), ..session(at) }
    }

    #[test]
    fn without_recorded_changes_stays_follow_the_phase_each_session_worked_in() {
        let items = vec![
            in_phase(1, "proposed"),
            in_phase(2, "proposed"),
            in_phase(3, "design"),
            session(4),
            in_phase(5, "planning"),
            in_phase(6, "design"),
            in_phase(7, "pr_draft"),
        ];
        let groups = build_groups(items, &[], "pr_draft");
        let shape: Vec<_> = groups.iter().map(|g| (g.title(), g.items.len())).collect();
        assert_eq!(
            shape,
            vec![("Proposed".into(), 2), ("Design".into(), 2), ("Planning".into(), 1), ("Design".into(), 1), ("PR draft".into(), 1)]
        );
    }

    #[test]
    fn recorded_stays_follow_inferred_ones() {
        let node = Uuid::new_v4();
        let timeline = [change(node, 10, "design", "planning", false)];
        let groups = build_groups(vec![in_phase(1, "proposed"), in_phase(2, "design"), session(12)], &timeline, "planning");
        let titles: Vec<_> = groups.iter().map(Group::title).collect();
        assert_eq!(titles, vec!["Proposed", "Design", "Planning"]);
    }

    #[test]
    fn a_conversation_is_cut_where_something_else_happened_between_its_turns() {
        let turns = [(1, 10), (2, 11), (3, 50), (4, 51), (5, 90)];
        // Nothing between: one span.
        assert_eq!(cut_pieces(&turns, &[]).len(), 1);
        // Something at 30 (between 11 and 50) and 70 (between 51 and 90).
        let spans = cut_pieces(&turns, &[30, 70, 200]);
        assert_eq!(
            spans,
            vec![
                Span { from_seq: 1, to_seq: 2, first_at: 10, last_at: 11 },
                Span { from_seq: 3, to_seq: 4, first_at: 50, last_at: 51 },
                Span { from_seq: 5, to_seq: 5, first_at: 90, last_at: 90 },
            ]
        );
        // A tie with a turn's own time is not between.
        assert_eq!(cut_pieces(&turns, &[11, 50]).len(), 1);
        assert!(cut_pieces(&[], &[1]).is_empty());
    }

    #[test]
    fn pieces_are_rows_placed_at_their_own_time_among_the_other_items() {
        let id = Uuid::new_v4();
        let turns = [(1, 10), (2, 11), (3, 50), (4, 51)];
        let mut items = session_items(id, "Pull request", None, 10, 60, &turns, &[30], &[]);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "Pull request \u{b7} part 1");
        assert_ne!(items[0].id, id);
        assert_ne!(items[0].id, items[1].id);
        assert_eq!((items[0].at, items[1].at), (11, 60));
        assert!(items[1].piece.as_ref().unwrap().last);
        assert_eq!(items[0].conversation(), Some(id));
        // Ordered by last update, with the other session in between.
        items.push(Item { at: 40, started: 30, ..session(40) });
        let groups = build_groups(items, &[], "pr_draft");
        let ats: Vec<_> = groups[0].items.iter().map(|i| i.at).collect();
        assert_eq!(ats, vec![11, 40, 60]);
    }

    #[test]
    fn a_session_row_shows_what_its_turns_cost_and_a_piece_only_its_own() {
        let id = Uuid::new_v4();
        // Turn rows (ms) at 100k, 110k | 500k, 510k; usage is recorded just
        // before each agent reply's row, and one turn's model was unpriced.
        let turns = [(1, 100_000), (2, 110_000), (3, 500_000), (4, 510_000)];
        let costs = [
            (99_990, Some(1_000_000)),
            (109_990, Some(2_000_000)),
            (499_990, Some(4_000_000)),
            (509_990, None),
        ];
        let items = session_items(id, "PR", None, 10, 600_000, &turns, &[300_000], &costs);
        assert_eq!(items.len(), 2);
        assert_eq!((items[0].cost.turns, items[0].cost.micros), (2, 3_000_000));
        assert_eq!((items[1].cost.turns, items[1].cost.micros, items[1].cost.unpriced_turns), (2, 4_000_000, 1));
        assert_eq!(items[1].cost.label().as_deref(), Some("$4.00+"));
        // Uncut: the whole of it.
        let whole = session_items(id, "PR", None, 10, 600_000, &turns, &[], &costs);
        assert_eq!((whole[0].cost.turns, whole[0].cost.micros), (4, 7_000_000));
        let groups = build_groups(items, &[], "pr_draft");
        assert_eq!(groups[0].cost().micros, 7_000_000);
        // No usage recorded (an older session): nothing shown.
        assert_eq!(session_items(id, "x", None, 1, 600_000, &turns, &[], &[])[0].cost.label(), None);
    }

    #[test]
    fn an_uncut_session_stays_one_row_with_its_own_id() {
        let id = Uuid::new_v4();
        let items = session_items(id, "Implement", None, 1, 20, &[(1, 5), (2, 6)], &[], &[]);
        assert_eq!(items.len(), 1);
        assert_eq!((items[0].id, items[0].at, items[0].started), (id, 20, 1));
        assert!(items[0].piece.is_none());
        // No turns loaded: still one row, ordered by last update.
        assert_eq!(session_items(id, "x", None, 1, 20, &[], &[5], &[]).len(), 1);
    }

    #[test]
    fn a_pr_draft_phase_reads_as_words() {
        for (phase, title) in [("pr", "PR"), ("pr_draft", "PR draft"), ("pr_review", "PR review")] {
            let g = Group { key: 0, phase: phase.into(), by_user: false, items: Vec::new() };
            assert_eq!(g.title(), title);
        }
    }
}
