use std::cell::RefCell;
use std::rc::Rc;

use crate::ui::drag_payload::ObligationDragPayload;
use crate::ui::selectable_text::selectable_text;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, WeakEntity, Window,
    div, prelude::FluentBuilder, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::{ActiveTheme, Sizable as _, StyledExt, h_flex};
use tod_store::interview::PHASE_DESIGN;
use tod_store::outline::{KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation};

pub const GROUP_ROW_HEIGHT: gpui::Pixels = gpui::px(28.0);
pub const NO_SECTION: &str = "<no section>";
/// Tags the section-name text field so plain Enter commits it (unlike the
/// multi-line obligation-body field, which reserves Enter for newlines).
pub const SECTION_EDIT_TAG: &str = "ObligationsSectionEdit";

pub fn obligation_section(ob: &NodeObligation) -> &str {
    ob.section.as_deref().unwrap_or(NO_SECTION)
}

pub fn phase_row_key(phase: &str) -> String {
    format!("phase:{phase}")
}

pub fn group_row_key(phase: &str, kind: &str) -> String {
    format!("group:{phase}:{kind}")
}

pub fn section_row_key(phase: &str, kind: &str, section: &str) -> String {
    format!("section:{phase}:{kind}:{section}")
}

pub fn new_section_row_key(phase: &str, kind: &str) -> String {
    format!("new-section:{phase}:{kind}")
}

/// Human label for a phase, `Unknown` for the pre-phase-tagging sentinel.
/// `planning` is no longer a valid obligation phase (planning work is tracked
/// as plan steps instead) but a legacy row tagged that way before the split
/// still renders sensibly here.
pub fn phase_label(phase: &str) -> &str {
    use tod_store::interview::{PHASE_DESIGN, PHASE_PLANNING, PHASE_REQUIREMENTS, PHASE_UNKNOWN};
    match phase {
        PHASE_REQUIREMENTS => "Requirements phase",
        PHASE_DESIGN => "Design phase",
        PHASE_PLANNING => "Planning phase (legacy)",
        PHASE_UNKNOWN => "Unknown phase",
        other => other,
    }
}

#[derive(Debug, Clone)]
pub enum ObligationRow {
    Phase {
        phase: String,
        collapsed: bool,
        count: usize,
    },
    Group {
        phase: String,
        kind: &'static str,
        collapsed: bool,
        count: usize,
    },
    Section {
        phase: String,
        kind: &'static str,
        section: String,
        collapsed: bool,
        count: usize,
        /// A transient, not-yet-created section whose name is being typed.
        is_new: bool,
    },
    Item {
        obligation: NodeObligation,
    },
}

impl ObligationRow {
    pub fn key(&self) -> String {
        match self {
            Self::Phase { phase, .. } => phase_row_key(phase),
            Self::Group { phase, kind, .. } => group_row_key(phase, kind),
            Self::Section {
                phase,
                kind,
                section,
                is_new,
                ..
            } => {
                if *is_new {
                    new_section_row_key(phase, kind)
                } else {
                    section_row_key(phase, kind, section)
                }
            }
            Self::Item { obligation } => obligation.id.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum RowAction {
    TogglePhase { phase: String },
    ToggleGroup { phase: String, kind: String },
    ToggleSection { phase: String, kind: String, section: String },
    StartEdit { obligation_id: uuid::Uuid },
    StartSectionEdit { phase: String, kind: String, section: String },
    AddSection { phase: String, kind: String },
    Select { row_ix: usize },
    /// Clicked the design-phase obligation's "Design" affordance — create or
    /// open its associated visual-design mockup.
    OpenVisualDesign { obligation_id: uuid::Uuid },
}

pub struct ObligationListDelegate {
    rows: Vec<ObligationRow>,
    selected_index: Option<usize>,
    action_sink: Rc<RefCell<Vec<RowAction>>>,
    /// Weak handle to the owning view. Row click handlers only get `&App` (no
    /// `Context<ObligationsView>`), so pushing to `action_sink` alone doesn't
    /// schedule a repaint — nothing would ever drain the queue. Handlers use
    /// this to force one immediately after queuing an action.
    view: WeakEntity<super::ObligationsView>,
    editing_id: Option<String>,
    inline_edit_input: Option<Entity<TextareaState>>,
    section_edit_input: Option<Entity<InputState>>,
    /// Provenance by obligation id; `agent` rows get a subtle marker.
    marks: std::collections::HashMap<uuid::Uuid, tod_store::drafting::ObligationMark>,
}

impl ObligationListDelegate {
    pub fn new(
        rows: Vec<ObligationRow>,
        action_sink: Rc<RefCell<Vec<RowAction>>>,
        view: WeakEntity<super::ObligationsView>,
    ) -> Self {
        Self {
            rows,
            selected_index: None,
            action_sink,
            view,
            editing_id: None,
            inline_edit_input: None,
            section_edit_input: None,
            marks: std::collections::HashMap::new(),
        }
    }

    pub fn set_marks(
        &mut self,
        marks: std::collections::HashMap<uuid::Uuid, tod_store::drafting::ObligationMark>,
    ) {
        self.marks = marks;
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
        inline_edit_input: Entity<TextareaState>,
        section_edit_input: Entity<InputState>,
    ) {
        self.editing_id = editing_id;
        self.inline_edit_input = Some(inline_edit_input);
        self.section_edit_input = Some(section_edit_input);
    }

    pub fn render_row(
        &self,
        row_ix: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let row = self.rows.get(row_ix)?.clone();
        let row_key = row.key();
        let drag_id = match &row {
            ObligationRow::Item { obligation } => Some(obligation.id),
            _ => None,
        };
        let selected = self.selected_index == Some(row_ix);
        let theme = cx.theme();
        let border = theme.muted_foreground.opacity(0.5);
        let sink = self.action_sink.clone();
        let view = self.view.clone();

        let content = match row {
            ObligationRow::Phase {
                phase,
                collapsed,
                count,
            } => {
                let phase_owned = phase.clone();
                let select_sink = sink.clone();
                let toggle_sink = sink.clone();
                let select_view = view.clone();
                let toggle_view = view.clone();
                h_flex()
                    .h(GROUP_ROW_HEIGHT)
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .border_b_1()
                    .border_color(border)
                    .bg(theme.secondary.opacity(0.5))
                    .when(selected, |el| el.bg(theme.muted))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        select_sink.borrow_mut().push(RowAction::Select { row_ix });
                        notify(&select_view, cx);
                    })
                    .child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                toggle_sink.borrow_mut().push(RowAction::TogglePhase {
                                    phase: phase_owned.clone(),
                                });
                                notify(&toggle_view, cx);
                                cx.stop_propagation();
                            })
                            .child(if collapsed { "▸" } else { "▾" }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_bold()
                            .child(format!("{} ({count})", phase_label(&phase))),
                    )
            }
            ObligationRow::Group {
                phase,
                kind,
                collapsed,
                count,
            } => {
                let label = match kind {
                    KIND_REQUIREMENT => "Requirements",
                    KIND_CONSTRAINT => "Constraints",
                    other => other,
                };
                let phase_owned = phase.clone();
                let kind_owned = kind.to_string();
                let add_section_phase = phase.clone();
                let add_section_kind = kind.to_string();
                let select_sink = sink.clone();
                let toggle_sink = sink.clone();
                let add_sink = sink.clone();
                let select_view = view.clone();
                let toggle_view = view.clone();
                let add_view = view.clone();
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
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        select_sink.borrow_mut().push(RowAction::Select { row_ix });
                        notify(&select_view, cx);
                    })
                    .child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                toggle_sink.borrow_mut().push(RowAction::ToggleGroup {
                                    phase: phase_owned.clone(),
                                    kind: kind_owned.clone(),
                                });
                                notify(&toggle_view, cx);
                                cx.stop_propagation();
                            })
                            .child(if collapsed { "▸" } else { "▾" }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child(format!("{label} ({count})")),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .px_1()
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                add_sink.borrow_mut().push(RowAction::AddSection {
                                    phase: add_section_phase.clone(),
                                    kind: add_section_kind.clone(),
                                });
                                notify(&add_view, cx);
                                cx.stop_propagation();
                            })
                            .child("+ Section"),
                    )
            }
            ObligationRow::Section {
                phase,
                kind,
                section,
                collapsed,
                count,
                is_new,
            } => {
                let phase_owned = phase.clone();
                let kind_owned = kind.to_string();
                let section_owned = section.clone();
                let select_sink = sink.clone();
                let toggle_sink = sink.clone();
                let select_view = view.clone();
                let toggle_view = view.clone();
                let editing = is_new || self.editing_id.as_deref() == Some(row_key.as_str());
                let mut header = h_flex()
                    .h(GROUP_ROW_HEIGHT)
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .pl_9()
                    .border_b_1()
                    .border_color(border)
                    .when(selected, |el| el.bg(theme.muted))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        select_sink.borrow_mut().push(RowAction::Select { row_ix });
                        notify(&select_view, cx);
                    });
                if !is_new {
                    header = header.child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                toggle_sink.borrow_mut().push(RowAction::ToggleSection {
                                    phase: phase_owned.clone(),
                                    kind: kind_owned.clone(),
                                    section: section_owned.clone(),
                                });
                                notify(&toggle_view, cx);
                                cx.stop_propagation();
                            })
                            .child(if collapsed { "▸" } else { "▾" }),
                    );
                }
                if editing {
                    if let Some(input) = &self.section_edit_input {
                        header = header.child(
                            div()
                                .key_context(SECTION_EDIT_TAG)
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(input).w_full()),
                        );
                    }
                } else {
                    let phase_owned2 = phase.clone();
                    let kind_owned2 = kind.to_string();
                    let section_owned2 = section.clone();
                    let edit_sink = self.action_sink.clone();
                    let edit_view = view.clone();
                    header = header.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_medium()
                            .when(selected, |el| {
                                el.on_mouse_down(MouseButton::Left, move |event, _, cx| {
                                    if event.click_count >= 2 {
                                        edit_sink.borrow_mut().push(RowAction::StartSectionEdit {
                                            phase: phase_owned2.clone(),
                                            kind: kind_owned2.clone(),
                                            section: section_owned2.clone(),
                                        });
                                        notify(&edit_view, cx);
                                        cx.stop_propagation();
                                    }
                                })
                            })
                            .child(format!("{section} ({count})")),
                    );
                }
                header
            }
            ObligationRow::Item { obligation } => {
                let editing = self.editing_id.as_deref() == Some(&obligation.id.to_string());
                let is_empty = obligation.body.is_empty();
                let color = if is_empty {
                    theme.muted_foreground
                } else {
                    theme.foreground
                };
                let marker_color = theme.muted_foreground;
                let select_sink = sink.clone();
                let select_view = view.clone();
                let mut row_el = h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .items_start()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .pl_12()
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
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        select_sink.borrow_mut().push(RowAction::Select { row_ix });
                        notify(&select_view, cx);
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
                            .child(div().flex_1().min_w_0().child(Textarea::new(input).w_full()));
                    }
                } else {
                    let id = obligation.id;
                    let body = if is_empty {
                        "(new obligation)".to_string()
                    } else {
                        obligation.body.clone()
                    };
                    let edit_sink = self.action_sink.clone();
                    let edit_view = view.clone();
                    row_el = row_el.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(selected, |el| {
                                el.on_mouse_down(MouseButton::Left, move |event, _, cx| {
                                    if event.click_count >= 2 {
                                        edit_sink
                                            .borrow_mut()
                                            .push(RowAction::StartEdit { obligation_id: id });
                                        notify(&edit_view, cx);
                                        cx.stop_propagation();
                                    }
                                })
                            })
                            .child(obligation_body(row_ix, &body, color, window, cx)),
                    );
                    if self.marks.get(&id).is_some_and(|m| m.is_agent()) {
                        row_el = row_el.child(
                            div()
                                .text_xs()
                                .text_color(marker_color)
                                .flex_shrink_0()
                                .pt_0p5()
                                .child("agent"),
                        );
                    }
                    if obligation.phase == PHASE_DESIGN {
                        let has_design = obligation.visual_design_path.is_some();
                        let design_sink = self.action_sink.clone();
                        let design_view = view.clone();
                        row_el = row_el.child(
                            Button::new(("obligation-visual-design", row_ix))
                                .label(if has_design { "Design" } else { "+ Design" })
                                .ghost()
                                .xsmall()
                                .flex_shrink_0()
                                .on_click(move |_, _, cx| {
                                    design_sink
                                        .borrow_mut()
                                        .push(RowAction::OpenVisualDesign { obligation_id: id });
                                    notify(&design_view, cx);
                                }),
                        );
                    }
                }
                row_el
            }
        };

        let mut wrapper = div().id(("obligation-row", row_ix)).w_full().child(content);
        if let Some(obligation_id) = drag_id {
            wrapper = wrapper.on_drag(
                ObligationDragPayload { obligation_id },
                move |payload, _offset, _window, cx| {
                    let obligation_id = payload.obligation_id;
                    cx.new(|_| ObligationDragPreview { obligation_id })
                },
            );
        }
        Some(wrapper.into_any_element())
    }
}

struct ObligationDragPreview {
    #[allow(dead_code)]
    obligation_id: uuid::Uuid,
}

impl Render for ObligationDragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_sm()
            .text_color(theme.foreground)
            .child("Obligation")
    }
}

/// Row click handlers only get `&mut App` (no `Context<ObligationsView>`), so
/// queuing a `RowAction` alone doesn't schedule a repaint. Call this after
/// every push to force one, so `drain_row_actions` runs on the next frame.
fn notify(view: &WeakEntity<super::ObligationsView>, cx: &mut App) {
    let _ = view.update(cx, |_, cx| cx.notify());
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
