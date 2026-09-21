//! Scroll a scroll container the least amount needed to show one element.
//!
//! GPUI's `ScrollAnchor::scroll_to` always pins the anchored element to the top
//! of the viewport, even when it is already fully visible — so focusing or
//! clicking a visible field made the panel jump. `ScrollReveal` records the
//! marked element's bounds as it paints and, when asked, leaves the scroll
//! alone if the element is fully visible; otherwise it scrolls just far enough
//! to bring its nearer edge into view (its top, if it is taller than the
//! viewport).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, App, Bounds, ParentElement, Pixels, ScrollHandle, Styled, Window, point, px,
};

#[derive(Clone)]
pub struct ScrollReveal {
    handle: ScrollHandle,
    target: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl ScrollReveal {
    pub fn for_handle(handle: ScrollHandle) -> Self {
        Self {
            handle,
            target: Rc::default(),
        }
    }

    /// Mark `el` as the element to reveal. Mark at most one element per frame.
    pub fn mark<E: ParentElement + Styled>(&self, el: E) -> E {
        let target = self.target.clone();
        el.child(
            canvas(move |bounds, _, _| target.set(Some(bounds)), |_, _, _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        )
    }

    /// After the next frame, scroll the minimum needed to show the marked
    /// element fully. Does nothing if it is already fully visible.
    pub fn reveal(&self, window: &mut Window, _cx: &mut App) {
        self.target.set(None);
        let this = self.clone();
        // Two frames: the first paints the newly marked element, recording
        // its bounds; the second reads them.
        window.on_next_frame(move |window, _| {
            window.refresh();
            window.on_next_frame(move |window, _| this.apply(window));
        });
    }

    fn apply(&self, window: &mut Window) {
        let Some(target) = self.target.get() else {
            return;
        };
        let viewport = self.handle.bounds();
        let offset = self.handle.offset();
        let delta = reveal_delta(viewport.top(), viewport.bottom(), target.top(), target.bottom());
        if delta != px(0.) {
            let max = self.handle.max_offset().y;
            let y = (offset.y + delta).clamp(-max, px(0.));
            self.handle.set_offset(point(offset.x, y));
            window.refresh();
        }
    }
}

/// How far to move the scroll offset (GPUI offsets grow negative as content
/// scrolls up) so `[top, bottom]` fits in `[view_top, view_bottom]`.
fn reveal_delta(view_top: Pixels, view_bottom: Pixels, top: Pixels, bottom: Pixels) -> Pixels {
    if top >= view_top && bottom <= view_bottom {
        px(0.)
    } else if top < view_top || bottom - top > view_bottom - view_top {
        view_top - top
    } else {
        view_bottom - bottom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fully_visible_does_not_scroll() {
        assert_eq!(reveal_delta(px(0.), px(100.), px(10.), px(90.)), px(0.));
        assert_eq!(reveal_delta(px(0.), px(100.), px(0.), px(100.)), px(0.));
    }

    #[test]
    fn below_scrolls_just_enough_to_show_bottom() {
        assert_eq!(reveal_delta(px(0.), px(100.), px(90.), px(120.)), px(-20.));
    }

    #[test]
    fn above_scrolls_just_enough_to_show_top() {
        assert_eq!(reveal_delta(px(0.), px(100.), px(-15.), px(20.)), px(15.));
    }

    #[test]
    fn taller_than_viewport_aligns_top() {
        assert_eq!(reveal_delta(px(0.), px(100.), px(50.), px(300.)), px(-50.));
    }
}
