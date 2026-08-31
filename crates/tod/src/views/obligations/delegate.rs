use std::cell::RefCell;
use std::rc::Rc;

use tod_store::outline::{KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation};
use crate::ui::selectable_text::selectable_text;
use gpui::{
    AnyElement, App, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, StyledExt, h_flex};

pub const GROUP_ROW_HEIGHT: gpui::Pixels = gpui::px(28.0);
pub const NO_SECTION: &str = "<no section>";

pub fn obligation_section(ob: &NodeObligation) -> &str {
    ob.section.as_deref().unwrap_or(NO_SECTION)
}

pub fn section_row_key(kind: &str, section: &str) -> String {
    format!("section:{kind}:{section}")
}

#[derive(Debug, Clone)]
pub enum ObligationRow {
    Group {
        kind: &'static str,
        collapsed: bool,
        count: usize,
    },
    Section {
        kind: &'static str,
        section: String,
        collapsed: bool,
        count: usize,
    },
    Item {
        obligation: NodeObligation,
    },
}

impl ObligationRow {
    pub fn key(&self) -> String {
        match self {
            Self::Group { kind, .. } => format!("group:{kind}"),
            Self::Section { kind, section, .. } => section_row_key(kind, section),
            Self::Item { obligation } => obligation.id.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum RowAction {
    ToggleGroup { kind: String },
    ToggleSection { kind: String, section: String },
    StartEdit { obligation_id: uuid::Uuid },
    Select { row_ix: usize },
}

pub struct ObligationListDelegate {
    rows: Vec<ObligationRow>,
    selected_index: Option<usize>,
    action_sink: Rc<RefCell<Vec<RowAction>>>,
    editing_id: Option<String>,
    inline_edit_input: Option<Entity<InputState>>,
}

impl ObligationListDelegate {
    pub fn new(rows: Vec<ObligationRow>, action_sink: Rc<RefCell<Vec<RowAction>>>) -> Self {
        Self {
            rows,
            selected_index: None,
            action_sink,
            editing_id: None,
            inline_edit_input: None,
        }
    }

    pub fn set_rows(&mut self, rows: Vec<ObligationRow>) {
        self.rows = rows;
    }

    pub fn rows(&self) -> &[ObligationRow] {
        &self.rows
    }

    pub fn set_selected_index(&mut self, ix: Option<usize>) {
        self.selected_index = ix;
    }

    pub fn selected_row(&self) -> Option<&ObligationRow> {
        self.selected_index.and_then(|ix| self.rows.get(ix))
    }

    pub fn set_inline_edit(
        &mut self,
        editing_id: Option<String>,
        inline_edit_input: Entity<InputState>,
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
        let selected = self.selected_index == Some(row_ix);
        let theme = cx.theme();
        let border = theme.muted_foreground.opacity(0.5);
        let sink = self.action_sink.clone();

        let content = match row {
            ObligationRow::Group {
                kind,
                collapsed,
                count,
            } => {
                let label = match kind {
                    KIND_REQUIREMENT => "Requirements",
                    KIND_CONSTRAINT => "Constraints",
                    other => other,
                };
                let kind_owned = kind.to_string();
                let select_sink = sink.clone();
                let toggle_sink = sink.clone();
                h_flex()
                    .h(GROUP_ROW_HEIGHT)
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .border_b_1()
                    .border_color(border)
                    .when(selected, |el| el.bg(theme.muted))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, _, _| {
                        select_sink.borrow_mut().push(RowAction::Select { row_ix });
                    })
                    .child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, move |_, _, _| {
                                toggle_sink.borrow_mut().push(RowAction::ToggleGroup {
                                    kind: kind_owned.clone(),
                                });
                            })
                            .child(if collapsed { "▸" } else { "▾" }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child(format!("{label} ({count})")),
                    )
            }
            ObligationRow::Section {
                kind,
                section,
                collapsed,
                count,
            } => {
                let kind_owned = kind.to_string();
                let section_owned = section.clone();
                let select_sink = sink.clone();
                let toggle_sink = sink.clone();
                h_flex()
                    .h(GROUP_ROW_HEIGHT)
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .pl_5()
                    .border_b_1()
                    .border_color(border)
                    .when(selected, |el| el.bg(theme.muted))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, _, _| {
                        select_sink.borrow_mut().push(RowAction::Select { row_ix });
                    })
                    .child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, move |_, _, _| {
                                toggle_sink.borrow_mut().push(RowAction::ToggleSection {
                                    kind: kind_owned.clone(),
                                    section: section_owned.clone(),
                                });
                            })
                            .child(if collapsed { "▸" } else { "▾" }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .child(format!("{section} ({count})")),
                    )
            }
            ObligationRow::Item { obligation } => {
                let editing = self.editing_id.as_deref() == Some(&obligation.id.to_string());
                let is_empty = obligation.body.is_empty();
                let color = if is_empty {
                    theme.muted_foreground
                } else {
                    theme.foreground
                };
                let select_sink = sink.clone();
                let mut row_el = h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .items_start()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .pl_9()
                    .border_b_1()
                    .border_color(border)
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
                    .relative()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, _, _| {
                        select_sink.borrow_mut().push(RowAction::Select { row_ix });
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .flex_shrink_0()
                            .pt_0p5()
                            .child(format!("{}.", obligation.ordinal)),
                    );
                if editing {
                    if let Some(input) = &self.inline_edit_input {
                        row_el = row_el
                            .child(div().flex_1().min_w_0().child(Input::new(input).w_full()));
                    }
                } else {
                    let id = obligation.id;
                    let body = if is_empty {
                        "(new obligation)".to_string()
                    } else {
                        obligation.body.clone()
                    };
                    let edit_sink = self.action_sink.clone();
                    row_el = row_el.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(selected, |el| {
                                el.on_mouse_down(MouseButton::Left, move |event, _, _| {
                                    if event.click_count >= 2 {
                                        edit_sink
                                            .borrow_mut()
                                            .push(RowAction::StartEdit { obligation_id: id });
                                    }
                                })
                            })
                            .child(obligation_body(row_ix, &body, color, window, cx)),
                    );
                }
                row_el
            }
        };

        Some(
            div()
                .id(("obligation-row", row_ix))
                .w_full()
                .child(content)
                .into_any_element(),
        )
    }
}

fn obligation_body(
    row_ix: usize,
    body: &str,
    color: gpui::Hsla,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let text = SharedString::from(body.to_string());
    selectable_text(("obligation-body", row_ix), text, window, cx)
        .text_sm()
        .text_color(color)
        .whitespace_normal()
        .w_full()
        .min_w_0()
        .into_any_element()
}
