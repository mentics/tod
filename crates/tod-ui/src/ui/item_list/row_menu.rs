//! The right-click menu on an item row.
//!
//! The component owns the gesture, the anchoring and the chrome — once, here —
//! so no list installs a menu of its own. A list says only what an item
//! *affords* ([`super::ItemList::with_row_actions`]) and what its text is
//! ([`super::ItemList::with_row_text`]); the entries are then the row's own
//! actions, the same ones its hover buttons show, plus Copy.
//!
//! The gesture itself is `gpui_component`'s [`ContextMenuExt`], as
//! [`crate::ui::selectable_text`] uses it: it anchors the popup at the
//! pointer, paints it above everything, and dismisses it.

use gpui::{App, ClipboardItem, Div, Stateful, Window};
use gpui_base::TextSelection;
use gpui_component::Icon;
use gpui_component::menu::{ContextMenu, ContextMenuExt, PopupMenu, PopupMenuItem};

use crate::ui::style;
use crate::views::rows::RowAction;

/// What a row's menu offers.
#[derive(Default)]
pub struct RowMenu {
    /// The row's own actions, in the order the list declared them. Includes
    /// the menu-only ones, which are exactly what this is for.
    pub actions: Vec<RowAction>,
    /// The row's text, copied when nothing is selected. `None` where the list
    /// does not say what a row's text is — `T` is the caller's own payload and
    /// the row is an opaque element, so the component cannot read it off
    /// either.
    pub text: Option<String>,
}

impl RowMenu {
    /// A menu with nothing in it is not installed at all.
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.text.is_none()
    }
}

/// What Copy puts on the clipboard: what the user selected, else the whole
/// row. Empty when there is neither, and the entry is then disabled.
pub(super) fn copy_text(selected: &str, row: Option<&str>) -> String {
    let selected = selected.trim();
    if !selected.is_empty() {
        return selected.to_string();
    }
    row.unwrap_or_default().trim().to_string()
}

/// Hang `menu` off `row`: right-clicking the row opens it at the pointer.
pub(super) fn with_row_menu(row: Stateful<Div>, menu: RowMenu) -> ContextMenu<Stateful<Div>> {
    row.context_menu(move |popup, window, cx| build(popup, &menu, window, cx))
}

fn build(mut popup: PopupMenu, menu: &RowMenu, window: &mut Window, cx: &mut App) -> PopupMenu {
    for action in &menu.actions {
        let on_click = action.on_click.clone();
        let mut item = PopupMenuItem::new(action.label.clone())
            .on_click(move |_, window, cx| on_click(window, cx));
        if let Some(icon) = action.icon {
            item = item.icon(Icon::new(icon));
        }
        popup = popup.item(item);
    }
    // Read the selection as the menu opens: the left mouse-down that clicks
    // Copy clears it before the entry's own handler runs.
    let text = copy_text(
        &TextSelection::selected_text(window, cx),
        menu.text.as_deref(),
    );
    popup
        .separator()
        .item(
            PopupMenuItem::new("Copy")
                .disabled(text.is_empty())
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                }),
        )
        .min_w(style::size::ROW_MENU_MIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::item_list::{ItemList, ItemListEvent, ItemListRow};
    use crate::views::rows::RowHost;
    use gpui::{
        AppContext as _, Context, IntoElement, Modifiers, MouseButton, ParentElement as _, Render,
        Styled as _, TestAppContext, VisualTestContext, div, point, px,
    };
    use gpui_component::Root;
    use std::cell::Cell;
    use std::rc::Rc;

    /// A list of two rows, 20px each, in a 240x200 window.
    struct ListHost {
        list: ItemList<&'static str>,
        host: RowHost<ItemListEvent>,
        cursor: Rc<Cell<Option<usize>>>,
    }

    impl Render for ListHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            for event in self.host.drain() {
                if let ItemListEvent::Select { row_ix } = event {
                    self.list.set_cursor(row_ix);
                    self.cursor.set(Some(row_ix));
                }
            }
            div().w(px(240.)).h(px(200.)).child(self.list.render(
                "rows",
                &self.host,
                |item, _, _, _| div().h(px(20.)).child(*item).into_any_element(),
                window,
                cx,
            ))
        }
    }

    /// Open a window on `list` and right-click the second row.
    fn right_click_second_row<'a>(
        list: ItemList<&'static str>,
        cx: &'a mut TestAppContext,
    ) -> (Rc<Cell<Option<usize>>>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let cursor = Rc::new(Cell::new(None));
        let seen = cursor.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let mut list = list;
            list.set_rows(vec![
                ItemListRow::item("a", "Works offline"),
                ItemListRow::item("b", "Syncs later"),
            ]);
            let view = cx.new(|cx| ListHost {
                list,
                host: RowHost::for_entity(cx.weak_entity()),
                cursor: seen,
            });
            Root::new(view, window, cx)
        });
        let cx: &mut VisualTestContext = cx;
        draw(cx);
        cx.simulate_mouse_down(AT, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(AT, MouseButton::Right, Modifiers::default());
        // The menu is built in a deferred callback and rendered on the next
        // draw, which is also when the cursor move is applied.
        cx.run_until_parked();
        draw(cx);
        (cursor, cx)
    }

    /// Inside the second row (rows are 20px tall).
    const AT: gpui::Point<gpui::Pixels> = gpui::Point {
        x: px(20.),
        y: px(28.),
    };

    /// The first menu entry, which opens at the pointer.
    fn first_entry() -> gpui::Point<gpui::Pixels> {
        point(AT.x + px(30.), AT.y + px(16.))
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn right_click_moves_the_cursor_and_runs_the_rows_own_action(cx: &mut TestAppContext) {
        let ran = Rc::new(Cell::new(None));
        let fired = ran.clone();
        let list: ItemList<&'static str> = ItemList::new().with_row_actions(move |item: &&str| {
            let fired = fired.clone();
            let item = *item;
            vec![RowAction::new("edit", "Edit", move |_, _| fired.set(Some(item))).menu_only()]
        });
        let (cursor, cx) = right_click_second_row(list, cx);
        assert_eq!(
            cursor.get(),
            Some(1),
            "right-click moves the cursor to the row"
        );
        cx.simulate_click(first_entry(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            ran.get(),
            Some("Syncs later"),
            "the entry acts on the row that was right-clicked"
        );
    }

    #[gpui::test]
    fn right_click_copy_copies_the_whole_row_when_nothing_is_selected(cx: &mut TestAppContext) {
        let list: ItemList<&'static str> =
            ItemList::new().with_row_text(|item: &&str| item.to_string());
        let (_, cx) = right_click_second_row(list, cx);
        cx.simulate_click(first_entry(), Modifiers::default());
        cx.run_until_parked();
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some("Syncs later"));
    }

    #[test]
    fn copy_takes_the_selection_over_the_row() {
        assert_eq!(copy_text("  beta ", Some("alpha beta")), "beta");
        assert_eq!(copy_text("", Some("alpha beta")), "alpha beta");
        assert_eq!(copy_text("  ", Some(" alpha ")), "alpha");
        // Nothing selected and a list that cannot say what the row's text is:
        // the entry has nothing to copy and is shown disabled.
        assert!(copy_text("", None).is_empty());
        assert!(copy_text("", Some("   ")).is_empty());
    }
}
