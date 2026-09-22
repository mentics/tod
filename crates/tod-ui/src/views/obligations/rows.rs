//! What the obligations panel puts in the item list: its group identities, its
//! item payload, and how one obligation renders.
//!
//! The list itself — cursor, selection, collapsed groups, headings, keys,
//! scrolling — is [`crate::ui::item_list`]. Only what an obligation *is* lives
//! here.

use crate::ui::drag_payload::ObligationDragPayload;
use crate::ui::item_list::{ItemListEvent, ItemListRow, ItemRowState};
use crate::ui::style;
use crate::views::rows::{
    ObligationRowEvent, ObligationRowProps, RowHost, RowOptions, obligation_row, op_icon,
};
use gpui::{
    AnyElement, App, AppContext, Context, Div, Entity, IntoElement, ParentElement, Render,
    Stateful, StatefulInteractiveElement, Styled, Window, div,
};
use gpui_component::input::TextareaState;
use gpui_component::ActiveTheme;
use tod_store::conversation::NetOp;
use tod_store::outline::{KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation};
use tod_store::verification::VERDICT_FAILED;
use uuid::Uuid;

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

pub fn kind_label(kind: &str) -> &str {
    match kind {
        KIND_REQUIREMENT => "Requirements",
        KIND_CONSTRAINT => "Constraints",
        other => other,
    }
}

/// The kind a row belongs to, as one of the two static names.
pub fn static_kind(kind: &str) -> &'static str {
    if kind == KIND_CONSTRAINT {
        KIND_CONSTRAINT
    } else {
        KIND_REQUIREMENT
    }
}

/// What a group heading in this list stands for — the three levels
/// obligations group by, carried on the row so the view never parses a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObGroup {
    Phase {
        phase: String,
    },
    Kind {
        phase: String,
        kind: &'static str,
    },
    Section {
        phase: String,
        kind: &'static str,
        section: String,
        /// A section whose name is being typed and that does not exist yet.
        is_new: bool,
    },
}

/// One obligation, with what the panel shows about it beyond its own fields.
#[derive(Debug, Clone)]
pub struct ObligationItem {
    pub obligation: NodeObligation,
    /// Removed, and shown struck through at its old place.
    pub struck: bool,
    /// A change-set operation, shown as a leading op icon.
    pub marker: Option<NetOp>,
    /// Where the obligation stands — its verdict, else whether a plan step
    /// satisfies it — shown as a trailing badge. `None` before the node has a
    /// plan to stand against.
    pub standing: Option<String>,
}

pub type ObRow = ItemListRow<ObligationItem, ObGroup>;

/// What the user did in the list, queued for `ObligationsView` to apply.
#[derive(Debug, Clone)]
pub enum ListAction {
    Select {
        row_ix: usize,
    },
    ToggleGroup {
        key: String,
    },
    ToggleMark {
        row_ix: usize,
    },
    StartEdit {
        obligation_id: Uuid,
    },
    StartSectionEdit {
        phase: String,
        kind: &'static str,
        section: String,
    },
    AddSection {
        phase: String,
        kind: &'static str,
    },
    /// Clicked the design-phase obligation's "Design" affordance — create or
    /// open its associated visual-design mockup.
    OpenVisualDesign {
        obligation_id: Uuid,
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

impl From<ItemListEvent> for ListAction {
    fn from(event: ItemListEvent) -> Self {
        match event {
            ItemListEvent::Select { row_ix } => Self::Select { row_ix },
            ItemListEvent::ToggleGroup { key } => Self::ToggleGroup { key },
            ItemListEvent::ToggleMark { row_ix } => Self::ToggleMark { row_ix },
        }
    }
}

/// Render one obligation for the item list.
pub fn render_obligation(
    item: &ObligationItem,
    state: ItemRowState<'_>,
    editor: &Entity<TextareaState>,
    host: &RowHost<ListAction>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let opts = RowOptions {
        leading: item
            .marker
            .map(|op| op_icon(("obligation-op", state.row_ix), op)),
        trailing_context: item.standing.as_deref().map(standing_badge),
        struck: item.struck,
        ..RowOptions::default()
    };
    let props = ObligationRowProps {
        obligation: &item.obligation,
        row_ix: state.row_ix,
        highlighted: state.highlighted,
        editor: Some(editor).filter(|_| state.editing),
    };
    obligation_row(props, host, opts, window, cx)
}

/// Where the obligation stands, as a badge; a failed verdict reads as an
/// error.
fn standing_badge(standing: &str) -> AnyElement {
    let badge = style::badge(div()).child(standing.to_string());
    if standing == VERDICT_FAILED {
        style::text_error(badge).into_any_element()
    } else {
        badge.into_any_element()
    }
}

/// Make an obligation row draggable, with the obligation's own payload.
pub fn draggable(item: &ObligationItem, row: Stateful<Div>) -> Stateful<Div> {
    let obligation_id = item.obligation.id;
    row.on_drag(
        ObligationDragPayload { obligation_id },
        move |payload, _offset, _window, cx| {
            let obligation_id = payload.obligation_id;
            cx.new(|_| ObligationDragPreview { obligation_id })
        },
    )
}

struct ObligationDragPreview {
    #[allow(dead_code)]
    obligation_id: Uuid,
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
