//! The status dropdown a row's status chip opens.
//!
//! A plan step and a review finding both have a status the user sets by
//! picking from the ones that status may take. The menu — which item it is
//! open on, which entry the keyboard is on, and the anchored popup — is the
//! same wherever it is shown; what choosing does is not, so the caller hands
//! in what to run. A conversation records the choice as the user's own edit,
//! the plan panel applies it as a plain outline mutation.

use crate::ui::style;
use gpui::{
    Anchor, AnyElement, App, ElementId, InteractiveElement, IntoElement, MouseButton,
    ParentElement, SharedString, Styled, Window, anchored, deferred, div, prelude::FluentBuilder,
    px,
};
use gpui_component::{Icon, Sizable, h_flex, v_flex};
use gpui_kit_assets::IconName;
use std::rc::Rc;
use uuid::Uuid;

/// The status dropdown open on one item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusMenu {
    /// The plan step or finding it is open on.
    pub item: Uuid,
    /// The statuses it lists.
    pub options: &'static [&'static str],
    /// Index into `options`.
    pub highlighted: usize,
}

impl StatusMenu {
    /// Open on `item`, highlighting `current` — the first entry when the
    /// item's status is not one the menu offers.
    pub fn open(item: Uuid, options: &'static [&'static str], current: &str) -> Self {
        let highlighted = options.iter().position(|s| *s == current).unwrap_or(0);
        Self {
            item,
            options,
            highlighted,
        }
    }

    /// Move the highlight, stopping at either end. `false` when it did not
    /// move.
    pub fn move_highlight(&mut self, delta: isize) -> bool {
        let last = self.options.len() as isize - 1;
        let next = (self.highlighted as isize + delta).clamp(0, last) as usize;
        let moved = next != self.highlighted;
        self.highlighted = next;
        moved
    }

    /// The status the highlight is on.
    pub fn choice(&self) -> &'static str {
        self.options[self.highlighted]
    }

    pub fn is_on(&self, item: Uuid) -> bool {
        self.item == item
    }
}

/// What the chip and its menu do, in the caller's terms.
#[derive(Clone)]
pub struct StatusMenuHandlers {
    /// The chip was clicked: open the menu on this item, or close it when it
    /// is already open there.
    pub toggle: Rc<dyn Fn(&mut Window, &mut App)>,
    /// An entry was picked.
    pub choose: Rc<dyn Fn(&'static str, &mut Window, &mut App)>,
    /// A click landed outside the open menu.
    pub dismiss: Rc<dyn Fn(&mut Window, &mut App)>,
}

/// An item's status as a chip that opens the dropdown of the others. `menu`
/// is the open menu when it is open on *this* item, and `tone` what the
/// status says at a glance.
pub fn status_chip(
    id: impl Into<SharedString>,
    status: &str,
    tone: style::StatusTone,
    menu: Option<StatusMenu>,
    handlers: StatusMenuHandlers,
    _cx: &mut App,
) -> AnyElement {
    let id: SharedString = id.into();
    let current = status.to_string();
    let popup = menu.map(|menu| render_menu(&id, menu, &current, &handlers));
    let toggle = handlers.toggle.clone();
    div()
        .id(ElementId::Name(id))
        .relative()
        .flex_shrink_0()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            toggle(window, cx);
        })
        .child(
            style::status_chip(h_flex(), tone)
                .items_center()
                .gap(style::space::HAIRLINE)
                .child(current.clone())
                .child(Icon::new(IconName::ChevronDown).xsmall()),
        )
        .when_some(popup, |el, popup| {
            el.child(
                deferred(
                    anchored()
                        .anchor(Anchor::TopLeft)
                        .snap_to_window_with_margin(px(8.))
                        .child(div().occlude().mt_1().child(popup)),
                )
                .with_priority(1),
            )
        })
        .into_any_element()
}

/// The open dropdown: one entry per status, a tick beside the current one.
fn render_menu(
    id: &SharedString,
    menu: StatusMenu,
    current: &str,
    handlers: &StatusMenuHandlers,
) -> AnyElement {
    let dismiss = handlers.dismiss.clone();
    let mut list = style::floating_panel(v_flex())
        .id(ElementId::Name(format!("{id}-menu").into()))
        .min_w(px(160.))
        .gap(style::space::HAIRLINE)
        .px(style::space::INLINE)
        .py(style::space::INLINE)
        .on_mouse_down_out(move |_, window, cx| dismiss(window, cx));
    for (ix, status) in menu.options.iter().copied().enumerate() {
        let choose = handlers.choose.clone();
        list = list.child(
            style::menu_item(h_flex(), ix == menu.highlighted)
                .id(ElementId::Name(format!("{id}-{status}").into()))
                .w_full()
                .items_center()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    cx.stop_propagation();
                    choose(status, window, cx);
                })
                .child(
                    div()
                        .w(px(16.))
                        .flex_shrink_0()
                        .when(status == current, |el| {
                            el.child(Icon::new(IconName::Check).xsmall())
                        }),
                )
                .child(div().flex_1().child(status)),
        );
    }
    list.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUSES: [&str; 3] = ["open", "in_progress", "done"];

    #[test]
    fn the_menu_opens_on_the_current_status_and_stops_at_both_ends() {
        let item = Uuid::new_v4();
        let mut menu = StatusMenu::open(item, &STATUSES, "in_progress");
        assert!(menu.is_on(item));
        assert_eq!(menu.choice(), "in_progress");
        assert!(menu.move_highlight(1));
        assert_eq!(menu.choice(), "done");
        // The ends hold: another step down changes nothing.
        assert!(!menu.move_highlight(1));
        assert_eq!(menu.choice(), "done");
        assert!(menu.move_highlight(-5));
        assert_eq!(menu.choice(), "open");
        assert!(!menu.move_highlight(-1));
    }

    #[test]
    fn a_status_the_menu_does_not_offer_highlights_the_first_entry() {
        let menu = StatusMenu::open(Uuid::new_v4(), &STATUSES, "waived");
        assert_eq!(menu.choice(), "open");
    }
}
