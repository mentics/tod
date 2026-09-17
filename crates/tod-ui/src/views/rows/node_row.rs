//! One outline node, as a compact change-set row.

use super::obligation_row::one_line;
use super::{RowHost, RowOptions, row_group, row_tail};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use gpui::{
    AnyElement, App, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, Styled, Window, div, prelude::FluentBuilder,
};
use gpui_component::h_flex;
use gpui_component::input::{Textarea, TextareaState};
use tod_store::interview::short_id;
use uuid::Uuid;

/// What the user did on a node row. A host's action type converts from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeRowEvent {
    /// Clicked the row; `row_ix` is the index the host rendered it at.
    Select { row_ix: usize },
    /// Double-clicked the highlighted row's title.
    StartEdit { node_id: Uuid },
}

pub struct NodeRowProps<'a> {
    pub node_id: Uuid,
    pub title: &'a str,
    /// The row's index in its host, reported back on select and used to key
    /// its elements.
    pub row_ix: usize,
    pub highlighted: bool,
    /// The inline editor, when this row is being edited.
    pub editor: Option<&'a Entity<TextareaState>>,
}

/// Render one node as a one-line row: the short id, then the title. Nodes
/// only appear as rows in the change set, so there is no full-size form.
pub fn node_row<A: From<NodeRowEvent> + 'static>(
    props: NodeRowProps<'_>,
    host: &RowHost<A>,
    mut opts: RowOptions,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let NodeRowProps {
        node_id,
        title,
        row_ix,
        highlighted,
        editor,
    } = props;
    let key = node_id.to_string();
    let group = row_group(&key);

    let select_host = host.clone();
    let mut row = style::row(h_flex())
        .w_full()
        .flex_shrink_0()
        .items_center()
        .group(group.clone())
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            select_host.push(NodeRowEvent::Select { row_ix }.into(), cx);
        })
        .when(highlighted, style::highlighted)
        .children(opts.leading.take())
        .child(
            style::text_muted(div())
                .flex_shrink_0()
                .child(short_id(node_id)),
        );

    if let Some(editor) = editor {
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .child(Textarea::new(editor).w_full()),
        );
    } else {
        let edit_host = host.clone();
        let text = selectable_text(
            ("node-title", row_ix),
            SharedString::from(one_line(title)),
            window,
            cx,
        )
        .w_full()
        .min_w_0()
        .whitespace_nowrap()
        .text_ellipsis()
        .overflow_hidden();
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .when(opts.struck, |el| el.line_through())
                .when(highlighted, |el| {
                    el.on_mouse_down(MouseButton::Left, move |event, _, cx| {
                        if event.click_count >= 2 {
                            edit_host.push(NodeRowEvent::StartEdit { node_id }.into(), cx);
                            cx.stop_propagation();
                        }
                    })
                })
                .child(text.into_any_element()),
        );
    }

    row.children(row_tail(&key, &group, highlighted, &mut opts))
        .into_any_element()
}
