//! Implementations of the entries in `doc/ui-style-guide.yaml`.
//!
//! The guide is the source of truth. Each style, state, and token used by the
//! UI is implemented here exactly once, named after its guide entry
//! (`text-muted` becomes [`text_muted`]). Views apply these instead of raw
//! colors, sizes, or spacing. Values come only from the guide's `tokens`.
//!
//! Only the entries some view needs are implemented; add others here, under
//! their guide names, as views start to use them.

use gpui::{
    AnyElement, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, Styled,
    div, px, rgba,
};

/// `tokens.color`.
pub mod color {
    use super::*;

    fn hex(value: u32) -> Hsla {
        rgba(value).into()
    }

    pub fn text() -> Hsla {
        hex(0xfafafaff)
    }
    pub fn text_muted() -> Hsla {
        hex(0xa3a3a3ff)
    }
    pub fn divider() -> Hsla {
        hex(0x262626ff)
    }
    pub fn badge_fill() -> Hsla {
        hex(0x262626ff)
    }
    pub fn badge_edge() -> Hsla {
        hex(0xa3a3a38c)
    }
    pub fn highlight() -> Hsla {
        hex(0x1e40af33)
    }
    pub fn highlight_edge() -> Hsla {
        hex(0x1d4ed8ff)
    }
    // Pane styles: no pane uses them yet (gpui-component draws its own
    // resizable handles).
    #[allow(dead_code)]
    pub fn drag_edge() -> Hsla {
        hex(0xfafafaa6)
    }
    pub fn danger() -> Hsla {
        hex(0xf87171ff)
    }
    pub fn link() -> Hsla {
        hex(0xfafafaff)
    }
    pub fn toggle_on_fill() -> Hsla {
        hex(0x1d4ed8ff)
    }
    pub fn toggle_on_edge() -> Hsla {
        hex(0x93c5fdff)
    }
    pub fn control_edge() -> Hsla {
        hex(0x2f2f2fff)
    }
    pub fn accent() -> Hsla {
        hex(0x1d4ed88c)
    }
    pub fn surface() -> Hsla {
        hex(0x0a0a0aff)
    }
    pub fn scrim() -> Hsla {
        hex(0x00000073)
    }
    pub fn stale_fill() -> Hsla {
        hex(0xf9731626)
    }
    pub fn stale_edge() -> Hsla {
        hex(0xf97316ff)
    }
    pub fn stale_text() -> Hsla {
        hex(0xfdba74ff)
    }
    pub fn incoming_text() -> Hsla {
        hex(0xfacc15ff)
    }
    pub fn divider_strong() -> Hsla {
        hex(0xa3a3a380)
    }
    pub fn group_band() -> Hsla {
        hex(0x26262680)
    }
    pub fn status_done() -> Hsla {
        hex(0x4ade80ff)
    }
    pub fn status_active() -> Hsla {
        hex(0xfbbf24ff)
    }
    pub fn status_blocked() -> Hsla {
        hex(0xf87171ff)
    }
    pub fn status_ready() -> Hsla {
        hex(0xfafafaff)
    }
    pub fn status_idle() -> Hsla {
        hex(0xa3a3a3ff)
    }
}

/// What a status says at a glance (`tokens.color.status-*`). A status with
/// nothing to say — not started, not applicable — is [`StatusTone::Idle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusTone {
    Done,
    Active,
    Blocked,
    Ready,
    Idle,
}

impl StatusTone {
    fn color(self) -> Hsla {
        match self {
            Self::Done => color::status_done(),
            Self::Active => color::status_active(),
            Self::Blocked => color::status_blocked(),
            Self::Ready => color::status_ready(),
            Self::Idle => color::status_idle(),
        }
    }
}

/// `styles.status-chip`: an item's status, tinted by what it says.
pub fn status_chip<E: Styled>(el: E, tone: StatusTone) -> E {
    let color = tone.color();
    el.text_size(font::DENSE)
        .text_color(color)
        .bg(color.opacity(0.15))
        .rounded(radius::BADGE)
        .px(space::SNUG)
        .py(space::HAIRLINE)
        .whitespace_nowrap()
        .flex_shrink_0()
}

/// `tokens.font` sizes (weights are applied by the styles that use them).
pub mod font {
    use super::*;

    pub const BODY: Pixels = px(14.);
    pub const DENSE: Pixels = px(12.);
}

/// `tokens.space`.
pub mod space {
    use super::*;

    pub const HAIRLINE: Pixels = px(2.);
    pub const INLINE: Pixels = px(4.);
    pub const SNUG: Pixels = px(6.);
    pub const RELATED: Pixels = px(8.);
    pub const INSET: Pixels = px(12.);
    pub const SECTION: Pixels = px(16.);
}

/// `tokens.size`.
pub mod size {
    use super::*;

    pub const BORDER: Pixels = px(1.);
    pub const PANE_MIN: Pixels = px(320.);
    pub const CONTROL_XSMALL: Pixels = px(20.);
    pub const GROUP_ROW: Pixels = px(28.);
    pub const GROUP_INDENT: Pixels = px(16.);
    pub const SUMMARY_LIST_MAX: Pixels = px(240.);
}

/// `tokens.radius`.
pub mod radius {
    use super::*;

    pub const BADGE: Pixels = px(2.);
    pub const CONTROL: Pixels = px(6.);
    pub const FLOATING: Pixels = px(8.);
}

/// `styles.button-toggle`: a small outlined button; when `on` it takes the
/// `toggle-on` state (solid accent fill, bright edge, semibold text).
pub fn button_toggle<E: Styled>(el: E, on: bool) -> E {
    let el = el.border_1().border_color(color::control_edge());
    if on {
        el.bg(color::toggle_on_fill())
            .border_color(color::toggle_on_edge())
            .text_color(color::text())
            .font_weight(FontWeight::SEMIBOLD)
    } else {
        el.text_color(color::text_muted())
    }
}

/// `styles.text`: body font in the text color, wrapping.
pub fn text<E: Styled>(el: E) -> E {
    el.text_size(font::BODY)
        .font_weight(FontWeight::NORMAL)
        .text_color(color::text())
}

/// `styles.text-muted`.
pub fn text_muted<E: Styled>(el: E) -> E {
    text(el).text_color(color::text_muted())
}

/// `styles.text-title`: semibold, one line, truncated at the end.
pub fn text_title<E: Styled>(el: E) -> E {
    text(el)
        .font_weight(FontWeight::SEMIBOLD)
        .whitespace_nowrap()
        .text_ellipsis()
        .overflow_hidden()
}

/// `styles.text-dense`.
pub fn text_dense<E: Styled>(el: E) -> E {
    text(el).text_size(font::DENSE)
}

/// `styles.text-dense-muted`.
pub fn text_dense_muted<E: Styled>(el: E) -> E {
    text_dense(el).text_color(color::text_muted())
}

/// `styles.text-dense-label`: a dense label over the text it introduces.
pub fn text_dense_label<E: Styled>(el: E) -> E {
    text_dense_muted(el).font_weight(FontWeight::SEMIBOLD)
}

/// `styles.text-error`.
pub fn text_error<E: Styled>(el: E) -> E {
    text_dense(el).text_color(color::danger())
}

/// `styles.text-link`.
pub fn text_link<E: Styled>(el: E) -> E {
    text(el).text_color(color::link())
}

/// `styles.empty-message`: muted text, centered.
pub fn empty_message<E: Styled>(el: E) -> E {
    text_muted(el).text_center()
}

/// `styles.badge`. Single line: no wrapping, no ellipsis.
pub fn badge<E: Styled>(el: E) -> E {
    el.text_size(font::DENSE)
        .font_weight(FontWeight::MEDIUM)
        .text_color(color::text())
        .bg(color::badge_fill())
        .border(size::BORDER)
        .border_color(color::badge_edge())
        .rounded(radius::BADGE)
        .px(space::SNUG)
        .py(space::HAIRLINE)
        .whitespace_nowrap()
        .flex_shrink_0()
}

/// `styles.list-group`: a heading over a run of item rows. `depth` is the
/// grouping level, outermost 0: the outermost reads as a band across the
/// list, the ones inside it as progressively lighter headings, each indented
/// one step further than the one above.
pub fn list_group<E: Styled>(el: E, depth: usize) -> E {
    let el = el
        .h(size::GROUP_ROW)
        .px(space::RELATED)
        .pl(space::RELATED + size::GROUP_INDENT * depth as f32)
        .gap(space::INLINE)
        .text_size(font::BODY)
        .border_b(size::BORDER)
        .border_color(color::divider_strong());
    match depth {
        0 => el.font_weight(FontWeight::BOLD).bg(color::group_band()),
        1 => el.font_weight(FontWeight::SEMIBOLD),
        _ => el.font_weight(FontWeight::MEDIUM),
    }
}

/// `styles.list-group-chevron`: the collapse toggle in a heading's leading
/// gutter.
pub fn list_group_chevron<E: Styled>(el: E) -> E {
    text_dense_muted(el).w(size::CONTROL_XSMALL).flex_shrink_0()
}

/// `styles.panel`.
pub fn panel<E: Styled>(el: E) -> E {
    el.bg(color::surface())
}

/// `styles.callout-stale`: an orange-edged block for a state that no longer
/// holds and needs the user to act.
pub fn callout_stale<E: Styled>(el: E) -> E {
    el.flex()
        .flex_col()
        .gap(space::RELATED)
        .px(space::INSET)
        .py(space::RELATED)
        .rounded(radius::CONTROL)
        .border(size::BORDER)
        .border_color(color::stale_edge())
        .bg(color::stale_fill())
        .text_size(font::DENSE)
        .text_color(color::stale_text())
}

/// `styles.callout-stale-title`.
pub fn callout_stale_title<E: Styled>(el: E) -> E {
    el.text_size(font::BODY)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color::stale_text())
}

/// `styles.node-title-pending-changes`: a tree row title whose node has
/// incoming changes it has not been checked against.
pub fn node_title_pending_changes<E: Styled>(el: E) -> E {
    text_title(el).text_color(color::incoming_text())
}

/// `styles.scrim`.
pub fn scrim<E: Styled>(el: E) -> E {
    el.bg(color::scrim())
}

/// A floating panel: `panel` with the `toast` frame (divider border,
/// floating radius, section/inset padding).
pub fn floating_panel<E: Styled>(el: E) -> E {
    panel(el)
        .border(size::BORDER)
        .border_color(color::divider())
        .rounded(radius::FLOATING)
        .px(space::SECTION)
        .py(space::INSET)
        .gap(space::RELATED)
}

/// `styles.pane`. Unused until a pane needs it (see `color::drag_edge`).
#[allow(dead_code)]
pub fn pane<E: Styled>(el: E) -> E {
    el.min_w(size::PANE_MIN)
}

/// `styles.pane-divider`, in its `dragging` state while `dragging`.
#[allow(dead_code)]
pub fn pane_divider<E: Styled>(el: E, dragging: bool) -> E {
    if dragging {
        dragging_state(el)
    } else {
        el.bg(color::divider())
    }
}

/// `styles.panel-header`.
pub fn panel_header<E: Styled>(el: E) -> E {
    el.px(space::INSET)
        .py(space::RELATED)
        .gap(space::RELATED)
        .border_b(size::BORDER)
        .border_color(color::divider())
}

/// `styles.panel-footer`.
pub fn panel_footer<E: Styled>(el: E) -> E {
    el.px(space::INSET)
        .py(space::RELATED)
        .gap(space::RELATED)
        .border_t(size::BORDER)
        .border_color(color::divider())
}

/// `styles.row`: body font, control radius, related/inline padding, inline
/// gap, text truncated at the end, and the `hover-row` state. Apply
/// [`highlighted`] on top for the highlighted row.
pub fn row<E: Styled + InteractiveElement>(el: E) -> E {
    row_base(el)
        .whitespace_nowrap()
        .text_ellipsis()
        .overflow_hidden()
}

/// `styles.row-wrapped`: [`row`] with its text wrapping, so the row grows to
/// show all of it. Not built on [`row`]: GPUI truncates any text under an
/// ellipsis, wrapped or not, and a child cannot unset it.
pub fn row_wrapped<E: Styled + InteractiveElement>(el: E) -> E {
    row_base(el).whitespace_normal()
}

fn row_base<E: Styled + InteractiveElement>(el: E) -> E {
    hover_row(
        text(el)
            .rounded(radius::CONTROL)
            .px(space::RELATED)
            .py(space::INLINE)
            .gap(space::INLINE),
    )
}

/// `styles.chunk`: a bordered block that expands and collapses under a
/// [`chunk_header`].
pub fn chunk<E: Styled>(el: E) -> E {
    el.w_full()
        .border(size::BORDER)
        .border_color(color::divider())
        .rounded(radius::CONTROL)
}

/// `styles.chunk-header`: one line, truncated, with the `hover-row` state.
/// Apply [`highlighted`] on top for the highlighted chunk.
pub fn chunk_header<E: Styled + InteractiveElement>(el: E) -> E {
    hover_row(
        text_dense(el)
            .rounded(radius::CONTROL)
            .px(space::RELATED)
            .py(space::INLINE)
            .gap(space::INLINE)
            .items_center()
            .cursor_pointer()
            .whitespace_nowrap()
            .overflow_hidden(),
    )
}

/// `styles.chunk-label`.
pub fn chunk_label<E: Styled>(el: E) -> E {
    text_dense_muted(el)
}

/// `styles.chunk-body`.
pub fn chunk_body<E: Styled>(el: E) -> E {
    el.w_full().px(space::RELATED).pb(space::RELATED)
}

/// `styles.chunk-children`: the chunks inside another chunk, indented.
pub fn chunk_children<E: Styled>(el: E) -> E {
    el.w_full().pl(space::SECTION)
}

/// `styles.menu-item`: a row whose hover and highlighted states are both
/// `hover-menu`.
pub fn menu_item<E: Styled + InteractiveElement>(el: E, highlighted: bool) -> E {
    let el = text(el)
        .rounded(radius::CONTROL)
        .px(space::RELATED)
        .py(space::INLINE)
        .gap(space::INLINE)
        .whitespace_nowrap()
        .text_ellipsis()
        .overflow_hidden()
        .hover(|style| style.bg(color::accent()));
    if highlighted {
        el.bg(color::accent())
    } else {
        el
    }
}

/// `states.hover-row`.
pub fn hover_row<E: InteractiveElement>(el: E) -> E {
    el.hover(|style| style.bg(color::highlight()))
}

/// `states.highlighted`: the highlight background plus a highlight-edge
/// border. The edge is drawn as an overlay so it takes no layout space: a row
/// does not shift when it becomes highlighted. Both guide entries with this
/// state (`row`, `field`) have the control radius, so it is applied here too.
pub fn highlighted<E: Styled + ParentElement>(el: E) -> E {
    el.relative()
        .rounded(radius::CONTROL)
        .bg(color::highlight())
        .child(highlight_edge())
}

fn highlight_edge() -> AnyElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .border(size::BORDER)
        .border_color(color::highlight_edge())
        .rounded(radius::CONTROL)
        .into_any_element()
}

/// `states.dragging`.
#[allow(dead_code)]
fn dragging_state<E: Styled>(el: E) -> E {
    el.bg(color::drag_edge())
}
