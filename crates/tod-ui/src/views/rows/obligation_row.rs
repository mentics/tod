//! One obligation, as a row.

use super::{RowHost, RowOptions, row_group, row_tail};
use crate::ui::selectable_text::selectable_text_with_menu;
use crate::ui::style;
use gpui::{
    AnyElement, App, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, Styled, Window, div, prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::{ActiveTheme, Sizable as _, h_flex};
use tod_store::interview::{PHASE_DESIGN, short_id};
use tod_store::outline::NodeObligation;
use uuid::Uuid;

/// What the user did on an obligation row. A host's action type converts
/// from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObligationRowEvent {
    /// Clicked the row; `row_ix` is the index the host rendered it at.
    Select { row_ix: usize },
    /// Double-clicked the highlighted row's text.
    StartEdit { obligation_id: Uuid },
    /// Clicked a design-phase obligation's "Design" affordance.
    OpenVisualDesign { obligation_id: Uuid },
}

pub struct ObligationRowProps<'a> {
    pub obligation: &'a NodeObligation,
    /// The row's index in its host, reported back on select and used to key
    /// its elements.
    pub row_ix: usize,
    pub highlighted: bool,
    /// The inline editor, when this row is being edited.
    pub editor: Option<&'a Entity<TextareaState>>,
}

/// Render one obligation. The default [`RowOptions`] give the row the
/// obligations list shows; [`RowOptions::compact`] gives a one-line row
/// that shows the short id in place of the ordinal.
pub fn obligation_row<A: From<ObligationRowEvent> + 'static>(
    props: ObligationRowProps<'_>,
    host: &RowHost<A>,
    mut opts: RowOptions,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let ObligationRowProps {
        obligation,
        row_ix,
        highlighted,
        editor,
    } = props;
    let id = obligation.id;
    let key = id.to_string();
    let group = row_group(&key);
    let compact = opts.compact;
    // One line, truncated; a wrapped compact row shows all of its text.
    let one_line_text = compact && !opts.wrap;
    let hoverable = opts.hoverable();
    let muted = cx.theme().muted_foreground;
    let divider = muted.opacity(0.5);
    let is_empty = obligation.body.is_empty();
    let text_color = if is_empty {
        muted
    } else {
        cx.theme().foreground
    };

    let select_host = host.clone();
    let mut row = h_flex()
        .w_full()
        .flex_shrink_0()
        .group(group.clone())
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            select_host.push(ObligationRowEvent::Select { row_ix }.into(), cx);
        });
    row = if one_line_text {
        style::row(row).items_center()
    } else if compact {
        style::row_wrapped(row).items_start()
    } else {
        let row = row
            .items_start()
            .gap_2()
            .px_2()
            .py_1p5()
            .pl_12()
            .border_b_1()
            .border_color(divider);
        if hoverable {
            style::hover_row(row)
        } else {
            row
        }
    };
    row = row.when(highlighted, style::highlighted);
    row = row.children(opts.leading.take());

    let label = if compact {
        short_id(id)
    } else {
        format!("{}.", obligation.ordinal)
    };
    row = row.child(if compact {
        style::text_muted(div()).flex_shrink_0().child(label)
    } else {
        div()
            .text_xs()
            .text_color(muted)
            .flex_shrink_0()
            .pt_0p5()
            .child(label)
    });

    if let Some(editor) = editor {
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .child(Textarea::new(editor).w_full()),
        );
    } else {
        let body = if is_empty {
            "(new obligation)".to_string()
        } else if one_line_text {
            one_line(&obligation.body)
        } else {
            obligation.body.clone()
        };
        let edit_host = host.clone();
        let text = selectable_text_with_menu(
            ("obligation-body", row_ix),
            SharedString::from(body),
            !opts.menu_hosted,
            window,
            cx,
        )
        .text_sm()
        .text_color(text_color)
        .w_full()
        .min_w_0();
        let text = if one_line_text {
            text.whitespace_nowrap().text_ellipsis().overflow_hidden()
        } else {
            text.whitespace_normal()
        };
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .when(one_line_text, |el| el.overflow_hidden())
                .when(opts.struck, |el| el.line_through())
                .when(highlighted, |el| {
                    el.on_mouse_down(MouseButton::Left, move |event, _, cx| {
                        if event.click_count >= 2 {
                            edit_host.push(
                                ObligationRowEvent::StartEdit { obligation_id: id }.into(),
                                cx,
                            );
                            cx.stop_propagation();
                        }
                    })
                })
                .child(text.into_any_element()),
        );
        if !compact && obligation.phase == PHASE_DESIGN {
            let has_design = obligation.visual_design_path.is_some();
            let design_host = host.clone();
            row = row.child(
                Button::new(("obligation-visual-design", row_ix))
                    .label(if has_design { "Design" } else { "+ Design" })
                    .ghost()
                    .xsmall()
                    .flex_shrink_0()
                    .on_click(move |_, _, cx| {
                        design_host.push(
                            ObligationRowEvent::OpenVisualDesign { obligation_id: id }.into(),
                            cx,
                        );
                    }),
            );
        }
    }

    row.children(row_tail(&key, &group, highlighted, &mut opts))
        .into_any_element()
}

/// `text` on one line: runs of whitespace, newlines included, become one
/// space.
pub(super) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::one_line;

    #[test]
    fn compact_text_is_one_line() {
        assert_eq!(one_line("a\n  b\r\n\tc "), "a b c");
    }
}
