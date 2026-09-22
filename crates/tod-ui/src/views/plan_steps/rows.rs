//! What the plan-steps panel puts in the item list: its item payload, and how
//! one step renders.
//!
//! The list itself — cursor, keys, scrolling — is [`crate::ui::item_list`].
//! Only what a plan step *is* lives here.

use crate::ui::item_list::{ItemDropped, ItemListEvent, ItemListRow, ItemRowState};
use crate::views::rows::{
    PlanStepRowEvent, PlanStepRowProps, RowAction, RowHost, RowOptions, StatusMenu, op_icon,
    plan_step_row,
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

/// This list's name in an [`crate::ui::item_list::ItemDrag`] payload.
pub const DRAG_LIST: &str = "plan-steps";

/// What the user did in the list, queued for `PlanStepsView` to apply.
#[derive(Debug, Clone)]
pub enum ListAction {
    Select {
        row_ix: usize,
    },
    /// Dragged a step to a new place in the plan.
    Drop(ItemDropped),
    StartEdit {
        step_id: Uuid,
    },
    ToggleStatusMenu {
        step_id: Uuid,
    },
    ChooseStatus {
        step_id: Uuid,
        status: &'static str,
    },
    DismissStatusMenu,
    /// Add a step below the selected one (what `n` does).
    CreateBelow,
    /// Delete the selection (what Del does).
    DeleteSelected,
    /// Something the list can report but this one never does.
    Ignored,
}

impl From<PlanStepRowEvent> for ListAction {
    fn from(event: PlanStepRowEvent) -> Self {
        match event {
            PlanStepRowEvent::Select { row_ix } => Self::Select { row_ix },
            PlanStepRowEvent::StartEdit { step_id } => Self::StartEdit { step_id },
            PlanStepRowEvent::ToggleStatusMenu { step_id } => Self::ToggleStatusMenu { step_id },
            PlanStepRowEvent::ChooseStatus { step_id, status } => {
                Self::ChooseStatus { step_id, status }
            }
            PlanStepRowEvent::DismissStatusMenu => Self::DismissStatusMenu,
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
            ItemListEvent::Drop(dropped) => Self::Drop(dropped),
        }
    }
}

/// What a plan step affords, wherever it is shown: what its keys do, as
/// entries in its right-click menu. The panel shows no buttons for them --- the
/// rows stay as they were --- so each is
/// [menu-only](crate::views::rows::RowAction::menu_only). Setting the status
/// opens the same dropdown the status chip and `t` open.
pub fn plan_step_actions(item: &PlanStepItem, host: &RowHost<ListAction>) -> Vec<RowAction> {
    if item.struck {
        // A removed step shown at its old place is not a thing to edit.
        return Vec::new();
    }
    let step_id = item.step.id;
    let action = |id: &'static str, label: &'static str, list_action: ListAction| {
        let host = host.clone();
        RowAction::new(id, label, move |_, cx| {
            host.push(list_action.clone(), cx);
        })
        .menu_only()
    };
    vec![
        action("edit", "Edit", ListAction::StartEdit { step_id }),
        action(
            "status",
            "Set status...",
            ListAction::ToggleStatusMenu { step_id },
        ),
        action("add-below", "Add below", ListAction::CreateBelow),
        action("delete", "Delete", ListAction::DeleteSelected),
    ]
}

/// Render one plan step for the item list. `menu` is the open status
/// dropdown, whichever step it is on, and `detail` what the host has to add
/// under this step's body.
pub fn render_plan_step(
    item: &PlanStepItem,
    state: ItemRowState<'_>,
    editor: &Entity<TextareaState>,
    menu: Option<StatusMenu>,
    detail: Option<AnyElement>,
    host: &RowHost<ListAction>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let opts = RowOptions {
        leading: item
            .marker
            .map(|op| op_icon(("plan-step-op", state.row_ix), op)),
        detail,
        struck: item.struck,
        ..state.row_options()
    };
    let props = PlanStepRowProps {
        step: &item.step,
        depends_on: &item.depends_on,
        satisfies: &item.satisfies,
        row_ix: state.row_ix,
        highlighted: state.highlighted,
        editor: Some(editor).filter(|_| state.editing),
        status_menu: menu.filter(|m| m.is_on(item.step.id)),
        columns: state.columns,
    };
    plan_step_row(props, host, opts, window, cx)
}
