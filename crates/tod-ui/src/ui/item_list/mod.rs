//! The item list: one list component for every list-shaped view in the app.
//!
//! A list of items that *happens* to group. Inside any group, however deeply
//! nested, there is one flat run of items and an item never owns another item
//! — that, not how deep the grouping goes, is what separates this from the
//! node tree, where nodes own nodes. See `doc/ui/item-list.md`.
//!
//! The component owns the cursor, the selection, the collapsed groups, the
//! scrolling, the group headings, and the key set
//! ([`keyboard::bind_item_list_keys`]). The caller owns what an item *is*: it
//! flattens its data into [`ItemListRow`]s and renders each item, and it
//! carries out the changes the keys ask for.
//!
//! The same item affords the same actions wherever it is shown, so a list
//! leaves a capability out only where the data cannot take it.

// The component's surface is written for every list in `doc/ui/item-list.md`'s
// migration, not only the one migrated so far.
#![allow(dead_code)]

pub mod drag;
pub mod keyboard;
pub mod row_menu;
pub mod search;

use std::collections::HashSet;
use std::rc::Rc;

use crate::ui::style;
use crate::views::rows::{RowAction, RowHost, RowOptions};
use gpui::{
    AnyElement, App, AppContext, Div, InteractiveElement, IntoElement, MouseButton, ParentElement,
    ScrollHandle, SharedString, Stateful, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::Scrollbar;
use gpui_component::{Sizable as _, h_flex};

pub use drag::{ItemDrag, ItemDropped};
pub use keyboard::{ItemListKeys, bind_item_list_keys, bind_single_line_commit};
pub use row_menu::RowMenu;

/// Height of a group heading, and the unit page/viewport maths goes by.
pub const GROUP_ROW_HEIGHT: gpui::Pixels = px(28.);

/// What the user did to the list itself. A host's action type converts from
/// it, so a view keeps one action queue for rows and list alike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemListEvent {
    /// Clicked a row; `row_ix` is where it was rendered.
    Select { row_ix: usize },
    /// Clicked a group heading's chevron.
    ToggleGroup { key: String },
    /// Clicked an item's selection checkbox.
    ToggleMark { row_ix: usize },
    /// Dropped a dragged row onto this list. Reported even when the row lands
    /// where it already was: whether that changes anything depends on the
    /// view's own ordinal space, which the component does not know.
    Drop(ItemDropped),
}

/// One column of a list that is a table.
///
/// A list declares its columns once, so every row and the header above them
/// agree on where a value starts without any row setting a width itself. A
/// column is for something *every* row has — a plan step's status, a
/// finding's severity. A value only some rows carry stays inside the content
/// column, where empty space costs nothing.
pub struct ColumnSpec {
    /// How a row asks for this column: [`ItemRowState::column`]. Owned rather
    /// than `&'static str` because a list's columns are not always known when
    /// it is written — the database view's are the query's.
    pub key: SharedString,
    /// Named in the header row, in capitals.
    pub label: SharedString,
    /// Fixed width, or `None` for the content column, which takes what the
    /// fixed ones leave. Exactly one column has `None`, and it is the one a
    /// group heading aligns to.
    pub width: Option<gpui::Pixels>,
}

impl ColumnSpec {
    /// A fixed-width column.
    pub fn fixed(
        key: impl Into<SharedString>,
        label: impl Into<SharedString>,
        width: gpui::Pixels,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            width: Some(width),
        }
    }

    /// The content column: what the row is actually about.
    pub fn content(key: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            width: None,
        }
    }
}

/// A group heading: a label over a run of items, never an item itself.
pub struct GroupSpec {
    pub key: String,
    /// Nesting of the *group*. Open-ended: a list groups by as many levels as
    /// it needs. Items carry no depth.
    pub depth: usize,
    pub label: SharedString,
    /// Shown after the label as `(n)`.
    pub count: Option<usize>,
    pub collapsed: bool,
    /// The name is being typed (a rename, or a group not created yet).
    pub editing: bool,
    /// A group that cannot be collapsed yet (one being created) has no
    /// chevron.
    pub chevron: bool,
    /// Buttons at the end of the heading, always visible.
    pub actions: Vec<RowAction>,
    /// Double-clicking the heading's label when it is under the cursor.
    pub on_rename: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
}

impl GroupSpec {
    pub fn new(key: impl Into<String>, depth: usize, label: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            depth,
            label: label.into(),
            count: None,
            collapsed: false,
            editing: false,
            chevron: true,
            actions: Vec::new(),
            on_rename: None,
        }
    }

    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    pub fn editing(mut self, editing: bool) -> Self {
        self.editing = editing;
        self
    }

    pub fn chevron(mut self, chevron: bool) -> Self {
        self.chevron = chevron;
        self
    }

    pub fn action(mut self, action: RowAction) -> Self {
        self.actions.push(action);
        self
    }

    pub fn on_rename(mut self, on_rename: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_rename = Some(Rc::new(on_rename));
        self
    }
}

/// One row: a group heading, or an item. Both carry the caller's own payload —
/// `T` for an item, `G` for a group — so a view reads a row's meaning off the
/// row instead of parsing its key back.
pub enum ItemListRow<T, G = ()> {
    Group { spec: GroupSpec, group: G },
    Item { key: String, item: T },
}

impl<T, G> ItemListRow<T, G> {
    pub fn item(key: impl Into<String>, item: T) -> Self {
        Self::Item {
            key: key.into(),
            item,
        }
    }

    pub fn group(spec: GroupSpec, group: G) -> Self {
        Self::Group { spec, group }
    }

    pub fn key(&self) -> &str {
        match self {
            Self::Group { spec, .. } => &spec.key,
            Self::Item { key, .. } => key,
        }
    }

    pub fn as_item(&self) -> Option<&T> {
        match self {
            Self::Item { item, .. } => Some(item),
            Self::Group { .. } => None,
        }
    }

    pub fn as_group(&self) -> Option<&GroupSpec> {
        match self {
            Self::Group { spec, .. } => Some(spec),
            Self::Item { .. } => None,
        }
    }

    /// The caller's payload for a group heading.
    pub fn group_payload(&self) -> Option<&G> {
        match self {
            Self::Group { group, .. } => Some(group),
            Self::Item { .. } => None,
        }
    }

    fn depth(&self) -> Option<usize> {
        self.as_group().map(|group| group.depth)
    }
}

impl<T> ItemListRow<T, ()> {
    /// A group heading with no payload, for a list that groups by one thing.
    pub fn heading(spec: GroupSpec) -> Self {
        Self::Group { spec, group: () }
    }
}

/// How an item row is being shown, for the caller's renderer.
pub struct ItemRowState<'a> {
    /// Where the row was rendered, reported back on select.
    pub row_ix: usize,
    pub key: &'a str,
    /// Under the cursor.
    pub highlighted: bool,
    /// In the selection (multi-select).
    pub marked: bool,
    /// Being edited.
    pub editing: bool,
    /// The list's columns, empty when it is not a table.
    pub columns: &'a [ColumnSpec],
    /// What this row affords, as the list declared it
    /// ([`ItemList::with_row_actions`]). Already in [`Self::row_options`],
    /// which is how a renderer passes it on.
    pub actions: &'a [RowAction],
    /// The component installed a right-click menu on this row, so the row's
    /// text must not add a Copy menu of its own.
    pub menu_hosted: bool,
}

impl ItemRowState<'_> {
    /// Wrap `el` as the named column's cell, so it lines up with the same
    /// column on every other row and with the header. A key the list does not
    /// declare leaves the element as it is.
    pub fn column<E: Styled>(&self, key: &str, el: E) -> E {
        column_cell(self.columns, key, el)
    }

    /// The row's options as the list already decided them: its actions, and
    /// whether the menu on it is the component's. A renderer fills in what
    /// only it knows on top --- `RowOptions { leading: .., ..state.row_options() }`.
    pub fn row_options(&self) -> RowOptions {
        RowOptions {
            actions: self.actions.to_vec(),
            menu_hosted: self.menu_hosted,
            ..RowOptions::default()
        }
    }
}

/// [`ItemRowState::column`], for a row that was handed the columns rather
/// than the whole state.
pub fn column_cell<E: Styled>(columns: &[ColumnSpec], key: &str, el: E) -> E {
    match columns.iter().find(|column| column.key.as_ref() == key) {
        Some(column) => style::list_cell(el, column.width),
        None => el,
    }
}

/// What [`ItemList::collapse_step`] did, so the caller knows whether to
/// rebuild its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollapseStep {
    /// The group under the cursor collapsed; rebuild the rows.
    Collapsed,
    /// The cursor moved to the enclosing group.
    MovedToParent,
    Nothing,
}

/// The list's own state: rows, cursor, selection, collapsed groups, scroll.
pub struct ItemList<T, G = ()> {
    rows: Vec<ItemListRow<T, G>>,
    cursor: Option<usize>,
    cursor_key: Option<String>,
    marked: HashSet<String>,
    collapsed: HashSet<String>,
    editing_key: Option<String>,
    /// The field a group's name is typed into, and the tag that scopes plain
    /// Enter to it.
    group_editor: Option<gpui::Entity<InputState>>,
    group_edit_tag: Option<&'static str>,
    scroll: ScrollHandle,
    /// The columns every row lines up in, empty when the list is not a table.
    columns: Vec<ColumnSpec>,
    /// Items carry a selection checkbox and answer Space.
    marking: bool,
    /// The list's name while a row of it is being dragged, set by
    /// [`Self::with_reorder`]. `None` in a list whose rows do not drag.
    drag_list: Option<SharedString>,
    /// Which of the places a row could land this list actually allows.
    drop_filter: Option<Rc<dyn Fn(&ItemDropped) -> bool>>,
    /// What an item affords: its hover buttons and its menu entries, declared
    /// once.
    row_actions: Option<Rc<dyn Fn(&T) -> Vec<RowAction>>>,
    /// An item's text, for the menu's Copy.
    row_text: Option<Rc<dyn Fn(&T) -> String>>,
}

impl<T, G> Default for ItemList<T, G> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, G> ItemList<T, G> {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            cursor: None,
            cursor_key: None,
            marked: HashSet::new(),
            collapsed: HashSet::new(),
            editing_key: None,
            group_editor: None,
            group_edit_tag: None,
            scroll: ScrollHandle::new(),
            columns: Vec::new(),
            marking: false,
            drag_list: None,
            drop_filter: None,
            row_actions: None,
            row_text: None,
        }
    }

    /// Where a group's name is typed, and the tag that scopes plain Enter to
    /// that field (see [`keyboard::bind_single_line_commit`]).
    pub fn with_group_editor(
        mut self,
        editor: gpui::Entity<InputState>,
        tag: &'static str,
    ) -> Self {
        self.group_editor = Some(editor);
        self.group_edit_tag = Some(tag);
        self
    }

    /// Make the list a table: its rows line up in these columns, and a header
    /// naming them sits above the rows and does not scroll away. A row asks
    /// for a column by key through [`ItemRowState::column`]; nothing else sets
    /// a width, which is what keeps the columns aligned.
    pub fn with_columns(mut self, columns: Vec<ColumnSpec>) -> Self {
        self.set_columns(columns);
        self
    }

    /// Replace the columns. A list whose columns are its *data* — the
    /// database view's are whatever the query returned — declares them again
    /// each time the data changes.
    pub fn set_columns(&mut self, columns: Vec<ColumnSpec>) {
        debug_assert!(
            columns.is_empty() || columns.iter().filter(|c| c.width.is_none()).count() == 1,
            "a list's columns need exactly one content column to take the rest"
        );
        self.columns = columns;
    }

    pub fn columns(&self) -> &[ColumnSpec] {
        &self.columns
    }

    /// Where a group heading's label starts: past the selection gutter and
    /// the fixed columns, so it lines up with the content beneath it.
    fn lead_width(&self) -> gpui::Pixels {
        let gap = if self.columns.is_empty() {
            px(0.)
        } else {
            style::space::INLINE
        };
        let mark = if self.marking {
            style::size::MARK_GUTTER
        } else {
            px(0.)
        };
        self.columns
            .iter()
            .take_while(|column| column.width.is_some())
            .filter_map(|column| column.width)
            .fold(mark, |total, width| total + width + gap)
    }

    /// Let the user select several items: each row carries a checkbox, and
    /// Space marks the row under the cursor.
    pub fn with_marking(mut self) -> Self {
        self.marking = true;
        self
    }

    /// Let the user drag a row to a new place. `list` names this list in the
    /// [`ItemDrag`] payload, so a drop target can tell a row of this list from
    /// a row of another one; the drop arrives as [`ItemListEvent::Drop`].
    ///
    /// The component owns the whole gesture — the payload, the preview chip
    /// that follows the pointer, which row is the target, the line showing
    /// where the row will land, and scrolling the list when the pointer
    /// reaches its edge. What it does *not* decide is what the drop means:
    /// it reports the neighbour the row landed ahead of, and the view turns
    /// that into its own mutation.
    ///
    /// A list that wants the preview chip to say what the row says declares
    /// [`Self::with_row_text`] as well.
    pub fn with_reorder(mut self, list: impl Into<SharedString>) -> Self {
        self.drag_list = Some(list.into());
        self
    }

    /// Which of the places a row could land this list allows. The component
    /// works out where a row *would* go; only the view knows whether going
    /// there means anything it can carry out — an obligation moves between
    /// sections, but not between kinds, because its kind is what it is rather
    /// than where it sits. A refused target takes no drop and shows no
    /// landing line.
    ///
    /// A list with no filter allows every target in it.
    pub fn with_drop_filter(mut self, filter: impl Fn(&ItemDropped) -> bool + 'static) -> Self {
        self.drop_filter = Some(Rc::new(filter));
        self
    }

    /// The caller's payload for the group heading keyed `key`, so a view can
    /// read a dropped-on group as its own type instead of parsing the key it
    /// made. [`ItemDropped::group`] names the headings; this resolves them.
    pub fn group_payload_for(&self, key: &str) -> Option<&G> {
        self.rows
            .iter()
            .find(|row| row.as_group().is_some_and(|group| group.key == key))
            .and_then(ItemListRow::group_payload)
    }

    /// The headings enclosing `row_ix`, outermost first. Each enclosing
    /// heading is the nearest one above with a smaller depth than the one
    /// found before it, which is what makes an item's group chain readable off
    /// a flat row list.
    ///
    /// `row_ix` may be one past the last row, which is the chain the end of
    /// the list is in.
    fn group_chain(&self, row_ix: usize) -> Vec<String> {
        self.enclosing(row_ix, None)
    }

    /// [`Self::group_chain`], but only the headings shallower than `below`.
    ///
    /// An item is enclosed by the nearest heading at any depth. A *heading* is
    /// not: the nearest heading above it is usually its own previous sibling,
    /// which encloses nothing of its. So a heading's chain has to be taken
    /// from strictly shallower than itself.
    fn enclosing(&self, row_ix: usize, below: Option<usize>) -> Vec<String> {
        let mut chain: Vec<String> = Vec::new();
        let mut depth = below;
        for row in self.rows[..row_ix.min(self.rows.len())].iter().rev() {
            let Some(group) = row.as_group() else {
                continue;
            };
            if depth.is_none_or(|found| group.depth < found) {
                depth = Some(group.depth);
                chain.push(group.key.clone());
                if group.depth == 0 {
                    break;
                }
            }
        }
        chain.reverse();
        chain
    }

    /// Where a row dropped on `row_ix` lands. `None` is the strip past the
    /// last row, which means the end of the list.
    ///
    /// Dropping on an item row puts the dragged row ahead of it; dropping on
    /// a heading puts it first under that heading, which is the only way to
    /// reach an empty group.
    fn drop_position(&self, row_ix: Option<usize>) -> (Vec<String>, Option<String>) {
        let Some(row_ix) = row_ix else {
            // The end of the list: past the last row, and so under whichever
            // headings that row was under.
            return (self.group_chain(self.rows.len()), None);
        };
        match &self.rows[row_ix] {
            ItemListRow::Item { key, .. } => (self.group_chain(row_ix), Some(key.clone())),
            ItemListRow::Group { spec, .. } => {
                let mut chain = self.enclosing(row_ix, Some(spec.depth));
                chain.push(spec.key.clone());
                // First under this heading: the next row, when that row is one
                // of its items rather than a nested heading or the next group.
                let before = self
                    .rows
                    .get(row_ix + 1)
                    .and_then(ItemListRow::as_item)
                    .and(self.rows.get(row_ix + 1).map(|row| row.key().to_string()));
                (chain, before)
            }
        }
    }

    /// What an item affords, declared once: the buttons the row shows while it
    /// is hovered, and the entries its right-click menu offers. An action the
    /// row has no room for is [`RowAction::menu_only`] and reaches the user
    /// through the menu alone.
    ///
    /// The component owns the menu --- the gesture, the anchoring, the chrome,
    /// moving the cursor onto the row that was clicked, and the standard Copy
    /// entry. A list that declares neither actions nor
    /// [text](Self::with_row_text) has no menu.
    pub fn with_row_actions(mut self, actions: impl Fn(&T) -> Vec<RowAction> + 'static) -> Self {
        self.row_actions = Some(Rc::new(actions));
        self
    }

    /// What an item's text is, for the menu's Copy when nothing is selected.
    /// The component cannot read it off the row: `T` is the caller's payload
    /// and the rendered row is an opaque element.
    pub fn with_row_text(mut self, text: impl Fn(&T) -> String + 'static) -> Self {
        self.row_text = Some(Rc::new(text));
        self
    }

    /// Whether item rows carry a right-click menu.
    pub fn has_row_menu(&self) -> bool {
        self.row_actions.is_some() || self.row_text.is_some()
    }

    /// The menu for one item, as the component will build it. `None` when the
    /// list declared nothing to put in one.
    fn row_menu(&self, item: &T) -> Option<RowMenu> {
        let menu = RowMenu {
            actions: self
                .row_actions
                .as_ref()
                .map(|actions| actions(item))
                .unwrap_or_default(),
            text: self.row_text.as_ref().map(|text| text(item)),
        };
        (!menu.is_empty()).then_some(menu)
    }

    // -- rows ------------------------------------------------------------

    /// Replace the rows, keeping the cursor on the same *key* rather than the
    /// same index, so a change that reorders rows does not move it. Falls back
    /// to the first row. Marks are held by key too and survive the swap: a row
    /// that is gone counts for nothing (see [`Self::marked_count`]), and one
    /// that a collapsed group merely hid is marked again when it comes back.
    pub fn set_rows(&mut self, rows: Vec<ItemListRow<T, G>>) {
        let previous = self.cursor;
        let ix = self
            .cursor_key
            .as_ref()
            .and_then(|key| rows.iter().position(|row| row.key() == key.as_str()))
            .or_else(|| (!rows.is_empty()).then_some(0));
        self.rows = rows;
        match ix {
            Some(ix) => {
                self.cursor = Some(ix);
                self.cursor_key = Some(self.rows[ix].key().to_string());
            }
            None => {
                self.cursor = None;
                self.cursor_key = None;
            }
        }
        if let Some(ix) = self.cursor {
            if previous != Some(ix) {
                self.scroll.scroll_to_item(ix);
            }
        }
    }

    pub fn rows(&self) -> &[ItemListRow<T, G>] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Item rows only, in order.
    pub fn items(&self) -> impl Iterator<Item = &T> {
        self.rows.iter().filter_map(ItemListRow::as_item)
    }

    // -- cursor ----------------------------------------------------------

    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn cursor_key(&self) -> Option<&str> {
        self.cursor_key.as_deref()
    }

    pub fn cursor_row(&self) -> Option<&ItemListRow<T, G>> {
        self.cursor.and_then(|ix| self.rows.get(ix))
    }

    /// The item under the cursor, when the cursor is on an item rather than a
    /// group heading.
    pub fn cursor_item(&self) -> Option<&T> {
        self.cursor_row().and_then(ItemListRow::as_item)
    }

    /// The group heading under the cursor, with the caller's payload.
    pub fn cursor_group(&self) -> Option<&G> {
        self.cursor_row().and_then(ItemListRow::group_payload)
    }

    /// Put the cursor on `key` when the next [`set_rows`](Self::set_rows)
    /// finds it — how a caller keeps the cursor on something it just created,
    /// renamed, or was asked to reveal.
    pub fn set_cursor_key(&mut self, key: Option<String>) {
        self.cursor_key = key;
    }

    /// Move the cursor to `row_ix`. `false` when it was already there.
    pub fn set_cursor(&mut self, row_ix: usize) -> bool {
        if self.cursor == Some(row_ix) || row_ix >= self.rows.len() {
            return false;
        }
        self.cursor = Some(row_ix);
        self.cursor_key = Some(self.rows[row_ix].key().to_string());
        self.scroll.scroll_to_item(row_ix);
        true
    }

    /// Move the cursor by `delta` rows, stopping at either end.
    pub fn move_cursor(&mut self, delta: i32) -> bool {
        if self.rows.is_empty() {
            return false;
        }
        let current = self.cursor.unwrap_or(0);
        let next = if delta < 0 {
            current.saturating_sub((-delta) as usize)
        } else {
            (current + delta as usize).min(self.rows.len() - 1)
        };
        self.set_cursor(next)
    }

    /// A page is as many rows as the viewport shows.
    pub fn page_rows(viewport_height: gpui::Pixels) -> usize {
        (viewport_height / GROUP_ROW_HEIGHT).floor().max(1.) as usize
    }

    pub fn cursor_home(&mut self) -> bool {
        if self.rows.is_empty() {
            return false;
        }
        let moved = self.set_cursor(0);
        self.scroll.scroll_to_top_of_item(0);
        moved
    }

    pub fn cursor_end(&mut self) -> bool {
        if self.rows.is_empty() {
            return false;
        }
        let last = self.rows.len() - 1;
        let moved = self.set_cursor(last);
        self.scroll.scroll_to_top_of_item(last);
        moved
    }

    pub fn scroll_to_cursor(&self) {
        if let Some(ix) = self.cursor {
            self.scroll.scroll_to_item(ix);
        }
    }

    // -- selection -------------------------------------------------------

    /// Add or remove the row under the cursor from the selection. Only items
    /// are selectable: a group heading is a label over a run, not a row an
    /// action can work on.
    pub fn toggle_mark(&mut self) {
        let Some(ix) = self.cursor else {
            return;
        };
        self.toggle_mark_at(ix);
    }

    /// The same for the row at `row_ix` (its checkbox).
    pub fn toggle_mark_at(&mut self, row_ix: usize) {
        let Some(row) = self.rows.get(row_ix) else {
            return;
        };
        if row.as_item().is_none() {
            return;
        }
        let key = row.key().to_string();
        if !self.marked.remove(&key) {
            self.marked.insert(key);
        }
    }

    /// How many of the rows *present* are marked. A mark is held by key, so a
    /// row a collapsed group hides keeps its mark and gets it back when the
    /// group opens; while it is hidden it counts for nothing, and neither does
    /// a mark on a row that is gone for good.
    pub fn marked_count(&self) -> usize {
        self.marked_rows().count()
    }

    fn marked_rows(&self) -> impl Iterator<Item = &str> {
        self.rows
            .iter()
            .filter(|row| row.as_item().is_some())
            .map(ItemListRow::key)
            .filter(|key| self.marked.contains(*key))
    }

    pub fn is_marked(&self, key: &str) -> bool {
        self.marked.contains(key)
    }

    /// The items an action works on, in row order: the marked ones, or the
    /// item under the cursor when nothing is marked, so an action works on
    /// "this one" without a marking step. Empty when the cursor is on a group
    /// heading and nothing is marked.
    pub fn selection(&self) -> Vec<&str> {
        let marked: Vec<&str> = self.marked_rows().collect();
        if !marked.is_empty() {
            return marked;
        }
        self.cursor_row()
            .filter(|row| row.as_item().is_some())
            .map(ItemListRow::key)
            .into_iter()
            .collect()
    }

    pub fn clear_marks(&mut self) {
        self.marked.clear();
    }

    // -- groups ----------------------------------------------------------

    pub fn is_collapsed(&self, key: &str) -> bool {
        self.collapsed.contains(key)
    }

    pub fn set_collapsed(&mut self, key: impl Into<String>, collapsed: bool) {
        let key = key.into();
        if collapsed {
            self.collapsed.insert(key);
        } else {
            self.collapsed.remove(&key);
        }
    }

    pub fn toggle_collapsed(&mut self, key: &str) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_string());
        }
    }

    pub fn expand_all(&mut self) {
        self.collapsed.clear();
    }

    /// Carry a group's collapsed state over to the key it now has (a rename).
    pub fn rekey_collapsed(&mut self, old: &str, new: impl Into<String>) {
        if self.collapsed.remove(old) {
            self.collapsed.insert(new.into());
        }
    }

    /// Left: collapse the group under the cursor, else move to the group that
    /// encloses the cursor.
    pub fn collapse_step(&mut self) -> CollapseStep {
        let Some(ix) = self.cursor else {
            return CollapseStep::Nothing;
        };
        match self.rows.get(ix) {
            Some(ItemListRow::Group { spec, .. }) if !self.collapsed.contains(&spec.key) => {
                let key = spec.key.clone();
                self.collapsed.insert(key);
                CollapseStep::Collapsed
            }
            Some(_) => {
                if self.move_cursor_to_parent(ix) {
                    CollapseStep::MovedToParent
                } else {
                    CollapseStep::Nothing
                }
            }
            None => CollapseStep::Nothing,
        }
    }

    /// Right: expand the collapsed group under the cursor. `true` when
    /// something changed and the rows need rebuilding.
    pub fn expand_step(&mut self) -> bool {
        let Some(ItemListRow::Group { spec, .. }) = self.cursor.and_then(|ix| self.rows.get(ix))
        else {
            return false;
        };
        self.collapsed.remove(&spec.key)
    }

    /// The group that encloses row `ix`: the nearest heading above it that is
    /// shallower than it (or any heading, for an item).
    pub fn parent_group_key(&self, ix: usize) -> Option<&str> {
        let own_depth = self.rows.get(ix)?.depth();
        self.rows[..ix].iter().rev().find_map(|row| {
            let group = row.as_group()?;
            match own_depth {
                Some(depth) if group.depth >= depth => None,
                _ => Some(group.key.as_str()),
            }
        })
    }

    fn move_cursor_to_parent(&mut self, ix: usize) -> bool {
        let Some(key) = self.parent_group_key(ix).map(str::to_string) else {
            return false;
        };
        let Some(parent_ix) = self.rows.iter().position(|row| row.key() == key.as_str()) else {
            return false;
        };
        self.set_cursor(parent_ix)
    }

    // -- editing ---------------------------------------------------------

    /// The row whose text is being edited, if any.
    pub fn editing_key(&self) -> Option<&str> {
        self.editing_key.as_deref()
    }

    pub fn set_editing_key(&mut self, key: Option<String>) {
        self.editing_key = key;
    }

    pub fn is_editing(&self) -> bool {
        self.editing_key.is_some()
    }

    // -- rendering -------------------------------------------------------

    /// The scrolling list: every row, plus the scrollbar.
    ///
    /// `render_item` draws one item; the component wraps it with the row's id,
    /// its drag and its context menu, and draws the group headings itself.
    pub fn render<A, F>(
        &self,
        id: &'static str,
        host: &RowHost<A>,
        render_item: F,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement
    where
        A: From<ItemListEvent> + 'static,
        F: Fn(&T, ItemRowState<'_>, &mut Window, &mut App) -> AnyElement,
    {
        let mut elements = Vec::with_capacity(self.rows.len());
        for (row_ix, row) in self.rows.iter().enumerate() {
            let highlighted = self.cursor == Some(row_ix);
            let content = match row {
                ItemListRow::Group { spec, .. } => {
                    self.render_group(spec, row_ix, highlighted, host)
                }
                ItemListRow::Item { key, item } => {
                    let marked = self.marked.contains(key);
                    let actions = self
                        .row_actions
                        .as_ref()
                        .map(|actions| actions(item))
                        .unwrap_or_default();
                    let state = ItemRowState {
                        row_ix,
                        key,
                        highlighted,
                        marked,
                        editing: self.editing_key.as_deref() == Some(key.as_str()),
                        columns: &self.columns,
                        actions: &actions,
                        menu_hosted: self.has_row_menu(),
                    };
                    let row = render_item(item, state, window, cx);
                    if self.marking {
                        self.with_checkbox(row, row_ix, marked, host)
                    } else {
                        row
                    }
                }
            };
            let mut wrapper = div().id(("item-list-row", row_ix)).w_full().child(content);
            let mut menu = None;
            if let Some(item) = row.as_item() {
                menu = self.row_menu(item);
            }
            if self.drag_list.is_some() {
                wrapper = self.with_row_drag(wrapper, row_ix, host);
            }
            match menu {
                Some(menu) => {
                    // Right-clicking a row moves the cursor to it, as it does
                    // in any list; the menu then acts on what is under it.
                    let select_host = host.clone();
                    wrapper = wrapper.on_mouse_down(MouseButton::Right, move |_, _, cx| {
                        select_host.push(ItemListEvent::Select { row_ix }.into(), cx);
                    });
                    elements.push(row_menu::with_row_menu(wrapper, menu).into_any_element());
                }
                None => elements.push(wrapper.into_any_element()),
            }
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .relative()
            .children(self.render_header())
            .child(
                div()
                    .id(id)
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .when_some(self.autoscroll(), |el, autoscroll| {
                        el.on_drag_move::<ItemDrag>(autoscroll)
                    })
                    .children(elements)
                    .children(self.render_drop_strip(host)),
            )
            .child(
                // Narrow right-edge strip, not the full row area: the
                // Scrollbar element installs a click-to-jump handler across
                // its entire bounds, which would otherwise swallow every
                // mouse click meant for the rows below.
                div()
                    .occlude()
                    .absolute()
                    .top(if self.columns.is_empty() {
                        px(0.)
                    } else {
                        style::size::GROUP_ROW
                    })
                    .right_0()
                    .bottom_0()
                    .w(px(16.))
                    .child(Scrollbar::vertical(&self.scroll)),
            )
            .into_any_element()
    }

    /// The drag gesture on one row: the payload when the row is an item, and
    /// the drop target every row is — a heading included, since dropping on
    /// one is the only way to reach a group with nothing in it yet.
    fn with_row_drag<A>(
        &self,
        wrapper: Stateful<Div>,
        row_ix: usize,
        host: &RowHost<A>,
    ) -> Stateful<Div>
    where
        A: From<ItemListEvent> + 'static,
    {
        let Some(list) = self.drag_list.clone() else {
            return wrapper;
        };
        let row = &self.rows[row_ix];
        let row_key = row.key().to_string();
        let mut wrapper = wrapper;
        if let Some(item) = row.as_item() {
            let label: SharedString = self
                .row_text
                .as_ref()
                .map(|text| text(item))
                .unwrap_or_else(|| row_key.clone())
                .into();
            wrapper = wrapper.on_drag(
                ItemDrag {
                    list: list.clone(),
                    key: row_key.clone(),
                    group: self.group_chain(row_ix),
                    label,
                },
                |drag, _offset, _window, cx| {
                    let label = drag.label.clone();
                    cx.new(|_| drag::ItemDragPreview { label })
                },
            );
        }
        // A heading for a group that is not created yet (no chevron, because
        // there is nothing to collapse) has nowhere to put a row.
        if row.as_group().is_some_and(|group| !group.chevron) {
            return wrapper;
        }
        let (group, before) = self.drop_position(Some(row_ix));
        let takes = self.accepts(group.clone(), before.clone(), list.clone(), Some(row_key));
        wrapper
            .can_drop({
                let takes = takes.clone();
                move |any, _, _| any.downcast_ref::<ItemDrag>().is_some_and(&*takes)
            })
            .drag_over::<ItemDrag>({
                let takes = takes.clone();
                move |style, drag, _, _| {
                    if takes(drag) {
                        style::list_drop_indicator(style)
                    } else {
                        style.cursor_not_allowed()
                    }
                }
            })
            .on_drop::<ItemDrag>(self.report_drop(group, before, host))
    }

    /// Whether this target takes the row being dragged: a row of this same
    /// list, never the row itself (dropping a row where it already is moves
    /// nothing), and a place [the list allows](Self::with_drop_filter).
    fn accepts(
        &self,
        group: Vec<String>,
        before: Option<String>,
        list: SharedString,
        own_key: Option<String>,
    ) -> Rc<dyn Fn(&ItemDrag) -> bool> {
        let filter = self.drop_filter.clone();
        Rc::new(move |drag: &ItemDrag| {
            if drag.list != list || own_key.as_deref() == Some(drag.key.as_str()) {
                return false;
            }
            let Some(filter) = &filter else { return true };
            filter(&ItemDropped {
                from: drag.clone(),
                group: group.clone(),
                before: before.clone(),
            })
        })
    }

    /// Report the drop this target is, for the view to carry out.
    fn report_drop<A>(
        &self,
        group: Vec<String>,
        before: Option<String>,
        host: &RowHost<A>,
    ) -> impl Fn(&ItemDrag, &mut Window, &mut App) + 'static
    where
        A: From<ItemListEvent> + 'static,
    {
        let host = host.clone();
        move |drag: &ItemDrag, _: &mut Window, cx: &mut App| {
            host.push(
                ItemListEvent::Drop(ItemDropped {
                    from: drag.clone(),
                    group: group.clone(),
                    before: before.clone(),
                })
                .into(),
                cx,
            );
        }
    }

    /// The strip past the last row: how a list that drags says "the end".
    /// Without it the last position would be the one place a row could not be
    /// dropped, since every other target is a row that the drop goes *ahead*
    /// of.
    fn render_drop_strip<A>(&self, host: &RowHost<A>) -> Option<AnyElement>
    where
        A: From<ItemListEvent> + 'static,
    {
        let list = self.drag_list.clone()?;
        let (group, before) = self.drop_position(None);
        let takes = self.accepts(group.clone(), before.clone(), list, None);
        Some(
            div()
                .id("item-list-drop-strip")
                .w_full()
                .h(style::size::DROP_STRIP)
                .flex_shrink_0()
                .can_drop({
                    let takes = takes.clone();
                    move |any, _, _| any.downcast_ref::<ItemDrag>().is_some_and(&*takes)
                })
                .drag_over::<ItemDrag>({
                    let takes = takes.clone();
                    move |style, drag, _, _| {
                        if takes(drag) {
                            style::list_drop_indicator(style)
                        } else {
                            style.cursor_not_allowed()
                        }
                    }
                })
                .on_drop::<ItemDrag>(self.report_drop(group, before, host))
                .into_any_element(),
        )
    }

    /// Scroll the list while a row is dragged to its top or bottom edge, so a
    /// row can reach a place that is not on screen when the drag starts.
    ///
    /// The handler goes on the scrolling container rather than the row: a drag
    /// reports its moves to the elements that were under the pointer when it
    /// started, which is the row *and* everything around it, and the container
    /// is the one that is still under the pointer once the row has moved on.
    fn autoscroll(
        &self,
    ) -> Option<impl Fn(&gpui::DragMoveEvent<ItemDrag>, &mut Window, &mut App) + 'static> {
        self.drag_list.as_ref()?;
        let scroll = self.scroll.clone();
        Some(
            move |event: &gpui::DragMoveEvent<ItemDrag>, _: &mut Window, _: &mut App| {
                let bounds = event.bounds;
                let y = event.event.position.y;
                let step = if y < bounds.top() + drag::AUTOSCROLL_EDGE {
                    drag::AUTOSCROLL_STEP
                } else if y > bounds.bottom() - drag::AUTOSCROLL_EDGE {
                    -drag::AUTOSCROLL_STEP
                } else {
                    return;
                };
                let offset = scroll.offset();
                scroll.set_offset(gpui::point(offset.x, offset.y + step));
            },
        )
    }

    /// The column names, above the rows and outside the scrolling area. A
    /// list with no columns has no header.
    fn render_header(&self) -> Option<AnyElement> {
        if self.columns.is_empty() {
            return None;
        }
        let mut header = style::list_header(h_flex()).w_full().items_center();
        for column in &self.columns {
            header = header
                .child(style::list_cell(div(), column.width).child(column.label.to_uppercase()));
        }
        Some(header.into_any_element())
    }

    /// The selection checkbox, ahead of the caller's row.
    fn with_checkbox<A>(
        &self,
        row: AnyElement,
        row_ix: usize,
        marked: bool,
        host: &RowHost<A>,
    ) -> AnyElement
    where
        A: From<ItemListEvent> + 'static,
    {
        let host = host.clone();
        h_flex()
            .w_full()
            .items_start()
            .child(
                style::list_mark_gutter(div()).child(
                    Checkbox::new(("item-list-mark", row_ix))
                        .checked(marked)
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            host.push(ItemListEvent::ToggleMark { row_ix }.into(), cx);
                        }),
                ),
            )
            .child(div().flex_1().min_w_0().child(row))
            .into_any_element()
    }

    fn render_group<A>(
        &self,
        group: &GroupSpec,
        row_ix: usize,
        highlighted: bool,
        host: &RowHost<A>,
    ) -> AnyElement
    where
        A: From<ItemListEvent> + 'static,
    {
        let select_host = host.clone();
        let mut header = style::list_group(h_flex(), group.depth, self.lead_width())
            .flex_shrink_0()
            .items_center()
            .when(highlighted, style::highlighted)
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                select_host.push(ItemListEvent::Select { row_ix }.into(), cx);
            });

        if group.chevron {
            let toggle_host = host.clone();
            let key = group.key.clone();
            let collapsed = group.collapsed;
            header = header.child(
                style::list_group_chevron(div())
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        toggle_host
                            .push(ItemListEvent::ToggleGroup { key: key.clone() }.into(), cx);
                        cx.stop_propagation();
                    })
                    .child(if collapsed { "▸" } else { "▾" }),
            );
        }

        if group.editing {
            if let Some(editor) = &self.group_editor {
                let mut field = div().flex_1().min_w_0();
                if let Some(tag) = self.group_edit_tag {
                    field = field.key_context(tag);
                }
                return header
                    .child(field.child(Input::new(editor).w_full()))
                    .into_any_element();
            }
        }

        let label = match group.count {
            Some(count) => format!("{} ({count})", group.label),
            None => group.label.to_string(),
        };
        let mut text = div().flex_1().min_w_0().child(label);
        if let Some(on_rename) = group.on_rename.clone() {
            // Renaming from the heading follows the cursor, as double-click to
            // edit does on an item row.
            text = text.when(highlighted, |el| {
                el.on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    if event.click_count >= 2 {
                        on_rename(window, cx);
                        cx.stop_propagation();
                    }
                })
            });
        }
        header = header.child(text);

        for action in &group.actions {
            let on_click = action.on_click.clone();
            header = header.child(
                Button::new(gpui::ElementId::Name(
                    format!("group-action-{}-{}", group.key, action.id).into(),
                ))
                .label(action.label.clone())
                .ghost()
                .xsmall()
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    on_click(window, cx);
                }),
            );
        }
        header.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// phase 0
    ///   kind 1
    ///     section 2
    ///       item, item
    fn fixture() -> ItemList<&'static str> {
        let mut list = ItemList::new();
        list.set_rows(vec![
            ItemListRow::heading(GroupSpec::new("phase", 0, "Requirements phase").count(2)),
            ItemListRow::heading(GroupSpec::new("kind", 1, "Requirements").count(2)),
            ItemListRow::heading(GroupSpec::new("section", 2, "Offline").count(2)),
            ItemListRow::item("a", "Works offline"),
            ItemListRow::item("b", "Syncs later"),
        ]);
        list
    }

    /// Two sections under one kind, so a drop can cross a grouping boundary.
    ///
    /// phase 0
    ///   kind 1
    ///     offline 2 -> a, b
    ///     sync 2    -> c
    fn two_sections() -> ItemList<&'static str> {
        let mut list = ItemList::new();
        list.set_rows(vec![
            ItemListRow::heading(GroupSpec::new("phase", 0, "Requirements phase")),
            ItemListRow::heading(GroupSpec::new("kind", 1, "Requirements")),
            ItemListRow::heading(GroupSpec::new("offline", 2, "Offline")),
            ItemListRow::item("a", "Works offline"),
            ItemListRow::item("b", "Syncs later"),
            ItemListRow::heading(GroupSpec::new("sync", 2, "Sync")),
            ItemListRow::item("c", "Resolves conflicts"),
        ]);
        list
    }

    #[test]
    fn an_item_reads_the_headings_it_sits_under_off_the_flat_rows() {
        let list = two_sections();
        assert_eq!(list.group_chain(3), vec!["phase", "kind", "offline"]);
        assert_eq!(list.group_chain(6), vec!["phase", "kind", "sync"]);
        // One past the last row: the end of the list is in the last group.
        assert_eq!(list.group_chain(7), vec!["phase", "kind", "sync"]);
    }

    #[test]
    fn dropping_on_a_row_lands_ahead_of_it_and_dropping_past_the_end_lands_last() {
        let list = two_sections();
        assert_eq!(
            list.drop_position(Some(4)),
            (
                vec!["phase".into(), "kind".into(), "offline".into()],
                Some("b".into())
            )
        );
        // The strip past the last row: the end of the last group.
        assert_eq!(
            list.drop_position(None),
            (vec!["phase".into(), "kind".into(), "sync".into()], None)
        );
    }

    #[test]
    fn dropping_on_a_heading_lands_first_under_it() {
        let list = two_sections();
        // The heading's own key joins the chain, and the row lands ahead of
        // the first item under it.
        assert_eq!(
            list.drop_position(Some(5)),
            (
                vec!["phase".into(), "kind".into(), "sync".into()],
                Some("c".into())
            )
        );
    }

    #[test]
    fn a_heading_with_nothing_under_it_yet_takes_a_drop_at_its_end() {
        let mut list: ItemList<&'static str> = ItemList::new();
        list.set_rows(vec![
            ItemListRow::heading(GroupSpec::new("kind", 0, "Requirements")),
            ItemListRow::heading(GroupSpec::new("empty", 1, "Nothing here")),
        ]);
        // No item to land ahead of, so the drop is the group's end --- which is
        // how an item reaches a section that has nothing in it.
        assert_eq!(
            list.drop_position(Some(1)),
            (vec!["kind".into(), "empty".into()], None)
        );
    }

    #[test]
    fn an_empty_list_still_has_somewhere_to_drop() {
        let list: ItemList<&'static str> = ItemList::new();
        assert_eq!(list.drop_position(None), (Vec::new(), None));
    }

    #[test]
    fn the_cursor_starts_on_the_first_row_and_stops_at_either_end() {
        let mut list = fixture();
        assert_eq!(list.cursor(), Some(0));
        assert!(!list.move_cursor(-1));
        list.move_cursor(99);
        assert_eq!(list.cursor(), Some(4));
        assert!(!list.move_cursor(1));
    }

    #[test]
    fn the_cursor_follows_its_key_when_the_rows_change() {
        let mut list = fixture();
        list.move_cursor(4);
        assert_eq!(list.cursor_key(), Some("b"));
        // "b" moved above "a" and a group was added above them both.
        list.set_rows(vec![
            ItemListRow::heading(GroupSpec::new("phase", 0, "Requirements phase")),
            ItemListRow::heading(GroupSpec::new("kind", 1, "Requirements")),
            ItemListRow::item("b", "Syncs later"),
            ItemListRow::item("a", "Works offline"),
        ]);
        assert_eq!(list.cursor(), Some(2));
        assert_eq!(list.cursor_key(), Some("b"));
    }

    #[test]
    fn a_cursor_key_that_is_gone_falls_back_to_the_first_row() {
        let mut list = fixture();
        list.move_cursor(3);
        list.set_rows(vec![ItemListRow::item("c", "Something else")]);
        assert_eq!(list.cursor_key(), Some("c"));
        list.set_rows(Vec::new());
        assert_eq!(list.cursor(), None);
        assert_eq!(list.cursor_key(), None);
    }

    #[test]
    fn left_collapses_the_group_then_walks_out_to_the_enclosing_one() {
        let mut list = fixture();
        // On an item: out to its section.
        list.move_cursor(3);
        assert_eq!(list.collapse_step(), CollapseStep::MovedToParent);
        assert_eq!(list.cursor_key(), Some("section"));
        // On an expanded group: collapse it.
        assert_eq!(list.collapse_step(), CollapseStep::Collapsed);
        assert!(list.is_collapsed("section"));
        // On a collapsed group: out to the group above it, whatever the depth
        // distance.
        assert_eq!(list.collapse_step(), CollapseStep::MovedToParent);
        assert_eq!(list.cursor_key(), Some("kind"));
        assert_eq!(list.collapse_step(), CollapseStep::Collapsed);
        assert_eq!(list.collapse_step(), CollapseStep::MovedToParent);
        assert_eq!(list.cursor_key(), Some("phase"));
        // The outermost group has nowhere to walk out to.
        assert_eq!(list.collapse_step(), CollapseStep::Collapsed);
        assert_eq!(list.collapse_step(), CollapseStep::Nothing);
    }

    #[test]
    fn right_expands_only_a_collapsed_group() {
        let mut list = fixture();
        assert!(!list.expand_step());
        list.set_collapsed("phase", true);
        assert!(list.expand_step());
        assert!(!list.is_collapsed("phase"));
        // On an item, never.
        list.move_cursor(3);
        assert!(!list.expand_step());
    }

    #[test]
    fn the_selection_is_the_cursor_until_something_is_marked() {
        let mut list = fixture();
        list.move_cursor(3);
        assert_eq!(list.selection(), vec!["a"]);
        list.toggle_mark();
        list.move_cursor(1);
        list.toggle_mark();
        assert_eq!(list.selection(), vec!["a", "b"]);
        list.toggle_mark();
        assert_eq!(list.selection(), vec!["a"]);
        list.clear_marks();
        assert_eq!(list.selection(), vec!["b"]);
    }

    #[test]
    fn a_group_heading_is_not_selectable() {
        let mut list = fixture();
        // The cursor starts on the outermost heading.
        list.toggle_mark();
        assert_eq!(list.marked_count(), 0);
        assert!(list.selection().is_empty());
        list.toggle_mark_at(2);
        assert_eq!(list.marked_count(), 0);
    }

    #[test]
    fn a_marked_row_that_goes_away_leaves_the_selection() {
        let mut list = fixture();
        list.move_cursor(3);
        list.toggle_mark();
        list.set_rows(vec![ItemListRow::item("b", "Syncs later")]);
        assert_eq!(list.marked_count(), 0);
        assert_eq!(list.selection(), vec!["b"]);
    }

    #[test]
    fn a_mark_survives_the_row_being_hidden_and_shown_again() {
        let rows = || {
            vec![
                ItemListRow::heading(GroupSpec::new("section", 0, "Offline").count(2)),
                ItemListRow::item("a", "Works offline"),
                ItemListRow::item("b", "Syncs later"),
            ]
        };
        let mut list: ItemList<&'static str> = ItemList::new();
        list.set_rows(rows());
        list.move_cursor(1);
        list.toggle_mark();
        assert_eq!(list.selection(), vec!["a"]);

        // What a collapsed group does: its items are not passed in this time.
        list.set_rows(vec![ItemListRow::heading(
            GroupSpec::new("section", 0, "Offline").count(2),
        )]);
        assert_eq!(list.marked_count(), 0);

        list.set_rows(rows());
        assert_eq!(list.marked_count(), 1);
        assert_eq!(list.selection(), vec!["a"]);
    }

    #[test]
    fn a_renamed_group_keeps_its_collapsed_state() {
        let mut list = fixture();
        list.set_collapsed("section", true);
        list.rekey_collapsed("section", "section:renamed");
        assert!(!list.is_collapsed("section"));
        assert!(list.is_collapsed("section:renamed"));
    }

    #[test]
    fn a_group_heading_starts_where_the_content_column_does() {
        // A grouping is not a column: the heading's label lines up with the
        // content column, past the fixed ones.
        let list: ItemList<&str, ()> = ItemList::new().with_columns(vec![
            ColumnSpec::fixed("severity", "severity", px(64.)),
            ColumnSpec::fixed("status", "answer", px(120.)),
            ColumnSpec::content("finding", "finding"),
        ]);
        let gap = style::space::INLINE;
        assert_eq!(list.lead_width(), px(64.) + gap + px(120.) + gap);
    }

    #[test]
    fn a_marking_list_indents_its_headings_past_the_checkbox_gutter() {
        // The selection checkbox sits outside the row, so without this the
        // heading's label and the text beneath it would not line up.
        let plain: ItemList<&str, ()> = ItemList::new();
        let marking: ItemList<&str, ()> = ItemList::new().with_marking();
        assert_eq!(plain.lead_width(), px(0.));
        assert_eq!(marking.lead_width(), style::size::MARK_GUTTER);
    }

    #[test]
    fn a_row_menu_offers_what_the_row_affords_and_the_row_shows_only_some_of_it() {
        let list: ItemList<&str> = ItemList::new()
            .with_row_actions(|item: &&str| {
                vec![
                    RowAction::new("reverse", "Reverse", |_, _| {}),
                    RowAction::new("delete", format!("Delete {item}"), |_, _| {}).menu_only(),
                ]
            })
            .with_row_text(|item| item.to_string());
        assert!(list.has_row_menu());
        let menu = list.row_menu(&"Works offline").expect("a menu");
        let labels: Vec<_> = menu
            .actions
            .iter()
            .map(|action| action.label.to_string())
            .collect();
        assert_eq!(labels, vec!["Reverse", "Delete Works offline"]);
        assert_eq!(menu.text.as_deref(), Some("Works offline"));
        // The row itself shows only the action that is not menu-only.
        let shown: Vec<_> = menu
            .actions
            .iter()
            .filter(|action| !action.menu_only)
            .map(|action| action.id.to_string())
            .collect();
        assert_eq!(shown, vec!["reverse"]);
    }

    #[test]
    fn a_list_that_declares_neither_actions_nor_text_has_no_menu() {
        let list: ItemList<&str> = ItemList::new();
        assert!(!list.has_row_menu());
        assert!(list.row_menu(&"Works offline").is_none());
    }

    #[test]
    fn a_list_with_no_columns_leads_with_nothing() {
        let list: ItemList<&str, ()> = ItemList::new();
        assert_eq!(list.lead_width(), px(0.));
        assert!(list.render_header().is_none());
    }
}
