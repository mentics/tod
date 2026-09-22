//! One plan step, as a row.

use super::obligation_row::one_line;
use super::status_menu::{StatusMenu, StatusMenuHandlers, status_chip};
use super::{RowHost, RowOptions, row_group, row_tail};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use gpui::{
    AnyElement, App, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, Styled, Window, div, prelude::FluentBuilder,
};
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use std::rc::Rc;
use tod_store::interview::short_id;
use tod_store::outline::PlanStep;
use tod_store::outline::repos::plan_steps::{STATUS_BLOCKED, STATUS_FAILED};
use uuid::Uuid;

/// What the user did on a plan-step row. A host's action type converts
/// from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanStepRowEvent {
    /// Clicked the row; `row_ix` is the index the host rendered it at.
    Select { row_ix: usize },
    /// Double-clicked the highlighted row's text.
    StartEdit { step_id: Uuid },
    /// Clicked the status chip: open the dropdown on this step, or close it
    /// when it is already open there.
    ToggleStatusMenu { step_id: Uuid },
    /// Picked a status from the open dropdown.
    ChooseStatus { step_id: Uuid, status: &'static str },
    /// Clicked away from the open dropdown.
    DismissStatusMenu,
}

pub struct PlanStepRowProps<'a> {
    pub step: &'a PlanStep,
    /// Steps this one depends on.
    pub depends_on: &'a [Uuid],
    /// Obligations this step satisfies.
    pub satisfies: &'a [Uuid],
    /// The row's index in its host, reported back on select and used to key
    /// its elements.
    pub row_ix: usize,
    pub highlighted: bool,
    /// The inline editor, when this row is being edited.
    pub editor: Option<&'a Entity<TextareaState>>,
    /// The status dropdown, when it is open on this step.
    pub status_menu: Option<StatusMenu>,
}

/// Render one plan step. The default [`RowOptions`] give the row the plan
/// list shows; [`RowOptions::compact`] gives a one-line row with the short
/// id and the text only.
pub fn plan_step_row<A: From<PlanStepRowEvent> + 'static>(
    props: PlanStepRowProps<'_>,
    host: &RowHost<A>,
    mut opts: RowOptions,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let PlanStepRowProps {
        step,
        depends_on,
        satisfies,
        row_ix,
        highlighted,
        editor,
        status_menu,
    } = props;
    let id = step.id;
    let key = id.to_string();
    let group = row_group(&key);
    let compact = opts.compact;
    // One line, truncated; a wrapped compact row shows all of its text.
    let one_line_text = compact && !opts.wrap;
    let hoverable = opts.hoverable();
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let divider = muted.opacity(0.5);
    let status_color = status_color(&step.status, theme);
    let is_empty = step.body.is_empty();
    let text_color = if is_empty { muted } else { theme.foreground };

    let body = if editor.is_some() {
        None
    } else {
        let text = if is_empty {
            "(new plan step)".to_string()
        } else if one_line_text {
            one_line(&step.body)
        } else {
            step.body.clone()
        };
        let edit_host = host.clone();
        let text = selectable_text(
            ("plan-step-body", row_ix),
            SharedString::from(text),
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
        Some(
            div()
                .min_w_0()
                .when(compact, |el| el.flex_1())
                .when(one_line_text, |el| el.overflow_hidden())
                .when(!compact, |el| el.w_full())
                .when(opts.struck, |el| el.line_through())
                .when(highlighted, |el| {
                    el.on_mouse_down(MouseButton::Left, move |event, _, cx| {
                        if event.click_count >= 2 {
                            edit_host.push(PlanStepRowEvent::StartEdit { step_id: id }.into(), cx);
                            cx.stop_propagation();
                        }
                    })
                })
                .child(text.into_any_element()),
        )
    };
    let editor = editor.map(|editor| {
        div()
            .min_w_0()
            .when(compact, |el| el.flex_1())
            .when(!compact, |el| el.w_full())
            .child(Textarea::new(editor).w_full())
    });

    let select_host = host.clone();
    let on_select = move |_: &gpui::MouseDownEvent, _: &mut Window, cx: &mut App| {
        select_host.push(PlanStepRowEvent::Select { row_ix }.into(), cx);
    };

    if compact {
        let row = if one_line_text {
            style::row(h_flex()).items_center()
        } else {
            style::row_wrapped(h_flex()).items_start()
        };
        let row = row
            .w_full()
            .flex_shrink_0()
            .group(group.clone())
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, on_select)
            .when(highlighted, style::highlighted)
            .children(opts.leading.take())
            .child(style::text_muted(div()).flex_shrink_0().child(short_id(id)))
            .children(editor)
            .children(body);
        return row
            .children(row_tail(&key, &group, highlighted, &mut opts))
            .into_any_element();
    }

    let header = h_flex()
        .items_center()
        .gap_2()
        .children(opts.leading.take())
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(format!("{}.", step.ordinal)),
        )
        .child(status_chip(
            format!("plan-status-{key}"),
            &step.status,
            status_tone(&step.status),
            status_menu,
            StatusMenuHandlers {
                toggle: {
                    let host = host.clone();
                    Rc::new(move |_, cx| {
                        host.push(
                            PlanStepRowEvent::ToggleStatusMenu { step_id: id }.into(),
                            cx,
                        )
                    })
                },
                choose: {
                    let host = host.clone();
                    Rc::new(move |status, _, cx| {
                        host.push(
                            PlanStepRowEvent::ChooseStatus {
                                step_id: id,
                                status,
                            }
                            .into(),
                            cx,
                        )
                    })
                },
                dismiss: {
                    let host = host.clone();
                    Rc::new(move |_, cx| host.push(PlanStepRowEvent::DismissStatusMenu.into(), cx))
                },
            },
            cx,
        ))
        .child(div().flex_1())
        .children(row_tail(&key, &group, highlighted, &mut opts));

    let links = |label: &str, ids: &[Uuid]| {
        (!ids.is_empty()).then(|| {
            let ids: Vec<String> = ids.iter().map(|id| short_id(*id)).collect();
            div()
                .text_xs()
                .text_color(muted)
                .child(format!("{label}: [{}]", ids.join(", ")))
        })
    };

    let row = v_flex()
        .w_full()
        .flex_shrink_0()
        .gap_0p5()
        .px_2()
        .py_1p5()
        .border_b_1()
        .border_color(divider)
        .group(group)
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_select);
    let row = if hoverable {
        style::hover_row(row)
    } else {
        row
    };
    // Why a partial or blocked step stopped, and how to unblock it.
    let reason = step.reason.as_ref().map(|reason| {
        div()
            .text_xs()
            .text_color(status_color)
            .child(reason.label())
    });
    let note = step.note.as_ref().map(|note| {
        div()
            .text_xs()
            .text_color(status_color)
            .child(selectable_text(
                ("plan-step-note", row_ix),
                SharedString::from(note.clone()),
                window,
                cx,
            ))
    });
    row.when(highlighted, style::highlighted)
        .child(header)
        .children(editor)
        .children(body)
        .children(opts.detail.take())
        .children(reason)
        .children(note)
        .children(links("depends on", depends_on))
        .children(links("satisfies", satisfies))
        .into_any_element()
}

/// What a plan step's status says at a glance.
fn status_tone(status: &str) -> style::StatusTone {
    match status {
        "verified" | "implemented" => style::StatusTone::Done,
        "in_progress" | "partial" => style::StatusTone::Active,
        STATUS_BLOCKED | STATUS_FAILED => style::StatusTone::Blocked,
        "ready" => style::StatusTone::Ready,
        _ => style::StatusTone::Idle,
    }
}

fn status_color(status: &str, theme: &gpui_component::Theme) -> gpui::Hsla {
    match status {
        "verified" | "implemented" => theme.success,
        "in_progress" | "partial" => theme.warning,
        "blocked" | "failed" => theme.danger,
        "ready" => theme.primary,
        _ => theme.muted_foreground,
    }
}
