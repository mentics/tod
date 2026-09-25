//! `ColumnPanel`: the contract every panel shown in a unified-view column
//! implements, and the events a panel sends the unified view's root.

use gpui::{App, SharedString};
use tod_store::conversation::Focus;

use super::columns::PanelKind;

/// What any panel hosted in a unified-view column must offer the root: a
/// title for its column header, and a description of what it targets.
pub trait ColumnPanel {
    /// The column header's title for this panel.
    fn title(&self, cx: &App) -> SharedString;

    /// What this panel is showing, for the column header's subtitle.
    fn target_label(&self, cx: &App) -> SharedString;
}

/// Emitted when the user activates a link inside a panel — a click, ctrl
/// click, or Enter/Ctrl+Enter on the focused link. The root applies the
/// column-placement rule (`ColumnModel::open`) using `from_column` (this
/// panel's own column index, supplied by the root when it renders).
#[derive(Debug, Clone)]
pub struct PanelOpenRequest {
    pub target: PanelKind,
    pub ctrl: bool,
}

/// Emitted by a panel when its own selection changes to something that can
/// hold an agent session (an obligation or a plan step, alongside a node
/// selection in the tree or Details). The chat drawer follows whichever of
/// these happened most recently, across panels
/// (`doc/ui/unified-view.md` "The chat drawer").
#[derive(Debug, Clone, Copy)]
pub struct PanelFocusSelected(pub Focus);

/// Emitted by a panel when **E** is pressed on an item that has no
/// conversation yet: there is no transcript to open, so the chat drawer
/// opens on the item instead, where its first conversation starts.
#[derive(Debug, Clone, Copy)]
pub struct PanelOpenChat(pub Focus);
