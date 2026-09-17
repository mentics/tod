//! The context pane: the highlighted change shown in its node's obligations
//! or plan list, which are the real [`ObligationsView`] and [`PlanStepsView`]
//! hosted embedded. Spec: `doc/conversation/spec.md` §6, "Context panel".
//!
//! The panel has a [`PanelTarget`] — a node, a tab, and the item to
//! highlight. Moving the change-set cursor sets it
//! ([`ConversationView::follow_cursor`]); so do a row's "Context" button and
//! its reference links. The target is pushed to the lists on the next render
//! while the panel is open ([`ConversationView::sync_context`]), so a closed
//! panel keeps following and reopens on the current highlight.

use super::change_set::{key_of, node_of, obligation_of, plan_step_of};
use super::{ConversationView, ConversationViewEvent, Pane};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::views::obligations::ObligationsView;
use crate::views::plan_steps::PlanStepsView;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement,
    Styled, Window, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tab::Tab as TabItem;
use gpui_component::{Disableable, Icon, Sizable, h_flex, v_flex};
use gpui_kit_assets::IconName;
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::Arc;
use tod_core::conversation::context::focus_selection;
use tod_store::conversation::{
    ContextTarget, Entity as ItemEntity, EntitySnapshot, Focus, NetChange, NetOp,
};
use tod_store::fleet::FleetStore;
use tod_store::outline::repos::{ObligationRepo, PlanStepRepo};
use tod_store::outline::{NodeObligation, PlanStep};
use uuid::Uuid;

/// Which of the node's lists the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextTab {
    Obligations,
    Plan,
}

impl ContextTab {
    pub const ALL: [ContextTab; 2] = [ContextTab::Obligations, ContextTab::Plan];

    fn label(self) -> &'static str {
        match self {
            ContextTab::Obligations => "Obligations",
            ContextTab::Plan => "Plan",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    fn for_entity(entity: ItemEntity) -> Self {
        match entity {
            ItemEntity::PlanStep => ContextTab::Plan,
            ItemEntity::Node | ItemEntity::Obligation => ContextTab::Obligations,
        }
    }
}

/// What the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PanelTarget {
    pub node: Uuid,
    pub tab: ContextTab,
    /// The obligation or plan step to highlight, on `tab`'s list.
    pub item: Option<Uuid>,
}

impl PanelTarget {
    fn node(node: Uuid) -> Self {
        Self {
            node,
            tab: ContextTab::Obligations,
            item: None,
        }
    }
}

/// Where the panel goes for a change: the item on the node it now lives on
/// (or was deleted from). A node shows its own obligations; a deleted node
/// shows its former parent's, or nothing when it was top level.
pub(crate) fn target_for_change(change: &NetChange) -> Option<PanelTarget> {
    match change.entity {
        ItemEntity::Node => {
            if change.current.is_some() {
                return Some(PanelTarget::node(change.id));
            }
            match change.before.as_ref() {
                Some(EntitySnapshot::Node {
                    parent_id: Some(parent),
                    ..
                }) => Some(PanelTarget::node(*parent)),
                _ => None,
            }
        }
        entity => Some(PanelTarget {
            node: node_of(change)?,
            tab: ContextTab::for_entity(entity),
            item: Some(change.id),
        }),
    }
}

/// Where the panel goes for a reference link. An item the change set does
/// not hold is looked up; one that no longer exists anywhere goes nowhere.
pub(crate) fn target_for_ref(
    conn: &Connection,
    target: &ContextTarget,
    changes: &[NetChange],
) -> anyhow::Result<Option<PanelTarget>> {
    let (entity, id) = match target {
        ContextTarget::Node { id, .. } => return Ok(Some(PanelTarget::node(*id))),
        ContextTarget::Item { entity, id, .. } => (*entity, *id),
    };
    if entity == ItemEntity::Node {
        return Ok(Some(PanelTarget::node(id)));
    }
    if let Some(change) = changes.iter().find(|c| key_of(c) == (entity, id)) {
        return Ok(target_for_change(change));
    }
    let node = match entity {
        ItemEntity::Obligation => ObligationRepo::new(conn).get(id)?.map(|o| o.node_id),
        ItemEntity::PlanStep => PlanStepRepo::new(conn).get(id)?.map(|s| s.node_id),
        ItemEntity::Node => unreachable!("handled above"),
    };
    Ok(node.map(|node| PanelTarget {
        node,
        tab: ContextTab::for_entity(entity),
        item: Some(id),
    }))
}

/// The change markers for the lists: every changed obligation and plan step.
pub(crate) fn change_markers(
    changes: &[NetChange],
) -> (HashMap<Uuid, NetOp>, HashMap<Uuid, NetOp>) {
    let mut obligations = HashMap::new();
    let mut steps = HashMap::new();
    for change in changes {
        match change.entity {
            ItemEntity::Obligation => {
                obligations.insert(change.id, change.op);
            }
            ItemEntity::PlanStep => {
                steps.insert(change.id, change.op);
            }
            ItemEntity::Node => {}
        }
    }
    (obligations, steps)
}

/// The changed items on `node` that no longer exist, as their last known
/// state, for the lists to show struck through.
pub(crate) fn removed_items(
    changes: &[NetChange],
    node: Uuid,
) -> (Vec<NodeObligation>, Vec<PlanStep>) {
    let gone = changes
        .iter()
        .filter(|c| c.current.is_none() && node_of(c) == Some(node));
    let obligations = gone.clone().filter_map(obligation_of).collect();
    let steps = gone.filter_map(plan_step_of).collect();
    (obligations, steps)
}

pub(crate) struct ContextPanel {
    pub open: bool,
    pub target: Option<PanelTarget>,
    /// The target (or the changes) moved since the lists were last told.
    pub stale: bool,
    /// Titles from the root down to the target node, inclusive.
    pub path: Vec<String>,
    pub obligations: Entity<ObligationsView>,
    pub plan: Entity<PlanStepsView>,
}

impl ContextPanel {
    pub fn new(
        fleet: Arc<FleetStore>,
        window: &mut Window,
        cx: &mut Context<ConversationView>,
    ) -> Self {
        let obligations = cx.new(|cx| {
            let mut view = ObligationsView::new(window, cx, fleet.clone());
            view.set_embedded(true, cx);
            view
        });
        let plan = cx.new(|cx| {
            let mut view = PlanStepsView::new(window, cx, fleet);
            view.set_embedded(true, cx);
            view
        });
        Self {
            open: false,
            target: None,
            stale: false,
            path: Vec::new(),
            obligations,
            plan,
        }
    }

    pub fn tab(&self) -> Option<ContextTab> {
        self.target.map(|t| t.tab)
    }

    /// The focus handle of the list on show.
    pub fn list_focus(&self, cx: &gpui::App) -> Option<FocusHandle> {
        Some(match self.tab()? {
            ContextTab::Obligations => self.obligations.read(cx).focus_handle(cx),
            ContextTab::Plan => self.plan.read(cx).focus_handle(cx),
        })
    }

    /// The conversation Ctrl+J opens from the list on show.
    pub fn conversation_focus(&self, cx: &gpui::App) -> Option<Focus> {
        match self.tab()? {
            ContextTab::Obligations => self.obligations.read(cx).conversation_focus(),
            ContextTab::Plan => self.plan.read(cx).conversation_focus(),
        }
    }
}

impl ConversationView {
    /// Point the panel at the cursor's change. Called wherever the cursor
    /// moves, open or not.
    pub(super) fn follow_cursor(&mut self) {
        let Some(target) = self.highlighted_change().and_then(target_for_change) else {
            return;
        };
        self.set_context_target(target);
    }

    fn set_context_target(&mut self, target: PanelTarget) {
        if self.context.target != Some(target) {
            self.context.target = Some(target);
            self.context.stale = true;
        }
    }

    /// Ctrl+I or the header button.
    pub(super) fn toggle_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.context.open = !self.context.open;
        if self.context.open {
            self.follow_cursor();
            self.context.stale = true;
        } else if self.pane == Pane::Context {
            self.focus_pane(Pane::ChangeSet, window, cx);
        }
        cx.notify();
    }

    /// Open the panel on `target`, leaving keyboard focus where it is.
    pub(super) fn show_in_context(&mut self, target: PanelTarget, cx: &mut Context<Self>) {
        self.context.open = true;
        self.set_context_target(target);
        self.context.stale = true;
        cx.notify();
    }

    /// A row's "Context" button: highlight the row and show its item.
    pub(super) fn show_change_in_context(
        &mut self,
        key: super::ChangeKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pane = Pane::ChangeSet;
        self.picker = None;
        self.set_cursor(Some(key), cx);
        if let Some(target) = self.change(key).and_then(target_for_change) {
            self.show_in_context(target, cx);
        }
        if !self.text_editing() {
            self.focus_handle.focus(window, cx);
        }
    }

    /// Follow link `n` of `changes[ix]`.
    pub(super) fn open_link(&mut self, ix: usize, n: usize, cx: &mut Context<Self>) {
        let Some(reference) = self
            .data
            .changes
            .get(ix)
            .and_then(|c| c.context.get(n))
            .cloned()
        else {
            return;
        };
        let changes = &self.data.changes;
        match self
            .fleet
            .read(|conn| target_for_ref(conn, &reference.target, changes))
        {
            Ok(Some(target)) => {
                self.error = None;
                self.show_in_context(target, cx);
            }
            Ok(None) => {
                self.status_line =
                    format!("{} no longer exists", link_label(&reference.target)).into();
                cx.notify();
            }
            Err(err) => {
                self.error = Some(format!("{err:#}").into());
                cx.notify();
            }
        }
    }

    /// Enter on a highlighted link.
    pub(super) fn open_highlighted_link(&mut self, cx: &mut Context<Self>) -> bool {
        let (Some(key), Some(n)) = (self.cursor, self.link) else {
            return false;
        };
        let Some(ix) = self.data.changes.iter().position(|c| key_of(c) == key) else {
            return false;
        };
        self.open_link(ix, n, cx);
        true
    }

    /// How many links the cursor's row has.
    pub(super) fn cursor_links(&self) -> usize {
        self.cursor
            .and_then(|k| self.change(k))
            .map_or(0, |c| c.context.len())
    }

    /// Right in the change set: into the row's links, then along them.
    /// Returns false when there is nowhere to go (the key moves panes).
    pub(super) fn link_right(&mut self, cx: &mut Context<Self>) -> bool {
        let count = self.cursor_links();
        if self.pane != Pane::ChangeSet || count == 0 {
            return false;
        }
        self.link = Some(match self.link {
            None => 0,
            Some(n) => (n + 1).min(count - 1),
        });
        cx.notify();
        true
    }

    /// Left in the change set: back along the links, then out to the row.
    pub(super) fn link_left(&mut self, cx: &mut Context<Self>) -> bool {
        if self.pane != Pane::ChangeSet {
            return false;
        }
        let Some(n) = self.link else {
            return false;
        };
        self.link = n.checked_sub(1);
        cx.notify();
        true
    }

    pub(super) fn set_context_tab(
        &mut self,
        tab: ContextTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.context.target else {
            return;
        };
        if target.tab == tab {
            return;
        }
        self.context.target = Some(PanelTarget {
            tab,
            // The highlighted item belongs to the other list.
            item: None,
            ..target
        });
        self.context.stale = true;
        if self.pane == Pane::Context {
            self.focus_context_list(window, cx);
        }
        cx.notify();
    }

    pub(super) fn focus_context_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.context.list_focus(cx) {
            Some(handle) => handle.focus(window, cx),
            None => self.focus_handle.focus(window, cx),
        }
    }

    /// "Go to Tasks": the shell shows the node in the Tasks view.
    pub(super) fn go_to_tasks(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.context.target else {
            return;
        };
        let obligation_id = target
            .item
            .filter(|_| target.tab == ContextTab::Obligations);
        cx.emit(ConversationViewEvent::GoToTasks {
            node_id: target.node,
            obligation_id,
        });
    }

    /// Push the target, markers, and removed items to the lists. Runs on
    /// render, so every path that moves the target only has to mark it stale.
    pub(super) fn sync_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.context.open || !std::mem::take(&mut self.context.stale) {
            return;
        }
        let Some(target) = self.context.target else {
            return;
        };
        self.context.path = self
            .fleet
            .read(|conn| focus_selection(conn, Focus::Node(target.node)))
            .map(|s| {
                // A node focus keeps the node's own title out of its path.
                let mut path = s.path;
                path.push(s.title);
                path
            })
            .unwrap_or_default();
        let title = self.context.path.last().cloned().unwrap_or_default();
        let (obligation_markers, step_markers) = change_markers(&self.data.changes);
        let (removed_obligations, removed_steps) = removed_items(&self.data.changes, target.node);
        let item = target.item;
        let tab = target.tab;
        self.context.obligations.update(cx, |list, cx| {
            list.retarget(target.node, &title, None, false, window, cx);
            list.set_removed_items(removed_obligations, window, cx);
            list.set_change_markers(obligation_markers, cx);
            if tab == ContextTab::Obligations
                && let Some(id) = item
            {
                list.highlight_item(id, window, cx);
            }
        });
        self.context.plan.update(cx, |list, cx| {
            list.retarget(target.node, &title, false, window, cx);
            list.set_removed_items(removed_steps, window, cx);
            list.set_change_markers(step_markers, cx);
            if tab == ContextTab::Plan
                && let Some(id) = item
            {
                list.highlight_item(id, window, cx);
            }
        });
        if self.pane == Pane::Context {
            self.focus_context_list(window, cx);
        }
    }

    /// Whether keyboard focus is inside the panel's lists.
    pub(super) fn context_has_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.context.open
            && (self
                .context
                .obligations
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
                || self
                    .context
                    .plan
                    .read(cx)
                    .focus_handle(cx)
                    .contains_focused(window, cx))
    }

    pub(super) fn render_context_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.pane == Pane::Context;
        let target = self.context.target;
        let path = self.context.path.join(" › ");

        let header = style::panel_header(h_flex())
            .w_full()
            .min_w_0()
            .items_center()
            .child(
                if active {
                    style::text_title(div())
                } else {
                    style::text_muted(div())
                }
                .flex_shrink_0()
                .child("Context"),
            )
            .child(div().flex_1().min_w_0().when(!path.is_empty(), |el| {
                el.child(
                    style::text_muted(selectable_text("context-path", path, window, cx))
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .overflow_hidden(),
                )
            }))
            .child(
                Button::new("context-go-to-tasks")
                    .label("Go to Tasks")
                    .icon(Icon::new(IconName::ExternalLink))
                    .ghost()
                    .small()
                    .disabled(target.is_none())
                    .tooltip("Show this node in the Tasks view (G)")
                    .on_click(cx.listener(|this, _, _, cx| this.go_to_tasks(cx))),
            )
            .child(
                Button::new("context-close")
                    .icon(Icon::new(IconName::Close))
                    .ghost()
                    .small()
                    .tooltip("Close (Ctrl+I)")
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_context(window, cx))),
            );

        let body = match target {
            None => style::empty_message(div())
                .p(style::space::INSET)
                .child("Highlight a change to see it in its list")
                .into_any_element(),
            Some(target) => v_flex()
                .size_full()
                .min_h_0()
                .child(
                    style::panel_header(div()).child(
                        super::underline_tab_bar("context-tabs", target.tab.index())
                            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                                this.set_context_tab(ContextTab::ALL[*ix], window, cx);
                            }))
                            .children(
                                ContextTab::ALL
                                    .iter()
                                    .map(|tab| TabItem::new().label(tab.label())),
                            ),
                    ),
                )
                .child(div().flex_1().min_h_0().child(match target.tab {
                    ContextTab::Obligations => self.context.obligations.clone().into_any_element(),
                    ContextTab::Plan => self.context.plan.clone().into_any_element(),
                }))
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .child(
                style::panel_footer(div()).child(
                    style::text_dense_muted(div())
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .overflow_hidden()
                        .child(
                            "1 Obligations · 2 Plan · G Go to Tasks · Ctrl+Left back to changes · Ctrl+I closes",
                        ),
                ),
            )
            .into_any_element()
    }
}

/// A reference's shown label.
pub(crate) fn link_label(target: &ContextTarget) -> &str {
    match target {
        ContextTarget::Item { label, .. } | ContextTarget::Node { label, .. } => label,
    }
}
