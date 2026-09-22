//! One code review finding, as a row.
//!
//! A review conversation's agent records the finding; the user answers it from
//! the status chip, which opens the same dropdown a plan step's does. The
//! The list declares the columns (see [`finding_columns`]); the row only
//! says which one each value goes in, so severity, status and text line up
//! down the list and under the header naming them.

use super::status_menu::{StatusMenu, StatusMenuHandlers, status_chip};
use super::{RowHost, RowOptions, row_group, row_tail};
use crate::ui::item_list::{ColumnSpec, column_cell};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use gpui::{
    AnyElement, App, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Styled,
    Window, div, prelude::FluentBuilder, px,
};
use gpui_component::{h_flex, v_flex};
use std::rc::Rc;
use tod_store::review::{
    FINDING_DECLINED, FINDING_FIXED, FINDING_OPEN, FINDING_OUT_OF_SCOPE, FINDING_REJECTED,
    ReviewFinding,
};
use uuid::Uuid;

/// The status column's width, which the requirements table beside the
/// findings shares so the two line up. It goes once the requirements table is
/// on the item list too.
pub const STATUS_COLUMN_WIDTH: Pixels = px(120.);

pub const COLUMN_SEVERITY: &str = "severity";
pub const COLUMN_STATUS: &str = "status";
pub const COLUMN_FINDING: &str = "finding";

/// The columns a findings list is a table of. Every finding has all three,
/// which is what makes them columns rather than trailing context.
pub fn finding_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::fixed(COLUMN_SEVERITY, COLUMN_SEVERITY, px(64.)),
        ColumnSpec::fixed(COLUMN_STATUS, "answer", STATUS_COLUMN_WIDTH),
        ColumnSpec::content(COLUMN_FINDING, COLUMN_FINDING),
    ]
}

/// What the user did on a finding row. A host's action type converts from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingRowEvent {
    /// Clicked the row; `row_ix` is the index the host rendered it at.
    Select { row_ix: usize },
    /// Clicked the status chip: open the dropdown on this finding, or close it
    /// when it is already open there.
    ToggleStatusMenu { finding_id: Uuid },
    /// Picked an answer from the open dropdown.
    ChooseStatus {
        finding_id: Uuid,
        status: &'static str,
    },
    /// Clicked away from the open dropdown.
    DismissStatusMenu,
}

pub struct FindingRowProps<'a> {
    pub finding: &'a ReviewFinding,
    /// The row's index in its host, reported back on select and used to key
    /// its elements.
    pub row_ix: usize,
    pub highlighted: bool,
    /// The status dropdown, whichever finding it is open on; the row shows it
    /// only when it is open on this one.
    pub status_menu: Option<StatusMenu>,
    /// The list's columns, so this row lines up with the others.
    pub columns: &'a [ColumnSpec],
}

/// Render one review finding: how much it matters, the answer it has, and
/// what it says.
pub fn finding_row<A: From<FindingRowEvent> + 'static>(
    props: FindingRowProps<'_>,
    host: &RowHost<A>,
    mut opts: RowOptions,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let FindingRowProps {
        finding,
        row_ix,
        highlighted,
        status_menu,
        columns,
    } = props;
    let id = finding.id;
    let key = id.to_string();
    let group = row_group(&key);
    let hoverable = opts.hoverable();

    let severity = style::badge(div())
        .flex_shrink_0()
        .child(finding.severity.clone());
    let severity = if finding.severity == "high" {
        style::text_error(severity)
    } else {
        severity
    };

    let chip = status_chip(
        format!("finding-status-{key}"),
        &finding.status,
        finding_tone(&finding.status),
        status_menu.filter(|menu| menu.is_on(id)),
        StatusMenuHandlers {
            toggle: {
                let host = host.clone();
                Rc::new(move |_, cx| {
                    host.push(
                        FindingRowEvent::ToggleStatusMenu { finding_id: id }.into(),
                        cx,
                    )
                })
            },
            choose: {
                let host = host.clone();
                Rc::new(move |status, _, cx| {
                    host.push(
                        FindingRowEvent::ChooseStatus {
                            finding_id: id,
                            status,
                        }
                        .into(),
                        cx,
                    )
                })
            },
            dismiss: {
                let host = host.clone();
                Rc::new(move |_, cx| host.push(FindingRowEvent::DismissStatusMenu.into(), cx))
            },
        },
        cx,
    );

    let mut body = column_cell(columns, COLUMN_FINDING, v_flex()).gap(style::space::HAIRLINE)
        .child(
            div()
                .min_w_0()
                .when(opts.struck, |el| el.line_through())
                .child(selectable_text(
                    format!("review-finding-{id}"),
                    finding.summary.clone(),
                    window,
                    cx,
                )),
        );
    if let Some(location) = finding.location() {
        body = body.child(style::text_dense_muted(div()).child(selectable_text(
            format!("review-finding-location-{id}"),
            location,
            window,
            cx,
        )));
    }
    if let Some(detail) = &finding.detail {
        body = body.child(style::text_dense_muted(div()).child(selectable_text(
            format!("review-finding-detail-{id}"),
            detail.clone(),
            window,
            cx,
        )));
    }
    if let Some(response) = &finding.response {
        body = body.child(
            v_flex()
                .pt(style::space::HAIRLINE)
                .child(style::text_dense_label(div()).child(response_label(&finding.status)))
                .child(style::text_dense(div()).child(selectable_text(
                    format!("review-finding-response-{id}"),
                    response.clone(),
                    window,
                    cx,
                ))),
        );
    }
    let body = body.children(opts.detail.take());

    let select_host = host.clone();
    let row = h_flex()
        .w_full()
        .flex_shrink_0()
        .gap(style::space::INLINE)
        .px(style::space::RELATED)
        .py(style::space::INLINE)
        .items_start()
        .group(group.clone())
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            select_host.push(FindingRowEvent::Select { row_ix }.into(), cx);
        });
    let row = if hoverable { style::hover_row(row) } else { row };
    row.when(highlighted, style::highlighted)
        .children(opts.leading.take())
        .child(column_cell(columns, COLUMN_SEVERITY, h_flex()).child(severity))
        .child(column_cell(columns, COLUMN_STATUS, h_flex()).child(chip))
        .child(body)
        .children(row_tail(&key, &group, highlighted, &mut opts))
        .into_any_element()
}

/// What a finding's response is, by the answer it came with.
fn response_label(status: &str) -> &'static str {
    match status {
        FINDING_FIXED => "Fixed",
        FINDING_REJECTED => "Rejected — why it is not a problem",
        FINDING_OUT_OF_SCOPE => "Out of scope",
        FINDING_DECLINED => "Declined",
        _ => "Note",
    }
}

/// What a review finding's status says at a glance. An open finding is still
/// asking for an answer, so it reads as blocking; a rejected one is the
/// user's answer standing against the agent's, so it is neither.
fn finding_tone(status: &str) -> style::StatusTone {
    match status {
        FINDING_OPEN => style::StatusTone::Blocked,
        FINDING_FIXED => style::StatusTone::Done,
        FINDING_REJECTED => style::StatusTone::Active,
        _ => style::StatusTone::Idle,
    }
}
