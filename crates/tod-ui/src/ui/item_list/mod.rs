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

pub mod keyboard;
pub mod search;

use std::collections::HashSet;
use std::rc::Rc;

use crate::ui::style;
use crate::views::rows::{RowAction, RowHost};
use gpui::{
    AnyElement, App, Div, InteractiveElement, IntoElement, MouseButton, ParentElement, ScrollHandle,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
    px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::Scrollbar;
use gpui_component::{Sizable as _, h_flex};

pub use keyboard::{ItemListKeys, bind_item_list_keys, bind_single_line_commit};

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
}

impl ItemRowState<'_> {
    /// Wrap `el` as the named column's cell, so it lines up with the same
    /// column on every other row and with the header. A key the list does not
    /// declare leaves the element as it is.
    pub fn column<E: Styled>(&self, key: &str, el: E) -> E {
        column_cell(self.columns, key, el)
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
    drag: Option<Rc<dyn Fn(&T, Stateful<Div>) -> Stateful<Div>>>,
    context_menu: Option<Rc<dyn Fn(&T, Stateful<Div>) -> Stateful<Div>>>,
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
            drag: None,
            context_menu: None,
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

    /// Make item rows draggable. The hook applies the caller's own payload
    /// type to the row the component built, so the gesture lives here once
    /// while the payload stays the caller's.
    pub fn with_drag(
        mut self,
        drag: impl Fn(&T, Stateful<Div>) -> Stateful<Div> + 'static,
    ) -> Self {
        self.drag = Some(Rc::new(drag));
        self
    }

    /// Give item rows a context menu, the same way.
    pub fn with_context_menu(
        mut self,
        menu: impl Fn(&T, Stateful<Div>) -> Stateful<Div> + 'static,
    ) -> Self {
        self.context_menu = Some(Rc::new(menu));
        self
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
                    let state = ItemRowState {
                        row_ix,
                        key,
                        highlighted,
                        marked,
                        editing: self.editing_key.as_deref() == Some(key.as_str()),
                        columns: &self.columns,
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
            if let Some(item) = row.as_item() {
                if let Some(drag) = &self.drag {
                    wrapper = drag(item, wrapper);
                }
                if let Some(menu) = &self.context_menu {
                    wrapper = menu(item, wrapper);
                }
            }
            elements.push(wrapper.into_any_element());
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
                    .children(elements),
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

    /// The column names, above the rows and outside the scrolling area. A
    /// list with no columns has no header.
    fn render_header(&self) -> Option<AnyElement> {
        if self.columns.is_empty() {
            return None;
        }
        let mut header = style::list_header(h_flex()).w_full().items_center();
        for column in &self.columns {
            header = header.child(
                style::list_cell(div(), column.width).child(column.label.to_uppercase()),
            );
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
                        toggle_host.push(
                            ItemListEvent::ToggleGroup {
                                key: key.clone(),
                            }
                            .into(),
                            cx,
                        );
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
    fn a_list_with_no_columns_leads_with_nothing() {
        let list: ItemList<&str, ()> = ItemList::new();
        assert_eq!(list.lead_width(), px(0.));
        assert!(list.render_header().is_none());
    }
}
