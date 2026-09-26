//! Obligation, plan-step, node, review-finding, and pull-request rows that
//! any view can host.
//!
//! The obligations list, the plan list, and the conversation change set all
//! render the same rows. A row reports what the user did through a
//! [`RowHost`] rather than a handle to one particular view, and a
//! [`RowOptions`] adapts it to where it is shown (a compact change-set line,
//! an op icon, hover actions, strike-through, an unsure flag).

pub mod finding_row;
pub mod node_row;
pub mod obligation_row;
pub mod plan_step_row;
pub mod pull_request_row;
pub mod status_menu;

use std::cell::RefCell;
use std::rc::Rc;

use crate::ui::style;
use gpui::{
    AnyElement, App, ElementId, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, WeakEntity, Window, div, prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, Sizable as _, h_flex};
use gpui_kit_assets::IconName;
use tod_store::conversation::NetOp;

pub use finding_row::{
    FindingRowEvent, FindingRowProps, STATUS_COLUMN_WIDTH, finding_columns, finding_row,
};
pub use node_row::{NodeRowEvent, NodeRowProps, node_row};
pub use obligation_row::{ObligationRowEvent, ObligationRowProps, obligation_row};
pub use plan_step_row::{PlanStepRowEvent, PlanStepRowProps, plan_step_columns, plan_step_row};
pub use status_menu::StatusMenu;

/// Where a row sends what the user did: an action queue plus a callback
/// that makes the owner drain it.
///
/// Row event handlers only get `&mut App`, not the owner's `Context`, so
/// queuing an action alone would not schedule a repaint and nothing would
/// ever drain the queue. [`RowHost::push`] queues and then calls `notify`.
pub struct RowHost<A> {
    sink: Rc<RefCell<Vec<A>>>,
    notify: Rc<dyn Fn(&mut App)>,
}

impl<A> Clone for RowHost<A> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
            notify: self.notify.clone(),
        }
    }
}

impl<A: 'static> RowHost<A> {
    pub fn new(notify: impl Fn(&mut App) + 'static) -> Self {
        Self {
            sink: Rc::new(RefCell::new(Vec::new())),
            notify: Rc::new(notify),
        }
    }

    /// A host whose owner is `entity`: pushing an action repaints it, and its
    /// render drains the queue.
    pub fn for_entity<V: 'static>(entity: WeakEntity<V>) -> Self {
        Self::new(move |cx| {
            let _ = entity.update(cx, |_, cx| cx.notify());
        })
    }

    /// Queue `action` and ask the owner to drain it.
    pub fn push(&self, action: A, cx: &mut App) {
        self.sink.borrow_mut().push(action);
        (self.notify)(cx);
    }

    /// Take every queued action, oldest first.
    pub fn drain(&self) -> Vec<A> {
        self.sink.borrow_mut().drain(..).collect()
    }
}

/// Something a row affords: a button on the row while it is hovered or
/// highlighted, and an entry in the row's right-click menu.
///
/// One action, listed once. A list declares its rows' actions through
/// [`crate::ui::item_list::ItemList::with_row_actions`], and the component
/// puts them in both places — the same item affords the same actions wherever
/// it is shown.
#[derive(Clone)]
pub struct RowAction {
    /// Unique within the row.
    pub id: SharedString,
    pub label: SharedString,
    pub icon: Option<IconName>,
    pub tooltip: Option<SharedString>,
    /// Offered in the row's menu only, never as a button on the row. For an
    /// action the row has no room to show — the panels' own Edit, Delete and
    /// Add, which are keys and menu entries, not buttons on every row.
    pub menu_only: bool,
    pub on_click: Rc<dyn Fn(&mut Window, &mut App)>,
}

impl RowAction {
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            tooltip: None,
            menu_only: false,
            on_click: Rc::new(on_click),
        }
    }

    /// Offer this action in the row's menu only.
    pub fn menu_only(mut self) -> Self {
        self.menu_only = true;
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
}

/// How a row is shown where it is hosted. The default is the row as the
/// obligations and plan lists show it.
#[derive(Default)]
pub struct RowOptions {
    /// One line, the text truncated at the end.
    pub compact: bool,
    /// With `compact`: the text wraps instead, so the row grows to show all
    /// of it (`styles.row-wrapped`).
    pub wrap: bool,
    /// Shown first; the op icon ([`op_icon`]).
    pub leading: Option<AnyElement>,
    /// Shown last. Never covered and never shrunk: the text truncates first.
    pub trailing_context: Option<AnyElement>,
    /// What the row affords. The menu-only ones ([`RowAction::menu_only`]) are
    /// not shown as buttons; they reach the user through the row's menu.
    /// Shown while the row is hovered or highlighted, beside the trailing
    /// context and never on top of it.
    pub actions: Vec<RowAction>,
    /// Shown under the row's body: whatever the host has to say about this
    /// item that the item itself does not carry (a conversation's handoff
    /// answers, what verification found).
    pub detail: Option<AnyElement>,
    /// The text is struck through (deleted or reversed items).
    pub struck: bool,
    /// An unsure dot, with this reason as its tooltip.
    pub flag: Option<String>,
    /// The host installs a right-click menu on this row
    /// ([`crate::ui::item_list::ItemList::with_row_actions`]), and that menu
    /// carries Copy. The row's own text therefore adds no Copy menu of its
    /// own: both hitboxes are hovered by the one click, so two menus would
    /// open on top of each other.
    pub menu_hosted: bool,
}

impl RowOptions {
    #[allow(dead_code)]
    pub fn compact() -> Self {
        Self {
            compact: true,
            ..Self::default()
        }
    }

    /// The row gets the `hover-row` state. The lists never had one, so it is
    /// only added where the row is used in a new way.
    fn hoverable(&self) -> bool {
        self.compact || self.actions.iter().any(|action| !action.menu_only)
    }
}

/// The actions shown as buttons on the row: what is offered in the menu only
/// is not one of them.
fn hover_actions(actions: Vec<RowAction>) -> Vec<RowAction> {
    actions
        .into_iter()
        .filter(|action| !action.menu_only)
        .collect()
}

/// The icon-set icon for a change-set operation.
pub fn op_icon_name(op: NetOp) -> IconName {
    match op {
        NetOp::Added => IconName::Plus,
        NetOp::Edited => IconName::Pencil,
        NetOp::Moved => IconName::ArrowRight,
        NetOp::Deleted => IconName::Minus,
        NetOp::Reversed => IconName::Undo2,
    }
}

/// The name of an operation, for its icon's tooltip.
pub fn op_name(op: NetOp) -> &'static str {
    match op {
        NetOp::Added => "Added",
        NetOp::Edited => "Edited",
        NetOp::Moved => "Moved",
        NetOp::Deleted => "Deleted",
        NetOp::Reversed => "Reversed",
    }
}

/// The leading op icon for a row, with the operation's name as its tooltip.
/// `id` must be unique among the rows it is shown in.
pub fn op_icon(id: impl Into<ElementId>, op: NetOp) -> AnyElement {
    let name = op_name(op);
    style::text_muted(div())
        .id(id)
        .flex_shrink_0()
        .flex()
        .items_center()
        .child(Icon::new(op_icon_name(op)).small())
        .tooltip(move |window, cx| Tooltip::new(name).build(window, cx))
        .into_any_element()
}

/// The unsure dot, with the reason as its tooltip.
fn flag_dot(id: ElementId, reason: String) -> AnyElement {
    let reason = SharedString::from(reason);
    style::text_muted(div())
        .id(id)
        .flex_shrink_0()
        .flex()
        .items_center()
        .child(Icon::new(IconName::CircleDot).xsmall())
        .tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx))
        .into_any_element()
}

/// The buttons a row shows for what it affords: the actions offered in the
/// menu only are not among them, so a row with nothing but menu entries shows
/// no strip at all. `group` is the row's hover group ([`row_group`]), which the
/// row element itself must carry; `highlighted` keeps the buttons visible
/// without hover.
///
/// Implemented here once so a row that is not one of this module's own — a
/// table row a list renders itself — shows its actions the same way.
pub fn row_action_buttons(
    key: &str,
    group: &SharedString,
    highlighted: bool,
    actions: Vec<RowAction>,
) -> Option<AnyElement> {
    let actions = hover_actions(actions);
    if actions.is_empty() {
        return None;
    }
    let group = group.clone();
    Some(
        h_flex()
            .flex_shrink_0()
            .items_center()
            .gap(style::space::INLINE)
            // Hidden rather than absent, so showing the buttons never moves
            // whatever sits beside them.
            .when(!highlighted, |el| {
                el.invisible().group_hover(group, |style| style.visible())
            })
            .children(actions.into_iter().map(|action| {
                let on_click = action.on_click.clone();
                let mut button = Button::new(ElementId::Name(
                    format!("row-action-{key}-{}", action.id).into(),
                ))
                .label(action.label.clone())
                .ghost()
                .xsmall()
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    on_click(window, cx);
                });
                if let Some(icon) = action.icon {
                    button = button.icon(Icon::new(icon));
                }
                if let Some(tooltip) = action.tooltip.clone() {
                    button = button.tooltip(tooltip);
                }
                button.into_any_element()
            }))
            .into_any_element(),
    )
}

/// The shared tail of a row: flag, hover actions, then trailing context.
/// `group` names the row's hover group; `highlighted` keeps the actions
/// visible without hover.
fn row_tail(
    key: &str,
    group: &SharedString,
    highlighted: bool,
    opts: &mut RowOptions,
) -> Vec<AnyElement> {
    let mut tail = Vec::new();
    if let Some(reason) = opts.flag.take() {
        tail.push(flag_dot(
            ElementId::Name(format!("row-flag-{key}").into()),
            reason,
        ));
    }
    if let Some(buttons) =
        row_action_buttons(key, group, highlighted, std::mem::take(&mut opts.actions))
    {
        tail.push(buttons);
    }
    if let Some(context) = opts.trailing_context.take() {
        tail.push(
            style::text_muted(div())
                .flex_shrink_0()
                .whitespace_nowrap()
                .child(context)
                .into_any_element(),
        );
    }
    tail
}

/// The hover group name for the row with `key`.
pub fn row_group(key: &str) -> SharedString {
    format!("row-{key}").into()
}

/// A store with one Spec node holding obligations and plan steps, for the
/// list views' tests.
#[cfg(test)]
pub(crate) mod fixture {
    use std::sync::Arc;
    use tod_store::fleet::FleetStore;
    use tod_store::interview::{PHASE_DESIGN, PHASE_REQUIREMENTS};
    use tod_store::outline::types::Capability;
    use tod_store::outline::{CreatePosition, KIND_REQUIREMENT, OutlineMutation};
    use uuid::Uuid;

    pub struct Fixture {
        pub store: Arc<FleetStore>,
        pub node_id: Uuid,
        /// A requirements-phase obligation in section "Offline".
        pub offline_obligation: Uuid,
        /// A design-phase obligation.
        pub design_obligation: Uuid,
        pub steps: Vec<Uuid>,
    }

    impl Fixture {
        pub fn new() -> Self {
            let root = std::env::temp_dir().join(format!("tod-rows-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let store = FleetStore::open(&root).unwrap();
            let apply = |mutation| {
                store.enqueue_outline(mutation).unwrap();
                store.writer().flush().unwrap();
            };
            apply(OutlineMutation::CreateList {
                slug: "rows".into(),
                title: "Rows".into(),
            });
            let list_id = store.list_outline_lists().unwrap()[0].id;
            let node_id = Uuid::new_v4();
            apply(OutlineMutation::CreateNode {
                node_id: Some(node_id),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Web client".into(),
            });
            apply(OutlineMutation::EnableCapabilities {
                node_id,
                capabilities: vec![Capability::Spec],
            });
            let obligation = |phase: &str, section: Option<&str>, body: &str| {
                let id = Uuid::new_v4();
                apply(OutlineMutation::CreateObligation {
                    obligation_id: Some(id),
                    node_id,
                    kind: KIND_REQUIREMENT.into(),
                    after_id: None,
                    before: false,
                    section: section.map(str::to_string),
                    body: body.into(),
                    phase: phase.into(),
                });
                id
            };
            obligation(PHASE_REQUIREMENTS, None, "Users can sign in");
            let offline_obligation =
                obligation(PHASE_REQUIREMENTS, Some("Offline"), "Works offline");
            let design_obligation = obligation(PHASE_DESIGN, None, "Sign-in is one screen");
            let steps = ["Build the form", "Add offline sync"]
                .into_iter()
                .map(|body| {
                    let id = Uuid::new_v4();
                    apply(OutlineMutation::CreatePlanStep {
                        step_id: Some(id),
                        node_id,
                        after_id: None,
                        before: false,
                        body: body.into(),
                    });
                    id
                })
                .collect();
            store.reload_if_stale().ok();
            Self {
                store: Arc::new(store),
                node_id,
                offline_obligation,
                design_obligation,
                steps,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[gpui::test]
    fn row_host_queues_then_notifies(cx: &mut gpui::TestAppContext) {
        let notified = Rc::new(Cell::new(0));
        let seen = notified.clone();
        let host: RowHost<u32> = RowHost::new(move |_| seen.set(seen.get() + 1));
        let observer = host.clone();
        cx.update(|cx| {
            host.push(1, cx);
            host.push(2, cx);
        });
        assert_eq!(notified.get(), 2);
        assert_eq!(observer.drain(), vec![1, 2]);
        assert!(host.drain().is_empty());
    }

    #[test]
    fn a_menu_only_action_is_no_button_on_the_row() {
        let actions = vec![
            RowAction::new("reverse", "Reverse", |_, _| {}),
            RowAction::new("delete", "Delete", |_, _| {}).menu_only(),
        ];
        let shown: Vec<_> = hover_actions(actions.clone())
            .into_iter()
            .map(|action| action.id)
            .collect();
        assert_eq!(shown, vec![SharedString::from("reverse")]);
        // A row whose only actions are menu entries does not become hoverable
        // for buttons it never shows.
        let opts = RowOptions {
            actions: vec![actions[1].clone()],
            ..RowOptions::default()
        };
        assert!(!opts.hoverable());
    }

    #[test]
    fn every_op_has_a_distinct_icon() {
        let ops = [
            NetOp::Added,
            NetOp::Edited,
            NetOp::Moved,
            NetOp::Deleted,
            NetOp::Reversed,
        ];
        let icons: std::collections::HashSet<_> = ops.iter().map(|op| op_icon_name(*op)).collect();
        assert_eq!(icons.len(), ops.len());
        // Each icon must be one the app's asset source serves.
        for op in ops {
            let path = op_icon_name(op).path();
            assert!(crate::app::assets::serves(&path), "{path} is not bundled");
        }
    }
}
