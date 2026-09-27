//! One pull request, as a row.
//!
//! The list declares the columns ([`pull_request_columns`]); the row says
//! which one each value goes in, so the numbers, states and titles line up
//! under the header naming them. Which branch it merges into, and who opened
//! it, are trailing context inside the content column.

use super::{RowHost, row_action_buttons, row_group};
use crate::ui::item_list::{ColumnSpec, ItemListEvent, ItemRowState};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style::{self, StatusTone};
use gpui::{
    AnyElement, App, InteractiveElement, IntoElement, MouseButton, ParentElement, Styled, Window,
    div, prelude::FluentBuilder, px,
};
use gpui_component::h_flex;
use tod_store::github::{PullState, PullSummary};

pub const COLUMN_NUMBER: &str = "number";
pub const COLUMN_STATE: &str = "state";
pub const COLUMN_TITLE: &str = "pull request";

/// Every pull request has a number, a state and a title, so those are the
/// columns; where it merges and who opened it are context.
pub fn pull_request_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::fixed(COLUMN_NUMBER, "#", px(56.)),
        ColumnSpec::fixed(COLUMN_STATE, COLUMN_STATE, px(72.)),
        ColumnSpec::content(COLUMN_TITLE, COLUMN_TITLE),
    ]
}

/// What a state says at a glance: open work is in progress, a draft not yet
/// asking for anything, merged done, closed unmerged stopped.
pub fn state_tone(state: PullState) -> StatusTone {
    match state {
        PullState::Open => StatusTone::Active,
        PullState::Draft => StatusTone::Idle,
        PullState::Merged => StatusTone::Done,
        PullState::Closed => StatusTone::Blocked,
    }
}

/// `head → base · author`.
pub fn pull_context(pull: &PullSummary) -> String {
    let mut out = format!("{} → {}", pull.head, pull.base);
    if let Some(author) = &pull.author {
        out.push_str(" · ");
        out.push_str(author);
    }
    out
}

/// Render one pull request. Clicking the row selects it; opening it is a row
/// action, so a click never leaves the app.
pub fn pull_request_row<A: From<ItemListEvent> + 'static>(
    pull: &PullSummary,
    state: ItemRowState<'_>,
    host: &RowHost<A>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let group = row_group(state.key);
    let select_host = host.clone();
    let row_ix = state.row_ix;
    let key = state.key.to_string();
    style::row(h_flex())
        .group(group.clone())
        .w_full()
        .items_center()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            select_host.push(ItemListEvent::Select { row_ix }.into(), cx);
        })
        .when(state.highlighted, style::highlighted)
        .child(
            state
                .column(COLUMN_NUMBER, style::text_dense_muted(div()))
                .child(format!("#{}", pull.number)),
        )
        .child(
            state.column(COLUMN_STATE, div()).child(
                style::status_chip(div(), state_tone(pull.state)).child(pull.state.as_str()),
            ),
        )
        .child(
            state
                .column(COLUMN_TITLE, h_flex().min_w_0().items_center().gap(style::space::RELATED))
                // Both give way as the panel narrows, each in proportion to
                // its length, so a long branch name never hides the title.
                .child(
                    div().min_w_0().flex_auto().overflow_hidden().child(selectable_text(
                        format!("pull-title-{key}"),
                        pull.title.clone(),
                        window,
                        cx,
                    )),
                )
                .child(
                    style::text_dense_muted(div())
                        .min_w_0()
                        .flex_initial()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(pull_context(pull)),
                ),
        )
        .children(row_action_buttons(
            state.key,
            &group,
            state.highlighted,
            state.actions.to_vec(),
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pull_request_is_a_table_of_number_state_and_title() {
        let columns = pull_request_columns();
        let keys: Vec<&str> = columns.iter().map(|c| c.key.as_ref()).collect();
        assert_eq!(keys, vec![COLUMN_NUMBER, COLUMN_STATE, COLUMN_TITLE]);
        // Exactly one content column, as the component requires.
        assert_eq!(columns.iter().filter(|c| c.width.is_none()).count(), 1);
        assert_eq!(columns[2].width, None);
    }

    #[test]
    fn context_says_where_it_merges_and_who_opened_it() {
        let mut pull = PullSummary {
            number: 3,
            title: "t".into(),
            url: String::new(),
            state: PullState::Open,
            author: Some("joel".into()),
            head: "tod/x".into(),
            base: "main".into(),
            updated_at: String::new(),
        };
        assert_eq!(pull_context(&pull), "tod/x → main · joel");
        pull.author = None;
        assert_eq!(pull_context(&pull), "tod/x → main");
    }
}
