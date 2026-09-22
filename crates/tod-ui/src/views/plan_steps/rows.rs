//! What the plan-steps panel puts in the item list: its item payload, and how
//! one step renders.
//!
//! The list itself — cursor, keys, scrolling — is [`crate::ui::item_list`].
//! Only what a plan step *is* lives here.

use crate::ui::item_list::{ItemListEvent, ItemListRow, ItemRowState};
use crate::views::rows::{
    PlanStepRowEvent, PlanStepRowProps, RowHost, RowOptions, op_icon, plan_step_row,
};
use gpui::{AnyElement, App, Entity, Window};
use gpui_component::input::TextareaState;
use tod_store::conversation::NetOp;
use tod_store::outline::PlanStep;
use uuid::Uuid;

/// One plan step, with what the panel shows about it beyond its own fields.
#[derive(Debug, Clone)]
pub struct PlanStepItem {
    pub step: PlanStep,
    /// Steps this one depends on.
    pub depends_on: Vec<Uuid>,
    /// Obligations this step satisfies.
    pub satisfies: Vec<Uuid>,
    /// Removed, and shown struck through at its old place.
    pub struck: bool,
    /// A change-set operation, shown as a leading op icon.
    pub marker: Option<NetOp>,
}

/// A plan is a flat run of steps: their order and structure come from the
/// dependency graph, not from a hierarchy, so there is nothing to group by.
pub type PlanRow = ItemListRow<PlanStepItem>;

/// What the user did in the list, queued for `PlanStepsView` to apply.
#[derive(Debug, Clone)]
pub enum ListAction {
    Select {
        row_ix: usize,
    },
    StartEdit {
        step_id: Uuid,
    },
    /// Something the list can report but this one never does.
    Ignored,
}

impl From<PlanStepRowEvent> for ListAction {
    fn from(event: PlanStepRowEvent) -> Self {
        match event {
            PlanStepRowEvent::Select { row_ix } => Self::Select { row_ix },
            PlanStepRowEvent::StartEdit { step_id } => Self::StartEdit { step_id },
        }
    }
}

impl From<ItemListEvent> for ListAction {
    fn from(event: ItemListEvent) -> Self {
        match event {
            ItemListEvent::Select { row_ix } => Self::Select { row_ix },
            // A flat, single-select list has neither a group heading to
            // collapse nor a checkbox to tick, so neither event arrives.
            ItemListEvent::ToggleGroup { .. } | ItemListEvent::ToggleMark { .. } => Self::Ignored,
        }
    }
}

/// Render one plan step for the item list.
pub fn render_plan_step(
    item: &PlanStepItem,
    state: ItemRowState<'_>,
    editor: &Entity<TextareaState>,
    host: &RowHost<ListAction>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let opts = RowOptions {
        leading: item
            .marker
            .map(|op| op_icon(("plan-step-op", state.row_ix), op)),
        struck: item.struck,
        ..RowOptions::default()
    };
    let props = PlanStepRowProps {
        step: &item.step,
        depends_on: &item.depends_on,
        satisfies: &item.satisfies,
        row_ix: state.row_ix,
        highlighted: state.highlighted,
        editor: Some(editor).filter(|_| state.editing),
    };
    plan_step_row(props, host, opts, window, cx)
}
