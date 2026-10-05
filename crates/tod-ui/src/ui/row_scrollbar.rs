//! A scrollbar for a [`ListState`] that moves by row, not by pixel.
//!
//! A virtualized list only knows the height of the rows it has laid out; the
//! rest are estimated, and wrapped text can be far taller than the estimate.
//! A pixel scrollbar therefore changes length as rows are measured, and a
//! drag to the bottom lands short of the real end. Here the thumb's position
//! is the fraction of the way through the *rows* (a row and how far into it),
//! which does not depend on any height, so the bottom of the track is always
//! the end of the list, however long it is and whatever has been measured.

use gpui::{Bounds, ListOffset, ListState, Pixels, Point, Size, point, px, size};
use gpui_component::scroll::ScrollbarHandle;
use std::cell::Cell;
use std::rc::Rc;

/// Virtual height of one row on the track. Only the ratios matter.
const ROW_UNIT: f32 = 24.;
/// Row height assumed when the row at the top has not been laid out.
const FALLBACK_ROW_HEIGHT: f32 = 80.;
/// Shortest track the thumb can travel while the list overflows at all.
const MIN_TRAVEL: f32 = 1.;
/// Closer than this to the last scroll position counts as the end.
const END_EPSILON: f32 = 1e-3;

#[derive(Clone)]
pub struct RowScrollbar {
    list: ListState,
    /// The last drag position was the end of the list.
    dragged_to_end: Rc<Cell<bool>>,
}

impl RowScrollbar {
    pub fn new(list: &ListState) -> Self {
        Self {
            list: list.clone(),
            dragged_to_end: Rc::new(Cell::new(false)),
        }
    }

    /// Height of the row `ix`, in pixels, when it is on screen.
    fn row_height(&self, ix: usize) -> f32 {
        self.list
            .bounds_for_item(ix)
            .map(|bounds| bounds.size.height.as_f32())
            .filter(|height| *height > 0.)
            .unwrap_or(FALLBACK_ROW_HEIGHT)
    }

    /// The last row-position the top of the viewport can have: the rows that
    /// do not fit on one screen.
    fn max_position(&self) -> f32 {
        // The list's own measure says whether there is anything to scroll.
        // The estimate below depends on the height of whichever row is on
        // top, which changes as it is dragged; it must never decide that
        // there is nothing to scroll, or the scrollbar disappears mid-drag.
        if self.list.max_offset_for_scrollbar().y.as_f32() <= 0. {
            return 0.;
        }
        let viewport = self.list.viewport_bounds().size.height.as_f32();
        let top = self.list.logical_scroll_top().item_ix;
        let visible = viewport / self.row_height(top);
        (self.list.item_count() as f32 - visible).max(MIN_TRAVEL)
    }
}

impl ScrollbarHandle for RowScrollbar {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.list.viewport_bounds()
    }

    fn offset(&self) -> Point<Pixels> {
        let max = self.max_position();
        let top = self.list.logical_scroll_top();
        let at_end =
            top.item_ix >= self.list.item_count() || self.list.is_scrolled_to_end() == Some(true);
        let position = if at_end {
            max
        } else {
            let within = (top.offset_in_item.as_f32() / self.row_height(top.item_ix)).min(1.);
            (top.item_ix as f32 + within).min(max)
        };
        point(px(0.), px(-position * ROW_UNIT))
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let max = self.max_position();
        let position = (-offset.y.as_f32() / ROW_UNIT).clamp(0., max);
        if max > 0. && position >= max - END_EPSILON {
            self.dragged_to_end.set(true);
            self.list.scroll_to_end();
            return;
        }
        self.dragged_to_end.set(false);
        let item_ix = position.floor() as usize;
        let within = position - item_ix as f32;
        self.list.scroll_to(ListOffset {
            item_ix,
            offset_in_item: px(within * self.row_height(item_ix)),
        });
    }

    fn content_size(&self) -> Size<Pixels> {
        let viewport = self.list.viewport_bounds().size;
        size(
            viewport.width,
            viewport.height + px(self.max_position() * ROW_UNIT),
        )
    }

    fn end_drag(&self) {
        // Rows measured during the drag may have moved the end: ask again.
        if self.dragged_to_end.take() {
            self.list.scroll_to_end();
        }
    }
}
