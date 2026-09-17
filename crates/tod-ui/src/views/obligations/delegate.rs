use std::collections::{HashMap, HashSet};

use crate::ui::drag_payload::ObligationDragPayload;
use crate::ui::style;
use crate::views::rows::{
    ObligationRowEvent, ObligationRowProps, RowHost, RowOptions, obligation_row, op_icon,
};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
    px,
};
use gpui_component::input::{Input, InputState, TextareaState};
use gpui_component::{ActiveTheme, StyledExt, h_flex};
use tod_store::conversation::NetOp;
use tod_store::outline::{KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation};
use uuid::Uuid;

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

/// What the user did in the list, queued for `ObligationsView` to apply.
#[derive(Debug, Clone)]
pub enum ListAction {
    TogglePhase {
        phase: String,
    },
    ToggleGroup {
        phase: String,
        kind: String,
    },
    ToggleSection {
        phase: String,
        kind: String,
        section: String,
    },
    StartEdit {
        obligation_id: uuid::Uuid,
    },
    StartSectionEdit {
        phase: String,
        kind: String,
        section: String,
    },
    AddSection {
        phase: String,
        kind: String,
    },
    Select {
        row_ix: usize,
    },
    /// Clicked the design-phase obligation's "Design" affordance — create or
    /// open its associated visual-design mockup.
    OpenVisualDesign {
        obligation_id: uuid::Uuid,
    },
}

impl From<ObligationRowEvent> for ListAction {
    fn from(event: ObligationRowEvent) -> Self {
        match event {
            ObligationRowEvent::Select { row_ix } => Self::Select { row_ix },
            ObligationRowEvent::StartEdit { obligation_id } => Self::StartEdit { obligation_id },
            ObligationRowEvent::OpenVisualDesign { obligation_id } => {
                Self::OpenVisualDesign { obligation_id }
            }
        }
    }
}

pub struct ObligationListDelegate {
    rows: Vec<ObligationRow>,
    selected_index: Option<usize>,
    host: RowHost<ListAction>,
    editing_id: Option<String>,
    inline_edit_input: Option<Entity<TextareaState>>,
    section_edit_input: Option<Entity<InputState>>,
    /// Provenance by obligation id; `agent` rows get a subtle marker.
    /// Change-set operations by obligation id, shown as a leading op icon.
    change_markers: HashMap<Uuid, NetOp>,
    /// Obligations shown struck through: removed ones the host still shows.
    struck: HashSet<Uuid>,
}

impl ObligationListDelegate {
    pub fn new(rows: Vec<ObligationRow>, host: RowHost<ListAction>) -> Self {
        Self {
            rows,
            selected_index: None,
            host,
            editing_id: None,
            inline_edit_input: None,
            section_edit_input: None,
            change_markers: HashMap::new(),
            struck: HashSet::new(),
        }
    }

    pub fn set_change_markers(&mut self, markers: HashMap<Uuid, NetOp>) {
        self.change_markers = markers;
    }

    pub fn set_struck(&mut self, struck: HashSet<Uuid>) {
        self.struck = struck;
    }

    pub fn is_struck(&self, id: Uuid) -> bool {
        self.struck.contains(&id)
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
        let host = &self.host;

        let content = match row {
            ObligationRow::Phase {
                phase,
                collapsed,
                count,
            } => {
                let phase_owned = phase.clone();
                let select_host = host.clone();
                let toggle_host = host.clone();
                h_flex()
                    .h(GROUP_ROW_HEIGHT)
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .border_b_1()
                    .border_color(border)
                    .bg(theme.secondary.opacity(0.5))
                    .when(selected, style::highlighted)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        select_host.push(ListAction::Select { row_ix }, cx);
                    })
                    .child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                toggle_host.push(
                                    ListAction::TogglePhase {
                                        phase: phase_owned.clone(),
                                    },
                                    cx,
                                );
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
                    .into_any_element()
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
                let select_host = host.clone();
                let toggle_host = host.clone();
                let add_host = host.clone();
                h_flex()
                    .h(GROUP_ROW_HEIGHT)
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .pl_5()
                    .border_b_1()
                    .border_color(border)
                    .when(selected, style::highlighted)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        select_host.push(ListAction::Select { row_ix }, cx);
                    })
                    .child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                toggle_host.push(
                                    ListAction::ToggleGroup {
                                        phase: phase_owned.clone(),
                                        kind: kind_owned.clone(),
                                    },
                                    cx,
                                );
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
                                add_host.push(
                                    ListAction::AddSection {
                                        phase: add_section_phase.clone(),
                                        kind: add_section_kind.clone(),
                                    },
                                    cx,
                                );
                                cx.stop_propagation();
                            })
                            .child("+ Section"),
                    )
                    .into_any_element()
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
                let select_host = host.clone();
                let toggle_host = host.clone();
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
                    .when(selected, style::highlighted)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        select_host.push(ListAction::Select { row_ix }, cx);
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
                                toggle_host.push(
                                    ListAction::ToggleSection {
                                        phase: phase_owned.clone(),
                                        kind: kind_owned.clone(),
                                        section: section_owned.clone(),
                                    },
                                    cx,
                                );
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
                    let edit_host = host.clone();
                    header = header.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_medium()
                            .when(selected, |el| {
                                el.on_mouse_down(MouseButton::Left, move |event, _, cx| {
                                    if event.click_count >= 2 {
                                        edit_host.push(
                                            ListAction::StartSectionEdit {
                                                phase: phase_owned2.clone(),
                                                kind: kind_owned2.clone(),
                                                section: section_owned2.clone(),
                                            },
                                            cx,
                                        );
                                        cx.stop_propagation();
                                    }
                                })
                            })
                            .child(format!("{section} ({count})")),
                    );
                }
                header.into_any_element()
            }
            ObligationRow::Item { obligation } => {
                let editing = self.editing_id.as_deref() == Some(row_key.as_str());
                let opts = RowOptions {
                    leading: self
                        .change_markers
                        .get(&obligation.id)
                        .map(|op| op_icon(("obligation-op", row_ix), *op)),
                    struck: self.struck.contains(&obligation.id),
                    ..RowOptions::default()
                };
                let props = ObligationRowProps {
                    obligation: &obligation,
                    row_ix,
                    highlighted: selected,
                    editor: self.inline_edit_input.as_ref().filter(|_| editing),
                };
                obligation_row(props, host, opts, window, cx)
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
