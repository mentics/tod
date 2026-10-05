//! A scrollbar for a [`ListState`] that moves by row, not by pixel.
//!
//! A virtualized list only knows the height of the rows it has laid out; the
//! rest count for nothing, so a pixel scrollbar is the wrong length and a drag
//! to its end lands nowhere near the real end of a long list. Here the thumb
//! is placed by *rows*: `p` is how far the top of the viewport is through the
//! rows (a row and the fraction of it above the viewport), and `visible` is
//! how many rows the viewport shows (also fractional, from the rows on
//! screen). The top of the viewport can reach `count - visible`, so
//!
//! ```text
//! thumb position = p / (count - visible)
//! thumb length   = visible / count
//! ```
//!
//! which is exactly 0 at the top and exactly 1 at the end whatever any row's
//! height is, because at the end `p + visible == count`. A scrollbar must be
//! kept between frames (it remembers a drag), so the owner of the list holds
//! one and hands out clones.

use gpui::{Bounds, ListOffset, ListState, Pixels, Point, Size, point, px, size};
use gpui_component::scroll::ScrollbarHandle;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Smallest span and visible-row count, so nothing divides by zero.
const END_EPSILON: f32 = 1e-3;
/// Virtual pixels from either end of the track that count as the end.
const SNAP_PIXELS: f32 = 1.5;
/// Closer than this (in pixels) to the bottom of the viewport counts as the end.
const END_PIXELS: f32 = 0.5;

/// Where the viewport is, measured in rows.
#[derive(Clone, Copy, Debug)]
struct Position {
    /// Rows above the top of the viewport (the row it starts in, plus the
    /// fraction of that row that is above it).
    top: f32,
    /// Rows the viewport shows, fractionally.
    visible: f32,
    at_end: bool,
}

#[derive(Default)]
struct State {
    /// The thumb is being dragged.
    dragging: bool,
    /// While dragging: the thumb's place on the track.
    drag_fraction: f32,
    /// How many rows the viewport shows when the list is at its end (0 until
    /// known). The thumb's length and its place both use this, not the rows
    /// on screen now, so neither changes as rows of other heights scroll by;
    /// it is replaced by the real count whenever the list is at its end,
    /// which is the one place it has to be exact.
    visible_at_end: f32,
    /// Viewport height `visible_at_end` was taken at.
    view_height: f32,
    /// Track length in virtual pixels, as last given to the scrollbar.
    travel: f32,
    /// Row heights seen on screen, to turn a fraction of a row into pixels
    /// for a row that is not at or below the top any more.
    heights: HashMap<usize, f32>,
    item_count: usize,
}

#[derive(Clone)]
pub struct RowScrollbar {
    list: ListState,
    state: Rc<RefCell<State>>,
}

impl RowScrollbar {
    pub fn new(list: &ListState) -> Self {
        Self {
            list: list.clone(),
            state: Rc::new(RefCell::new(State::default())),
        }
    }

    /// Whether the list has more than one screen to show.
    fn overflows(&self) -> bool {
        self.list.max_offset_for_scrollbar().y.as_f32() > 0.
    }

    /// Where the viewport is, from the rows laid out around it.
    fn position(&self) -> Option<Position> {
        let count = self.list.item_count();
        let viewport = self.list.viewport_bounds();
        let view_height = viewport.size.height.as_f32();
        if count == 0 || view_height <= 0. {
            return None;
        }
        let top = self.list.logical_scroll_top();
        if top.item_ix >= count {
            // Asked to show the end, and not laid out since.
            return Some(Position { top: 0., visible: 0., at_end: true });
        }
        let (view_top, view_bottom) = (viewport.top().as_f32(), viewport.bottom().as_f32());

        let mut state = self.state.borrow_mut();
        if state.item_count != count {
            state.item_count = count;
            state.heights.clear();
        }
        let mut visible = 0.;
        let mut above = 0.;
        let mut at_end = false;
        let mut first_height = None;
        for ix in top.item_ix..count {
            let Some(bounds) = self.list.bounds_for_item(ix) else {
                // Not measured: the rest of the screen is this many rows of
                // the height of the first.
                let used = visible_pixels(top.item_ix, ix, &self.list, view_top, view_bottom);
                let row = first_height.unwrap_or(view_height).max(1.);
                visible += ((view_height - used) / row).max(0.);
                break;
            };
            let height = bounds.size.height.as_f32().max(1.);
            state.heights.insert(ix, height);
            first_height.get_or_insert(height);
            if ix == top.item_ix {
                above = (top.offset_in_item.as_f32() / height).clamp(0., 1.);
            }
            let covered = (bounds.bottom().as_f32().min(view_bottom)
                - bounds.top().as_f32().max(view_top))
            .max(0.);
            visible += covered / height;
            if bounds.bottom().as_f32() >= view_bottom - END_PIXELS {
                at_end = ix + 1 == count && bounds.bottom().as_f32() <= view_bottom + END_PIXELS;
                break;
            }
            if ix + 1 == count {
                at_end = true;
            }
        }
        Some(Position {
            top: top.item_ix as f32 + above,
            visible: visible.min(count as f32),
            at_end,
        })
    }

    /// The last place the top of the viewport can be, in rows.
    fn span(count: usize, visible: f32) -> f32 {
        (count as f32 - visible).max(END_EPSILON)
    }

    /// The rows the viewport shows at the end of the list, kept current.
    fn visible_at_end(&self, position: Option<Position>, count: usize) -> f32 {
        let view_height = self.list.viewport_bounds().size.height.as_f32();
        let mut state = self.state.borrow_mut();
        if (state.view_height - view_height).abs() > 1. {
            state.view_height = view_height;
            state.visible_at_end = 0.;
        }
        if !state.dragging {
            if let Some(p) = position {
                let exact = p.at_end && p.visible > 0.;
                if exact || state.visible_at_end <= 0. {
                    state.visible_at_end = p.visible;
                }
            }
        }
        state.visible_at_end.clamp(END_EPSILON, count as f32)
    }

    /// (virtual content height, thumb place 0..=1) as the scrollbar should see
    /// them now.
    fn geometry(&self) -> (f32, f32) {
        let view_height = self.list.viewport_bounds().size.height.as_f32();
        let count = self.list.item_count();
        if !self.overflows() || count == 0 {
            return (view_height, 0.);
        }
        let position = self.position();
        let visible = self.visible_at_end(position, count);
        let content = (view_height * count as f32 / visible).max(view_height + 1.);
        let (dragging, drag_fraction) = {
            let state = self.state.borrow();
            (state.dragging, state.drag_fraction)
        };
        let fraction = if dragging {
            drag_fraction
        } else {
            match position {
                Some(p) if p.at_end => 1.,
                Some(p) => (p.top / Self::span(count, visible)).clamp(0., 1.),
                None => 0.,
            }
        };
        self.state.borrow_mut().travel = content - view_height;
        (content, fraction)
    }
}

/// Pixels of the viewport covered by rows `from..to`.
fn visible_pixels(
    from: usize,
    to: usize,
    list: &ListState,
    view_top: f32,
    view_bottom: f32,
) -> f32 {
    (from..to)
        .filter_map(|ix| list.bounds_for_item(ix))
        .map(|b| (b.bottom().as_f32().min(view_bottom) - b.top().as_f32().max(view_top)).max(0.))
        .sum()
}

impl ScrollbarHandle for RowScrollbar {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.list.viewport_bounds()
    }

    fn offset(&self) -> Point<Pixels> {
        let (_, fraction) = self.geometry();
        let travel = self.state.borrow().travel;
        point(px(0.), px(-fraction * travel))
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let count = self.list.item_count();
        let travel = self.state.borrow().travel;
        if count == 0 || travel <= 0. {
            return;
        }
        let mut fraction = (-offset.y.as_f32() / travel).clamp(0., 1.);
        // The scrollbar ignores a move of under a pixel, so a drag can stop
        // that far short of either end of the track: that is the end.
        if fraction * travel <= SNAP_PIXELS {
            fraction = 0.;
        } else if (1. - fraction) * travel <= SNAP_PIXELS {
            fraction = 1.;
        }
        if self.state.borrow().dragging {
            self.state.borrow_mut().drag_fraction = fraction;
        }
        let visible = self.visible_at_end(self.position(), count);
        if fraction >= 1. {
            self.list.scroll_to_end();
            return;
        }
        let top = fraction * Self::span(count, visible);
        let item_ix = (top.floor() as usize).min(count - 1);
        let within = (top - item_ix as f32).clamp(0., 1.);
        let height = self
            .list
            .bounds_for_item(item_ix)
            .map(|b| b.size.height.as_f32())
            .or_else(|| self.state.borrow().heights.get(&item_ix).copied())
            .unwrap_or(0.);
        self.list.scroll_to(ListOffset {
            item_ix,
            offset_in_item: px(within * height),
        });
    }

    fn content_size(&self) -> Size<Pixels> {
        let width = self.list.viewport_bounds().size.width;
        let (content, _) = self.geometry();
        size(width, px(content))
    }

    fn start_drag(&self) {
        let (_, fraction) = self.geometry();
        let mut state = self.state.borrow_mut();
        state.dragging = true;
        state.drag_fraction = fraction;
    }

    fn end_drag(&self) {
        let at_end = {
            let mut state = self.state.borrow_mut();
            state.dragging = false;
            state.drag_fraction >= 1.
        };
        // Rows measured during the drag may have moved the end: ask again.
        if at_end {
            self.list.scroll_to_end();
        }
    }
}
