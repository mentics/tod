//! Copy a column's visible content as Markdown (the header's copy icon).
//!
//! Every piece of dynamic text is drawn through `ui::selectable_text`, which
//! reports what it painted here ([`record`]). A frame starts by clearing the
//! record ([`frame_start`], the first thing the unified view paints), so what
//! is recorded is exactly what was painted last frame: a collapsed row, a
//! folded section, a tab not shown never paints and so is never exported.
//! Each record carries its bounds clipped to the content mask, so text
//! scrolled out of a list's viewport is left out too.
//!
//! [`export`] then takes the records inside one column's bounds, in reading
//! order. Static chrome (button labels, fixed headings) is not dynamic data
//! and does not go through `selectable_text`, so it is not in the export.

use gpui::{
    AnyElement, App, Bounds, Global, IntoElement, Pixels, SharedString, Window, canvas, px,
};

/// What one text view painted last frame.
#[derive(Clone, Debug)]
struct Painted {
    /// Window coordinates, already clipped to the visible part.
    bounds: Bounds<Pixels>,
    text: SharedString,
    /// Markdown source (`selectable_markdown`) rather than plain text.
    markdown: bool,
}

#[derive(Default)]
struct Registry(Vec<Painted>);

impl Global for Registry {}

/// An invisible element that clears the record when painted. Put it ahead of
/// everything whose text should be exportable.
pub fn frame_start() -> AnyElement {
    use gpui::Styled as _;
    canvas(
        |_, _, _| {},
        |_, _, _, cx| cx.default_global::<Registry>().0.clear(),
    )
    .absolute()
    .size_0()
    .into_any_element()
}

/// An invisible element that records `text` as painted where it is placed.
/// It fills its (relatively positioned) parent.
pub fn record(text: SharedString, markdown: bool) -> AnyElement {
    use gpui::Styled as _;
    canvas(
        |_, _, _| {},
        move |bounds, _, window: &mut Window, cx: &mut App| {
            let visible = bounds.intersect(&window.content_mask().bounds);
            if visible.size.width <= px(0.) || visible.size.height <= px(0.) {
                return;
            }
            cx.default_global::<Registry>().0.push(Painted {
                bounds: visible,
                text,
                markdown,
            });
        },
    )
    .absolute()
    // Pinned: with no insets an absolute child sits where it would have
    // flowed, below its siblings in a block parent, outside the parent's
    // clip, and the text would count as not visible.
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

/// Make an element's own text part of the export: `div().text_xs().child(t)`
/// becomes `div().text_xs().child(t.clone()).record_text(t)`. For text that is
/// data and changes (a lifecycle state, a count, a status, an error) but is
/// drawn as a plain child rather than through `selectable_text`. It adds an
/// invisible child filling the element, so layout is unchanged; the element
/// must not itself be absolutely positioned (this makes it `relative`).
pub trait RecordText: gpui::Styled + gpui::ParentElement + Sized {
    fn record_text(self, text: impl Into<SharedString>) -> Self {
        self.relative().child(record(text.into(), false))
    }
}

impl<T: gpui::Styled + gpui::ParentElement> RecordText for T {}

/// The same for an element that cannot take children (a `Button`): wraps it in
/// a `div` that records `text`, the label shown.
pub fn recorded(el: impl IntoElement, text: impl Into<SharedString>) -> gpui::Div {
    use gpui::{ParentElement as _, Styled as _};
    gpui::div()
        .relative()
        .flex_none()
        .child(el)
        .child(record(text.into(), false))
}

/// The Markdown for what `region` showed last frame, under `title` as its
/// heading. `None` when nothing in it was recorded.
pub fn export(title: &str, region: Bounds<Pixels>, cx: &App) -> Option<String> {
    let painted = cx.try_global::<Registry>()?;
    let inside: Vec<Block> = painted
        .0
        .iter()
        .filter(|p| region.intersects(&p.bounds))
        .map(|p| Block {
            top: f32::from(p.bounds.top()),
            bottom: f32::from(p.bounds.bottom()),
            left: f32::from(p.bounds.left()),
            text: p.text.to_string(),
            markdown: p.markdown,
        })
        .collect();
    if inside.is_empty() {
        return None;
    }
    Some(render(title, inside))
}

#[derive(Clone, Debug, PartialEq)]
struct Block {
    top: f32,
    bottom: f32,
    left: f32,
    text: String,
    markdown: bool,
}

/// Reading order: top to bottom, and left to right among blocks that share a
/// line (their vertical extents overlap); those are joined with ` · ` so a
/// label and its value stay together.
fn render(title: &str, mut blocks: Vec<Block>) -> String {
    blocks.sort_by(|a, b| {
        a.top
            .total_cmp(&b.top)
            .then(a.left.total_cmp(&b.left))
    });
    let mut lines: Vec<Vec<Block>> = Vec::new();
    for block in blocks {
        match lines.last_mut() {
            Some(line)
                if line
                    .iter()
                    .all(|b| !b.markdown && b.text.lines().count() <= 1)
                    && !block.markdown
                    && block.text.lines().count() <= 1
                    && overlaps(line, &block) =>
            {
                line.push(block)
            }
            _ => lines.push(vec![block]),
        }
    }
    // A taller block (bigger type) can start a little higher than the text
    // beside it, so the top-to-bottom sort alone puts it first: within a
    // line, read left to right.
    for line in &mut lines {
        line.sort_by(|a, b| a.left.total_cmp(&b.left));
    }
    let mut out = format!("# {}\n", title.trim());
    for line in lines {
        let parts: Vec<String> = line
            .iter()
            .map(|b| b.text.trim_end().to_string())
            .filter(|t| !t.trim().is_empty())
            .collect();
        if parts.is_empty() {
            continue;
        }
        out.push('\n');
        out.push_str(&parts.join(" · "));
        out.push('\n');
    }
    out
}

fn overlaps(line: &[Block], block: &Block) -> bool {
    let (top, bottom) = line
        .iter()
        .fold((f32::MAX, f32::MIN), |(t, b), x| (t.min(x.top), b.max(x.bottom)));
    // More than half the shorter extent shared.
    let shared = bottom.min(block.bottom) - top.max(block.top);
    let shorter = (bottom - top).min(block.bottom - block.top);
    shared > shorter / 2.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(top: f32, left: f32, text: &str, markdown: bool) -> Block {
        Block {
            top,
            bottom: top + 16.,
            left,
            text: text.into(),
            markdown,
        }
    }

    #[test]
    fn reads_top_to_bottom_and_joins_a_line() {
        let out = render(
            "Plan — Ship it",
            vec![
                block(40., 0., "second", false),
                block(0., 100., "value", false),
                block(0., 0., "label", false),
            ],
        );
        assert_eq!(out, "# Plan — Ship it\n\nlabel · value\n\nsecond\n");
    }

    #[test]
    fn a_taller_block_that_starts_higher_still_reads_after_the_one_on_its_left() {
        let mut tall = block(0., 200., "Conflicting requirements", false);
        tall.bottom = 22.;
        let mut small = block(3., 0., "Run the KB cleanup…", false);
        small.bottom = 19.;
        let out = render("T", vec![tall, small]);
        assert_eq!(out, "# T\n\nRun the KB cleanup… · Conflicting requirements\n");
    }

    #[test]
    fn markdown_and_multiline_blocks_stand_alone() {
        let out = render(
            "T",
            vec![
                block(0., 0., "## Heading\n\nbody", true),
                block(0., 100., "beside", false),
            ],
        );
        assert_eq!(out, "# T\n\n## Heading\n\nbody\n\nbeside\n");
    }

    #[test]
    fn empty_text_is_skipped() {
        let out = render("T", vec![block(0., 0., "  ", false), block(20., 0., "x", false)]);
        assert_eq!(out, "# T\n\nx\n");
    }
}
