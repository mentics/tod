use std::collections::{HashMap, HashSet};

use crate::views::rows::{
    PlanStepRowEvent, PlanStepRowProps, RowHost, RowOptions, op_icon, plan_step_row,
};
use gpui::{
    AnyElement, App, Entity, InteractiveElement, IntoElement, ParentElement, Styled, Window, div,
};
use gpui_component::input::TextareaState;
use tod_store::conversation::NetOp;
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

/// What the user did in the list, queued for `PlanStepsView` to apply.
#[derive(Debug, Clone)]
pub enum ListAction {
    StartEdit { step_id: Uuid },
    Select { row_ix: usize },
}

impl From<PlanStepRowEvent> for ListAction {
    fn from(event: PlanStepRowEvent) -> Self {
        match event {
            PlanStepRowEvent::Select { row_ix } => Self::Select { row_ix },
            PlanStepRowEvent::StartEdit { step_id } => Self::StartEdit { step_id },
        }
    }
}

pub struct PlanStepListDelegate {
    rows: Vec<PlanStepRow>,
    selected_index: Option<usize>,
    host: RowHost<ListAction>,
    editing_id: Option<String>,
    inline_edit_input: Option<Entity<TextareaState>>,
    /// Change-set operations by step id, shown as a leading op icon.
    change_markers: HashMap<Uuid, NetOp>,
    /// Steps shown struck through: removed ones the host still shows.
    struck: HashSet<Uuid>,
}

impl PlanStepListDelegate {
    pub fn new(rows: Vec<PlanStepRow>, host: RowHost<ListAction>) -> Self {
        Self {
            rows,
            selected_index: None,
            host,
            editing_id: None,
            inline_edit_input: None,
            change_markers: HashMap::new(),
            struck: HashSet::new(),
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

    pub fn set_struck(&mut self, struck: HashSet<Uuid>) {
        self.struck = struck;
    }

    pub fn is_struck(&self, id: Uuid) -> bool {
        self.struck.contains(&id)
    }

    pub fn set_change_markers(&mut self, markers: HashMap<Uuid, NetOp>) {
        self.change_markers = markers;
    }

    pub fn render_row(
        &self,
        row_ix: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let row = self.rows.get(row_ix)?;
        let editing = self.editing_id.as_deref() == Some(row.key().as_str());
        let opts = RowOptions {
            leading: self
                .change_markers
                .get(&row.step.id)
                .map(|op| op_icon(("plan-step-op", row_ix), *op)),
            struck: self.struck.contains(&row.step.id),
            ..RowOptions::default()
        };
        let props = PlanStepRowProps {
            step: &row.step,
            depends_on: &row.depends_on,
            satisfies: &row.satisfies,
            row_ix,
            highlighted: self.selected_index == Some(row_ix),
            editor: self.inline_edit_input.as_ref().filter(|_| editing),
        };
        let content = plan_step_row(props, &self.host, opts, window, cx);
        Some(
            div()
                .id(("plan-step-row", row_ix))
                .w_full()
                .child(content)
                .into_any_element(),
        )
    }
}
