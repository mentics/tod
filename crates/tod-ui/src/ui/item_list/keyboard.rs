//! The one key set every item list answers to.
//!
//! A list registers these under its own surface context, so the same keystroke
//! does the same thing in the obligations panel, the conversation side pane,
//! and anywhere else a list is shown. A list that does not support an action
//! simply has no handler for it; the key then does nothing rather than doing
//! something different.

use crate::ui::key_context;
use gpui::{App, KeyBinding, actions};

actions!(
    item_list,
    [
        ItemListUp,
        ItemListDown,
        ItemListPageUp,
        ItemListPageDown,
        ItemListHome,
        ItemListEnd,
        /// Left: collapse the group, else move to the enclosing group.
        ItemListCollapse,
        /// Right: expand the collapsed group.
        ItemListExpand,
        /// Enter: edit the item under the cursor, or create one.
        ItemListActivate,
        /// F2: edit the item under the cursor.
        ItemListEdit,
        /// Ctrl+Enter in a multi-line field, Enter in a single-line one.
        ItemListCommitEdit,
        ItemListCreateBelow,
        ItemListCreateAbove,
        ItemListCreateChild,
        ItemListDelete,
        ItemListMoveUp,
        ItemListMoveDown,
        /// Space: add or remove the item under the cursor from the selection.
        ItemListToggleMark,
        /// `s`: start a new group.
        ItemListAddGroup,
        ItemListFocusSearch,
        /// Space while the search field is focused — types a literal space
        /// rather than being swallowed as [`ItemListToggleMark`].
        ItemListSearchSpace,
    ]
);

/// Which parts of the key set a list wants. Navigation is never optional.
#[derive(Clone, Copy, Debug, Default)]
pub struct ItemListKeys {
    /// Enter, F2, Ctrl+Enter.
    pub editing: bool,
    /// `n`, Alt+Enter, Backspace/Delete.
    pub creation: bool,
    /// Cmd/Ctrl+Up/Down.
    pub reordering: bool,
    /// Space.
    pub marking: bool,
    /// `s`, Shift+Enter. Creating *into* a group is grouping, not creation:
    /// a list with no headings has nowhere to put the new item.
    pub grouping: bool,
    /// Ctrl+F, and Space inside the field.
    pub search: bool,
}

impl ItemListKeys {
    /// Everything: the obligations panel's set.
    pub fn all() -> Self {
        Self {
            editing: true,
            creation: true,
            reordering: true,
            marking: true,
            grouping: true,
            search: true,
        }
    }

    pub fn editing(mut self) -> Self {
        self.editing = true;
        self
    }

    pub fn creation(mut self) -> Self {
        self.creation = true;
        self
    }

    pub fn reordering(mut self) -> Self {
        self.reordering = true;
        self
    }

    pub fn marking(mut self) -> Self {
        self.marking = true;
        self
    }

    pub fn search(mut self) -> Self {
        self.search = true;
        self
    }
}

/// Register the item-list keys on `surface`.
///
/// Navigation and collapse/expand are always bound. Because Left/Right act on
/// the list's own groups, a surface bound here crosses panels with
/// Ctrl+Left/Right ([`crate::ui::pane_nav::bind_modified_pane_nav`]) — call
/// that too.
pub fn bind_item_list_keys(cx: &mut App, surface: &str, keys: ItemListKeys) {
    let context = Some(key_context::excluding_input(surface));
    let mut bindings = vec![
        KeyBinding::new("up", ItemListUp, context),
        KeyBinding::new("down", ItemListDown, context),
        KeyBinding::new("pageup", ItemListPageUp, context),
        KeyBinding::new("pagedown", ItemListPageDown, context),
        KeyBinding::new("home", ItemListHome, context),
        KeyBinding::new("end", ItemListEnd, context),
        KeyBinding::new("left", ItemListCollapse, context),
        KeyBinding::new("right", ItemListExpand, context),
    ];
    if keys.editing {
        bindings.extend([
            KeyBinding::new("enter", ItemListActivate, context),
            KeyBinding::new("f2", ItemListEdit, context),
            // The item editor is multi-line: arrows move the cursor as usual,
            // Escape abandons the edit, and Ctrl+Enter commits it.
            KeyBinding::new(
                "ctrl-enter",
                ItemListCommitEdit,
                Some(key_context::including_input(surface)),
            ),
        ]);
    }
    if keys.creation {
        bindings.extend([
            KeyBinding::new("n", ItemListCreateBelow, context),
            KeyBinding::new("alt-enter", ItemListCreateAbove, context),

            KeyBinding::new("backspace", ItemListDelete, context),
            KeyBinding::new("delete", ItemListDelete, context),
        ]);
    }
    if keys.reordering {
        bindings.extend([
            KeyBinding::new("secondary-up", ItemListMoveUp, context),
            KeyBinding::new("secondary-down", ItemListMoveDown, context),
        ]);
    }
    if keys.marking {
        bindings.push(KeyBinding::new("space", ItemListToggleMark, context));
    }
    if keys.grouping {
        bindings.extend([
            KeyBinding::new("s", ItemListAddGroup, context),
            KeyBinding::new("shift-enter", ItemListCreateChild, context),
            // Shift+Enter also lands from inside the item editor, which
            // commits and opens the next one in the same group.
            KeyBinding::new(
                "shift-enter",
                ItemListCreateChild,
                Some(key_context::including_input(surface)),
            ),
        ]);
    }
    if keys.search {
        bindings.extend([
            KeyBinding::new("ctrl-f", ItemListFocusSearch, context),
            KeyBinding::new(
                "space",
                ItemListSearchSpace,
                Some(key_context::including_input(surface)),
            ),
        ]);
    }
    cx.bind_keys(bindings);
}

/// Bind plain Enter to commit the field tagged `tag` inside `surface` — a
/// single-line field (a group's name), where Enter is not needed for newlines.
pub fn bind_single_line_commit(cx: &mut App, surface: &str, tag: &str) {
    cx.bind_keys([KeyBinding::new(
        "enter",
        ItemListCommitEdit,
        Some(key_context::including_tag(surface, tag)),
    )]);
}
