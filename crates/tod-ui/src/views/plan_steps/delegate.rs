use std::cell::RefCell;
use std::rc::Rc;

use crate::ui::selectable_text::selectable_text;
use gpui::{
    AnyElement, App, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, Styled, WeakEntity, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use tod_store::interview::short_id;
use tod_store::outline::PlanStep;
use uuid::Uuid;

/// One row's worth of plan-step data plus the (already-resolved) short-id
/// lists for its dependency and satisfies lines.
#[derive(Debug, Clone)]
pub struct PlanStepRow {
    pub step: PlanStep,
    pub depends_on: Vec<Uuid>,
    pub satisfies: Vec<Uuid>,
}

impl PlanStepRow {
    pub fn key(&self) -> String {
        self.step.id.to_string()
    }
}

#[derive(Debug, Clone)]
pub enum RowAction {
    StartEdit { step_id: Uuid },
    Select { row_ix: usize },
}

pub struct PlanStepListDelegate {
    rows: Vec<PlanStepRow>,
    selected_index: Option<usize>,
    action_sink: Rc<RefCell<Vec<RowAction>>>,
    /// Weak handle to the owning view. Row click handlers only get `&App` (no
    /// `Context<PlanStepsView>`), so pushing to `action_sink` alone doesn't
    /// schedule a repaint — nothing would ever drain the queue. Handlers use
    /// this to force one immediately after queuing an action.
    view: WeakEntity<super::PlanStepsView>,
    editing_id: Option<String>,
    inline_edit_input: Option<Entity<TextareaState>>,
}

impl PlanStepListDelegate {
    pub fn new(
        rows: Vec<PlanStepRow>,
        action_sink: Rc<RefCell<Vec<RowAction>>>,
        view: WeakEntity<super::PlanStepsView>,
    ) -> Self {
        Self {
            rows,
            selected_index: None,
            action_sink,
            view,
            editing_id: None,
            inline_edit_input: None,
        }
    }

    pub fn set_rows(&mut self, rows: Vec<PlanStepRow>) {
        self.rows = rows;
    }

    pub fn rows(&self) -> &[PlanStepRow] {
        &self.rows
    }

    pub fn set_selected_index(&mut self, ix: Option<usize>) {
        self.selected_index = ix;
    }

    pub fn selected_row(&self) -> Option<&PlanStepRow> {
        self.selected_index.and_then(|ix| self.rows.get(ix))
    }

    pub fn set_inline_edit(
        &mut self,
        editing_id: Option<String>,
        inline_edit_input: Entity<TextareaState>,
    ) {
        self.editing_id = editing_id;
        self.inline_edit_input = Some(inline_edit_input);
    }

    pub fn render_row(
        &self,
        row_ix: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let row = self.rows.get(row_ix)?.clone();
        let row_key = row.key();
        let selected = self.selected_index == Some(row_ix);
        let theme = cx.theme().clone();
        let border = theme.muted_foreground.opacity(0.5);
        let sink = self.action_sink.clone();
        let view = self.view.clone();
        let editing = self.editing_id.as_deref() == Some(row_key.as_str());

        let select_sink = sink.clone();
        let select_view = view.clone();
        let mut row_el = v_flex()
            .w_full()
            .flex_shrink_0()
            .gap_0p5()
            .px_2()
            .py_1p5()
            .border_b_1()
            .border_color(border)
            .relative()
            .cursor_pointer()
            .when(selected, |el| {
                el.bg(theme.muted).child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(3.))
                        .bg(theme.primary),
                )
            })
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                select_sink.borrow_mut().push(RowAction::Select { row_ix });
                notify(&select_view, cx);
            });

        let status_color = status_color(&row.step.status, &theme);
        let header = h_flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{}.", row.step.ordinal)),
            )
            .child(
                div()
                    .text_xs()
                    .px_1p5()
                    .py_0p5()
                    .rounded_sm()
                    .bg(status_color.opacity(0.15))
                    .text_color(status_color)
                    .child(row.step.status.clone()),
            );

        row_el = row_el.child(header);

        if editing {
            if let Some(input) = &self.inline_edit_input {
                row_el = row_el.child(div().w_full().child(Textarea::new(input).w_full()));
            }
        } else {
            let is_empty = row.step.body.is_empty();
            let color = if is_empty {
                theme.muted_foreground
            } else {
                theme.foreground
            };
            let body = if is_empty {
                "(new plan step)".to_string()
            } else {
                row.step.body.clone()
            };
            let id = row.step.id;
            let edit_sink = self.action_sink.clone();
            let edit_view = view.clone();
            row_el = row_el.child(
                div()
                    .w_full()
                    .when(selected, |el| {
                        el.on_mouse_down(MouseButton::Left, move |event, _, cx| {
                            if event.click_count >= 2 {
                                edit_sink
                                    .borrow_mut()
                                    .push(RowAction::StartEdit { step_id: id });
                                notify(&edit_view, cx);
                                cx.stop_propagation();
                            }
                        })
                    })
                    .child(plan_step_body(row_ix, &body, color, window, cx)),
            );
        }

        if !row.depends_on.is_empty() {
            let ids: Vec<String> = row.depends_on.iter().map(|id| short_id(*id)).collect();
            row_el = row_el.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("depends on: [{}]", ids.join(", "))),
            );
        }
        if !row.satisfies.is_empty() {
            let ids: Vec<String> = row.satisfies.iter().map(|id| short_id(*id)).collect();
            row_el = row_el.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("satisfies: [{}]", ids.join(", "))),
            );
        }

        Some(
            div()
                .id(("plan-step-row", row_ix))
                .w_full()
                .child(row_el)
                .into_any_element(),
        )
    }
}

fn status_color(status: &str, theme: &gpui_component::Theme) -> gpui::Hsla {
    match status {
        "verified" | "implemented" => theme.success,
        "in_progress" => theme.warning,
        "blocked" => theme.danger,
        "ready" => theme.primary,
        _ => theme.muted_foreground,
    }
}

/// Row click handlers only get `&mut App` (no `Context<PlanStepsView>`), so
/// queuing a `RowAction` alone doesn't schedule a repaint. Call this after
/// every push to force one, so `drain_row_actions` runs on the next frame.
fn notify(view: &WeakEntity<super::PlanStepsView>, cx: &mut App) {
    let _ = view.update(cx, |_, cx| cx.notify());
}

fn plan_step_body(
    row_ix: usize,
    body: &str,
    color: gpui::Hsla,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let text = SharedString::from(body.to_string());
    selectable_text(("plan-step-body", row_ix), text, window, cx)
        .text_sm()
        .text_color(color)
        .whitespace_normal()
        .w_full()
        .min_w_0()
        .into_any_element()
}
