//! Dragging a row to a new place.
//!
//! One payload type for every list in the app ([`ItemDrag`]), so every drop
//! target asks the same two questions of every drag — which list did this row
//! come from, and which item is it. A target elsewhere in the app (a node on
//! the task tree) reads the same payload as a target inside the list, so the
//! gesture does not have to be built twice.
//!
//! Where a row lands is reported as the item it lands *ahead of*
//! ([`ItemDropped::before`]), never as an index. A list's grouping and its
//! ordinal space are not always the same space — an obligation is ordered
//! across the whole of its kind, but grouped by section inside it — so an
//! index counted here would be an index into the wrong list. The neighbour is
//! unambiguous in every list, and each view turns it into whatever its own
//! mutation counts in.

use crate::ui::style;
use gpui::{Context, IntoElement, ParentElement, Render, SharedString, Window, div};

/// How close to the top or bottom edge the pointer has to come before a drag
/// scrolls the list under it.
pub(super) const AUTOSCROLL_EDGE: gpui::Pixels = gpui::px(24.);

/// How far one drag-move inside that edge scrolls.
pub(super) const AUTOSCROLL_STEP: gpui::Pixels = gpui::px(12.);

/// A row of a list, while it is being dragged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemDrag {
    /// The list the row came from ([`super::ItemList::with_reorder`]), so a
    /// target can refuse a row that did not come from a list it accepts.
    pub list: SharedString,
    /// The item's key: the caller's own id for it, as it was given to
    /// [`super::ItemListRow::item`].
    pub key: String,
    /// The headings the row sits under, outermost first — where it is being
    /// dragged *from*. A list compares these with the target's
    /// ([`ItemDropped::group`]) to decide whether a drop is one it allows:
    /// what a group means is the view's, so the component can only report
    /// both ends.
    pub group: Vec<String>,
    /// What the row says, for the preview that follows the pointer.
    pub label: SharedString,
}

/// Where a dragged row was dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemDropped {
    pub from: ItemDrag,
    /// The headings it landed under, outermost first. Empty in a list that
    /// does not group. A view compares this with the row's own group to see
    /// whether the drop moved it somewhere else as well as reordering it.
    pub group: Vec<String>,
    /// The item it landed ahead of, or `None` at the end of that group.
    pub before: Option<String>,
}

/// The chip that follows the pointer while a row is dragged: what the row
/// says, so the user can see which one they have hold of once it is over a
/// list too long to show its origin.
pub(super) struct ItemDragPreview {
    pub(super) label: SharedString,
}

impl Render for ItemDragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        style::drag_preview(div()).child(self.label.clone())
    }
}
