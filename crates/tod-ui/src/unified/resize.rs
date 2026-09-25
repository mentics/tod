//! Dragging the dividers between the unified view's columns.
//!
//! Every divider between two columns can be dragged. A drag trades
//! width between the two columns beside it and leaves every other column
//! alone; the last column has no width of its own and takes whatever is
//! left, so dragging the divider before it only resizes its left neighbor.
//! Opening a column squeezes the others; none is ever folded away.

use gpui::{Pixels, Window, px};

/// Width of a divider's grab area. Only its centre pixel is drawn.
pub const DIVIDER_WIDTH: f32 = 7.;
/// The narrowest a panel column can be dragged to (opening more columns
/// can still squeeze it narrower).
pub const PANEL_MIN_WIDTH: f32 = 220.;
/// The narrowest the node tree can be dragged to.
pub const TREE_MIN_WIDTH: f32 = 200.;
/// How many characters of a row's title the tree starts out wide enough for.
const TREE_START_CHARS: f32 = 80.;
/// A tree row's indent, marker, and padding, beside its title.
const TREE_ROW_CHROME: f32 = 48.;

/// The drag payload: which divider is being dragged. Divider 0 is the one
/// right of the node tree; divider `n` is the one left of column `n`
/// (`UnifiedView::hosted[n]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnDivider(pub usize);

/// A divider drag, fixed when it begins: where the column left of the
/// divider starts, and the two columns' combined width, which the drag only
/// moves between them. `pair` is `None` when the column on the right is the
/// last one, which fills whatever is left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResizeStart {
    pub divider: usize,
    pub left_edge: Pixels,
    pub pair: Option<Pixels>,
}

impl ResizeStart {
    /// The widths that put the divider under the pointer at `x`, neither
    /// column going below its minimum.
    pub fn widths_at(
        &self,
        x: Pixels,
        min_left: Pixels,
        min_right: Pixels,
    ) -> (Pixels, Option<Pixels>) {
        let mut left = (x - self.left_edge - px(DIVIDER_WIDTH / 2.)).max(min_left);
        if let Some(pair) = self.pair {
            left = left.min((pair - min_right).max(min_left));
        }
        (left, self.pair.map(|pair| pair - left))
    }
}

/// Where the node tree starts: the width saved from the last drag, else
/// about 80 characters of the UI font, but never so wide that no panel fits
/// beside it in this window.
pub fn starting_tree_width(window: &Window, saved: Option<Pixels>) -> Pixels {
    let wanted = saved.unwrap_or_else(|| {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let font_id = window.text_system().resolve_font(&style.font());
        let ch = window
            .text_system()
            .ch_width(font_id, font_size)
            .unwrap_or(font_size * 0.6);
        ch * TREE_START_CHARS + px(TREE_ROW_CHROME)
    });
    let viewport = window.viewport_size().width;
    let fits = viewport - px(PANEL_MIN_WIDTH + DIVIDER_WIDTH);
    if fits > px(0.) {
        wanted.min(fits).max(px(TREE_MIN_WIDTH))
    } else {
        wanted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(pair: Option<f32>) -> ResizeStart {
        ResizeStart {
            divider: 1,
            left_edge: px(100.),
            pair: pair.map(px),
        }
    }

    #[test]
    fn the_divider_follows_the_pointer_and_the_pair_keeps_its_width() {
        let (left, right) = start(Some(700.)).widths_at(px(453.5), px(220.), px(220.));
        assert_eq!((left, right), (px(350.), Some(px(350.))));
        let (left, right) = start(Some(700.)).widths_at(px(363.5), px(220.), px(220.));
        assert_eq!((left, right), (px(260.), Some(px(440.))));
    }

    #[test]
    fn neither_neighbor_goes_below_its_minimum() {
        let (left, right) = start(Some(700.)).widths_at(px(2000.), px(220.), px(220.));
        assert_eq!((left, right), (px(480.), Some(px(220.))));
        let (left, right) = start(Some(700.)).widths_at(px(0.), px(220.), px(220.));
        assert_eq!((left, right), (px(220.), Some(px(480.))));
    }

    #[test]
    fn before_the_last_column_only_the_left_one_is_sized() {
        let (left, right) = start(None).widths_at(px(903.5), px(200.), px(220.));
        assert_eq!((left, right), (px(800.), None));
        let (left, _) = start(None).widths_at(px(0.), px(200.), px(220.));
        assert_eq!(left, px(200.));
    }
}
