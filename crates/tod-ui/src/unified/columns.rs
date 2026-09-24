//! The unified view's column model: plain Rust, no GPUI types.
//!
//! Column 1 (the node tree) is always shown and always pinned, but it is not
//! stored as a column here — this model only tracks columns 2 onward. See
//! `doc/ui/unified-view.md` ("Where a panel opens", "Singleton panels",
//! "Pinning") for the rules this implements.

/// What panel a column shows, and what it targets.
///
/// `Decisions` has no target: it is a singleton that always shows whichever
/// node's decisions are current (see `doc/ui/unified-view.md` "Singleton
/// panels").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Details(uuid::Uuid),
    Decisions,
    Obligations(uuid::Uuid),
    Plan(uuid::Uuid),
    Settings(uuid::Uuid),
    Transcript(uuid::Uuid),
}

impl PanelKind {
    /// Whether this panel kind may exist in at most one column at a time.
    /// Opening one that is already shown retargets and focuses it instead of
    /// opening a new column (`doc/ui/unified-view.md` "Singleton panels").
    pub fn is_singleton(&self) -> bool {
        matches!(self, PanelKind::Decisions)
    }

    /// Two panel kinds are "the same panel" for the singleton check: for a
    /// singleton kind, any instance matches regardless of target; for
    /// everything else, kind and target must match exactly.
    fn matches_open_request(&self, other: &PanelKind) -> bool {
        if self.is_singleton() && other.is_singleton() {
            return std::mem::discriminant(self) == std::mem::discriminant(other);
        }
        self == other
    }
}

#[derive(Debug, Clone)]
pub struct Column {
    pub panel: PanelKind,
    pub pinned: bool,
}

impl Column {
    fn new(panel: PanelKind) -> Self {
        Self {
            panel,
            pinned: false,
        }
    }
}

/// The column model for columns 2 onward. Index 0 here is "column 2" in the
/// user-facing numbering (column 1 is the node tree, hosted outside this
/// model).
#[derive(Debug, Clone, Default)]
pub struct ColumnModel {
    columns: Vec<Column>,
    /// Index into `columns` of the focused column, if any is focused at all.
    focused: Option<usize>,
}

/// How many columns 2+ can render at full width before folding into strips
/// (kept in one place so folding logic and tests agree).
pub const DEFAULT_VISIBLE_COLUMNS: usize = 4;

impl ColumnModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    pub fn len(&self) -> usize {
        self.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// The currently focused column's index (into `columns()`), if any.
    pub fn focused_index(&self) -> Option<usize> {
        self.focused
    }

    pub fn focused_panel(&self) -> Option<&PanelKind> {
        self.focused.and_then(|ix| self.columns.get(ix)).map(|c| &c.panel)
    }

    pub fn is_pinned(&self, index: usize) -> bool {
        self.columns.get(index).is_some_and(|c| c.pinned)
    }

    /// Open `panel`, following the placement rule in
    /// `doc/ui/unified-view.md`:
    ///
    /// - A singleton panel already shown anywhere is retargeted in place and
    ///   focused — the column rule below does not apply.
    /// - Otherwise: the first unpinned column starting at `from_column`
    ///   (inclusive, 0-based over `columns()`); with `ctrl`, starting
    ///   strictly after it. If none is found, a new column is appended.
    /// - Replacing a column's panel leaves every other column alone.
    ///
    /// `from_column` is a column-2+ index (0 = column 2). Passing an index at
    /// or past the end (e.g. from column 1, the node tree) behaves like
    /// "no column to start from": search begins at 0, or at the end when
    /// `ctrl` is set (i.e. append).
    pub fn open(&mut self, panel: PanelKind, from_column: usize, ctrl: bool) -> usize {
        if let Some(existing) = self
            .columns
            .iter()
            .position(|c| c.panel.matches_open_request(&panel))
        {
            self.columns[existing].panel = panel;
            self.focused = Some(existing);
            return existing;
        }

        let start = if ctrl {
            from_column.saturating_add(1)
        } else {
            from_column
        };

        let target = (start..self.columns.len()).find(|&ix| !self.columns[ix].pinned);

        match target {
            Some(ix) => {
                self.columns[ix].panel = panel;
                self.focused = Some(ix);
                ix
            }
            None => {
                self.columns.push(Column::new(panel));
                let ix = self.columns.len() - 1;
                self.focused = Some(ix);
                ix
            }
        }
    }

    /// Toggle the pinned flag of column `index`.
    pub fn toggle_pin(&mut self, index: usize) {
        if let Some(col) = self.columns.get_mut(index) {
            col.pinned = !col.pinned;
        }
    }

    pub fn set_pinned(&mut self, index: usize, pinned: bool) {
        if let Some(col) = self.columns.get_mut(index) {
            col.pinned = pinned;
        }
    }

    /// Toggle the pin of the focused column, if any.
    pub fn toggle_pin_focused(&mut self) {
        if let Some(ix) = self.focused {
            self.toggle_pin(ix);
        }
    }

    /// Close column `index`, unconditionally (pinned or not).
    pub fn close(&mut self, index: usize) {
        if index >= self.columns.len() {
            return;
        }
        self.columns.remove(index);
        self.focused = match self.focused {
            None => None,
            Some(f) if f == index => {
                if self.columns.is_empty() {
                    None
                } else {
                    Some(f.min(self.columns.len() - 1))
                }
            }
            Some(f) if f > index => Some(f - 1),
            Some(f) => Some(f),
        };
    }

    /// Move focus to `index` directly (e.g. a click on a column header).
    pub fn focus(&mut self, index: usize) {
        if index < self.columns.len() {
            self.focused = Some(index);
        }
    }

    /// Move focus one column left. Index 0 (column 2) moving left goes to
    /// the node tree (column 1), reported as `None` here — the caller (the
    /// view root) is what actually knows about column 1.
    pub fn focus_left(&mut self) {
        match self.focused {
            None => {
                if !self.columns.is_empty() {
                    self.focused = Some(self.columns.len() - 1);
                }
            }
            Some(0) => self.focused = None,
            Some(ix) => self.focused = Some(ix - 1),
        }
    }

    /// Move focus one column right. `None` (focus on the node tree) moves to
    /// column 2 (index 0) when there is one.
    pub fn focus_right(&mut self) {
        match self.focused {
            None => {
                if !self.columns.is_empty() {
                    self.focused = Some(0);
                }
            }
            Some(ix) if ix + 1 < self.columns.len() => self.focused = Some(ix + 1),
            Some(_) => {}
        }
    }

    /// Which columns (by index into `columns()`) fold into strips given
    /// `visible_slots` full-width slots are available. The oldest unpinned
    /// columns fold first; pinned columns never fold; if pinned columns
    /// alone exceed `visible_slots`, all unpinned columns fold and the
    /// pinned ones still render full width (folding pinned columns is not
    /// supported).
    ///
    /// "Oldest" means lowest index: columns are opened left to right, so a
    /// lower index is older.
    pub fn folded(&self, visible_slots: usize) -> Vec<usize> {
        let total = self.columns.len();
        if total <= visible_slots {
            return Vec::new();
        }
        let excess = total - visible_slots;
        let mut folded = Vec::new();
        for (ix, col) in self.columns.iter().enumerate() {
            if folded.len() >= excess {
                break;
            }
            if !col.pinned {
                folded.push(ix);
            }
        }
        folded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn details(n: u8) -> PanelKind {
        // Deterministic distinct uuids for readable test failures.
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        PanelKind::Details(Uuid::from_bytes(bytes))
    }

    fn obligations(n: u8) -> PanelKind {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        PanelKind::Obligations(Uuid::from_bytes(bytes))
    }

    #[test]
    fn open_on_empty_model_appends() {
        let mut m = ColumnModel::new();
        let ix = m.open(details(1), 0, false);
        assert_eq!(ix, 0);
        assert_eq!(m.len(), 1);
        assert_eq!(m.focused_index(), Some(0));
    }

    #[test]
    fn open_replaces_first_unpinned_from_clicked_column() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false); // column 2
        m.open(obligations(2), 1, false); // column 3
        // Click in column 2 (unpinned): replaces it.
        let ix = m.open(details(3), 0, false);
        assert_eq!(ix, 0);
        assert_eq!(m.columns()[0].panel, details(3));
        // Column 3 untouched.
        assert_eq!(m.columns()[1].panel, obligations(2));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn open_in_pinned_column_searches_right() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false); // col 2
        m.toggle_pin(0);
        m.open(obligations(2), 1, false); // col 3
        // Click in column 2 (pinned): opens in first unpinned to the right (col 3).
        let ix = m.open(details(3), 0, false);
        assert_eq!(ix, 1);
        assert_eq!(m.columns()[1].panel, details(3));
        assert_eq!(m.columns()[0].panel, details(1));
    }

    #[test]
    fn open_with_no_unpinned_column_appends() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false);
        m.toggle_pin(0);
        let ix = m.open(obligations(2), 0, false);
        assert_eq!(ix, 1);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn ctrl_click_opens_strictly_after_clicked_column_even_if_unpinned() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false); // col 2 (index 0), unpinned
        // Ctrl+click in column 2 itself: opens after it, not replacing it.
        let ix = m.open(obligations(2), 0, true);
        assert_eq!(ix, 1);
        assert_eq!(m.columns()[0].panel, details(1));
        assert_eq!(m.columns()[1].panel, obligations(2));
    }

    #[test]
    fn ctrl_click_skips_pinned_columns_after_clicked() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false); // col 2
        m.open(obligations(2), 1, false); // col 3
        m.toggle_pin(1); // pin col 3
        // Ctrl+click on col 2 -> first unpinned strictly after col 2 is col 4 (new).
        let ix = m.open(details(3), 0, true);
        assert_eq!(ix, 2);
        assert_eq!(m.len(), 3);
        assert_eq!(m.columns()[1].panel, obligations(2)); // untouched
    }

    #[test]
    fn replacing_a_column_leaves_columns_to_its_right_alone() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false); // col2
        m.open(obligations(2), 1, false); // col3, opened "from" col2's panel
        m.open(details(1), 2, false); // col4 (appended, since 2 is out of range -> append via ctrl-like search? see below)
        // Now replace column 2's content.
        m.open(obligations(9), 0, false);
        assert_eq!(m.columns()[1].panel, obligations(2));
    }

    #[test]
    fn singleton_reopen_retargets_existing_column_instead_of_opening_new() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false); // col2
        m.open(PanelKind::Decisions, 1, false); // col3
        assert_eq!(m.len(), 2);
        // Opening Decisions again from a different clicked column retargets
        // the existing one and focuses it, rather than opening a third column.
        let ix = m.open(PanelKind::Decisions, 0, false);
        assert_eq!(ix, 1);
        assert_eq!(m.len(), 2);
        assert_eq!(m.focused_index(), Some(1));
    }

    #[test]
    fn singleton_not_yet_shown_follows_the_normal_column_rule() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false);
        m.toggle_pin(0);
        let ix = m.open(PanelKind::Decisions, 0, false);
        assert_eq!(ix, 1);
    }

    #[test]
    fn non_singleton_reopen_with_same_target_replaces_in_place_and_focuses() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false); // col2
        m.open(obligations(2), 1, false); // col3
        m.focus(0);
        // Re-opening the exact same panel (Details for node 1) from column 3
        // finds it already shown and focuses it rather than duplicating it.
        let ix = m.open(details(1), 1, false);
        assert_eq!(ix, 0);
        assert_eq!(m.len(), 2);
        assert_eq!(m.focused_index(), Some(0));
    }

    #[test]
    fn toggle_pin_flips_state() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false);
        assert!(!m.is_pinned(0));
        m.toggle_pin(0);
        assert!(m.is_pinned(0));
        m.toggle_pin(0);
        assert!(!m.is_pinned(0));
    }

    #[test]
    fn toggle_pin_focused_affects_only_focused_column() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false);
        m.open(obligations(2), 1, false);
        m.focus(0);
        m.toggle_pin_focused();
        assert!(m.is_pinned(0));
        assert!(!m.is_pinned(1));
    }

    #[test]
    fn close_removes_column_and_updates_focus() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false); // 0
        m.open(obligations(2), 1, false); // 1
        m.open(details(3), 2, false); // 2
        m.focus(2);
        m.close(1);
        assert_eq!(m.len(), 2);
        assert_eq!(m.columns()[0].panel, details(1));
        assert_eq!(m.columns()[1].panel, details(3));
        // Focus was on 2, which shifted left to 1.
        assert_eq!(m.focused_index(), Some(1));
    }

    #[test]
    fn close_focused_column_moves_focus_to_a_neighbor() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false);
        m.open(obligations(2), 1, false);
        m.focus(1);
        m.close(1);
        assert_eq!(m.len(), 1);
        assert_eq!(m.focused_index(), Some(0));
    }

    #[test]
    fn close_last_column_clears_focus() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false);
        m.focus(0);
        m.close(0);
        assert_eq!(m.len(), 0);
        assert_eq!(m.focused_index(), None);
    }

    #[test]
    fn close_can_remove_a_pinned_column() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false);
        m.toggle_pin(0);
        m.close(0);
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn focus_left_and_right_move_across_columns() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false);
        m.open(obligations(2), 1, false);
        m.focus(0);
        m.focus_right();
        assert_eq!(m.focused_index(), Some(1));
        m.focus_right();
        assert_eq!(m.focused_index(), Some(1)); // no further column
        m.focus_left();
        assert_eq!(m.focused_index(), Some(0));
        m.focus_left();
        assert_eq!(m.focused_index(), None); // moved onto the node tree
    }

    #[test]
    fn focus_right_from_node_tree_enters_column_2() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false);
        m.focus_left(); // -> None (node tree), since it was focused at 0
        assert_eq!(m.focused_index(), None);
        m.focus_right();
        assert_eq!(m.focused_index(), Some(0));
    }

    #[test]
    fn folded_returns_empty_when_everything_fits() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false);
        m.open(obligations(2), 1, false);
        assert!(m.folded(4).is_empty());
    }

    #[test]
    fn folded_picks_oldest_unpinned_columns_first() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false); // 0, oldest
        m.open(obligations(2), 1, false); // 1
        m.open(details(3), 2, false); // 2
        m.open(obligations(4), 3, false); // 3
        m.open(details(5), 4, false); // 4, newest
        // 5 columns, 4 fit: the single oldest unpinned folds.
        let folded = m.folded(4);
        assert_eq!(folded, vec![0]);
    }

    #[test]
    fn folded_skips_pinned_columns() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false); // 0
        m.open(obligations(2), 1, false); // 1
        m.open(details(3), 2, false); // 2
        m.toggle_pin(0); // pin the oldest
        // 3 columns, 2 fit: oldest is pinned, so the next-oldest unpinned folds.
        let folded = m.folded(2);
        assert_eq!(folded, vec![1]);
    }

    #[test]
    fn folded_when_pinned_columns_alone_exceed_visible_slots() {
        let mut m = ColumnModel::new();
        m.open(details(1), 0, false);
        m.open(obligations(2), 1, false);
        m.toggle_pin(0);
        m.toggle_pin(1);
        // Both pinned, only 1 slot "fits": nothing unpinned to fold.
        let folded = m.folded(1);
        assert!(folded.is_empty());
    }
}
