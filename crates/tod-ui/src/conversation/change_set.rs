//! The change-set pane: everything the conversation changed, grouped by node,
//! with reversal, inline edits, and unsure flags.

use crate::ui::status_filter::{StatusFilter, render_status_filter};
use super::context_panel::link_label;
use super::{ChangeAction, ConversationView, Pane};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::views::rows::{
    NodeRowProps, ObligationRowProps, PlanStepRowProps, RowAction, RowOptions, node_row,
    obligation_row, op_icon, plan_step_row,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div,
};
use gpui_component::RopeExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::scroll::Scrollbar;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable, Icon, Sizable, h_flex, v_flex};
use gpui_kit_assets::IconName;
use std::collections::HashMap;
use tod_store::conversation::{
    Entity as ItemEntity, EntitySnapshot, Focus, NetChange, NetOp, ReverseOutcome,
    capabilities_changes,
};
use tod_store::interview::{InterviewCommand, short_id};
use tod_store::outline::repos::plan_steps::HandoffReason;
use tod_store::outline::{NodeObligation, OutlineMutation, PlanStep};
use uuid::Uuid;

/// A change's identity: the item it is about.
pub(crate) type ChangeKey = (ItemEntity, Uuid);

pub(crate) fn key_of(change: &NetChange) -> ChangeKey {
    (change.entity, change.id)
}

/// The item's latest known state.
fn snapshot_of(change: &NetChange) -> Option<&EntitySnapshot> {
    change.current.as_ref().or(change.before.as_ref())
}

/// The node a change is grouped under.
pub(crate) fn node_of(change: &NetChange) -> Option<Uuid> {
    change
        .node_id
        .or_else(|| snapshot_of(change).map(|s| s.node_id(change.id)))
}

/// The focus that talks about a change's item.
pub(crate) fn focus_of(change: &NetChange) -> Option<Focus> {
    let node = node_of(change)?;
    Some(match change.entity {
        ItemEntity::Node => Focus::Node(change.id),
        ItemEntity::Obligation => Focus::Obligation {
            node,
            id: change.id,
        },
        ItemEntity::PlanStep => Focus::PlanStep {
            node,
            id: change.id,
        },
        ItemEntity::Capabilities => Focus::Node(change.id),
    })
}

/// A capabilities row's text: what changed, e.g. "Capabilities: enabled
/// Lifecycle; disabled Spec, removing 3 obligation(s)".
pub(crate) fn capabilities_title(change: &NetChange) -> String {
    let changes = capabilities_changes(change.before.as_ref(), change.current.as_ref());
    if changes.is_empty() {
        "Capabilities".to_string()
    } else {
        format!("Capabilities: {}", changes.join("; "))
    }
}

/// The change-set filter's toggles: a change flagged as unsure, and one that
/// deletes its item. They overlap, so both on shows either kind.
pub(crate) const CHANGE_UNSURE: &str = "unsure";
pub(crate) const CHANGE_DELETED: &str = "deleted";

/// Whether `filter` lets `change` through; empty shows every change.
pub(crate) fn change_shows(filter: &StatusFilter, change: &NetChange) -> bool {
    filter.is_empty()
        || (filter.contains(CHANGE_UNSURE) && change.flag.is_some())
        || (filter.contains(CHANGE_DELETED) && change.op == NetOp::Deleted)
}

/// How many changes each toggle matches.
pub(crate) fn change_counts(changes: &[NetChange]) -> Vec<(String, usize)> {
    vec![
        (
            CHANGE_UNSURE.to_string(),
            changes.iter().filter(|c| c.flag.is_some()).count(),
        ),
        (
            CHANGE_DELETED.to_string(),
            changes.iter().filter(|c| c.op == NetOp::Deleted).count(),
        ),
    ]
}

/// One line of the change-set list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisplayRow {
    /// A node group's heading.
    Node(Uuid),
    /// The "Plan" label before a group's plan steps.
    PlanLabel(Uuid),
    /// `changes[ix]`.
    Change(usize),
}

/// The changes `filter` lets through, grouped by node, in the order
/// `net_changes` gives.
pub(crate) fn display_rows(changes: &[NetChange], filter: &StatusFilter) -> Vec<DisplayRow> {
    let mut rows = Vec::new();
    let mut group: Option<Option<Uuid>> = None;
    let mut plan_labelled = false;
    for (ix, change) in changes.iter().enumerate() {
        if !change_shows(filter, change) {
            continue;
        }
        let node = node_of(change);
        if group != Some(node) {
            group = Some(node);
            plan_labelled = false;
            if let Some(node) = node {
                rows.push(DisplayRow::Node(node));
            }
        }
        if change.entity == ItemEntity::PlanStep && !plan_labelled {
            plan_labelled = true;
            if let Some(node) = node {
                rows.push(DisplayRow::PlanLabel(node));
            }
        }
        rows.push(DisplayRow::Change(ix));
    }
    rows
}

/// A reversal waiting for the user to confirm it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingReverse {
    pub action_ids: Vec<i64>,
    /// Items changed since the conversation last touched them.
    pub conflicts: Vec<NetChange>,
    /// Unselected changes that must be reversed too.
    pub dependents: Vec<NetChange>,
}

/// One field that differs between two states of an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldDiff {
    pub label: &'static str,
    pub before: String,
    pub after: String,
}

/// The fields that differ between `before` and `current`. Empty unless both
/// exist (an added or deleted item shows its full text instead).
pub(crate) fn field_diffs(
    before: Option<&EntitySnapshot>,
    current: Option<&EntitySnapshot>,
    titles: &HashMap<Uuid, String>,
) -> Vec<FieldDiff> {
    let node = |id: &Uuid| titles.get(id).cloned().unwrap_or_else(|| short_id(*id));
    let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "(none)".into());
    let ids = |v: &[Uuid]| {
        if v.is_empty() {
            "(none)".to_string()
        } else {
            v.iter()
                .map(|id| short_id(*id))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let mut out = Vec::new();
    let mut field = |label, before: String, after: String| {
        if before != after {
            out.push(FieldDiff {
                label,
                before,
                after,
            });
        }
    };
    use EntitySnapshot as S;
    match (before, current) {
        (
            Some(S::Node {
                title: t0,
                list_id: l0,
                parent_id: p0,
                ordinal: o0,
            }),
            Some(S::Node {
                title: t1,
                list_id: l1,
                parent_id: p1,
                ordinal: o1,
            }),
        ) => {
            let parent = |p: &Option<Uuid>| p.as_ref().map_or("(top level)".into(), node);
            field("Title", t0.clone(), t1.clone());
            field("List", short_id(*l0), short_id(*l1));
            field("Parent", parent(p0), parent(p1));
            field("Position", (o0 + 1).to_string(), (o1 + 1).to_string());
        }
        (
            Some(S::Obligation {
                node_id: n0,
                kind: k0,
                section: s0,
                body: b0,
                phase: ph0,
                ordinal: o0,
                ..
            }),
            Some(S::Obligation {
                node_id: n1,
                kind: k1,
                section: s1,
                body: b1,
                phase: ph1,
                ordinal: o1,
                ..
            }),
        ) => {
            field("Text", b0.clone(), b1.clone());
            field("Kind", k0.clone(), k1.clone());
            field("Section", opt(s0), opt(s1));
            field("Phase", ph0.clone(), ph1.clone());
            field("Node", node(n0), node(n1));
            field("Position", o0.to_string(), o1.to_string());
        }
        (
            Some(S::PlanStep {
                node_id: n0,
                ordinal: o0,
                body: b0,
                status: st0,
                note: no0,
                reason: r0,
                depends_on: d0,
                satisfies: sa0,
            }),
            Some(S::PlanStep {
                node_id: n1,
                ordinal: o1,
                body: b1,
                status: st1,
                note: no1,
                reason: r1,
                depends_on: d1,
                satisfies: sa1,
            }),
        ) => {
            field("Text", b0.clone(), b1.clone());
            field("Status", st0.clone(), st1.clone());
            field("Note", opt(no0), opt(no1));
            let reason = |r: &Option<HandoffReason>| opt(&r.as_ref().map(HandoffReason::describe));
            field("Reason", reason(r0), reason(r1));
            field("Depends on", ids(d0), ids(d1));
            field("Satisfies", ids(sa0), ids(sa1));
            field("Node", node(n0), node(n1));
            field("Position", o0.to_string(), o1.to_string());
        }
        _ => {}
    }
    out
}

pub(crate) fn obligation_of(change: &NetChange) -> Option<NodeObligation> {
    match snapshot_of(change)? {
        EntitySnapshot::Obligation {
            node_id,
            kind,
            section,
            body,
            phase,
            ordinal,
            visual_design_path,
        } => Some(NodeObligation {
            id: change.id,
            node_id: *node_id,
            kind: kind.clone(),
            ordinal: *ordinal,
            section: section.clone(),
            body: body.clone(),
            phase: phase.clone(),
            visual_design_path: visual_design_path.clone(),
        }),
        _ => None,
    }
}

pub(crate) fn plan_step_of(change: &NetChange) -> Option<PlanStep> {
    match snapshot_of(change)? {
        EntitySnapshot::PlanStep {
            node_id,
            ordinal,
            body,
            status,
            note,
            reason,
            ..
        } => Some(PlanStep {
            id: change.id,
            node_id: *node_id,
            ordinal: *ordinal,
            body: body.clone(),
            status: status.clone(),
            note: note.clone(),
            reason: reason.clone(),
        }),
        _ => None,
    }
}

impl ConversationView {
    /// Toggle `toggle` in the change-set filter, or clear it (`None`, "All").
    pub(super) fn set_change_filter(&mut self, toggle: Option<&str>, cx: &mut Context<Self>) {
        match toggle {
            Some(toggle) => self.change_filter.toggle(toggle),
            None => {
                self.change_filter.clear();
            }
        }
        self.pane = Pane::ChangeSet;
        self.clamp_cursor();
        self.scroll_to_cursor = true;
        cx.notify();
    }

    pub(super) fn toggle_selected(&mut self, key: ChangeKey, cx: &mut Context<Self>) {
        if !self.selected.remove(&key) {
            self.selected.insert(key);
        }
        cx.notify();
    }

    /// R: the selection, or else the highlighted change.
    pub(super) fn reverse_selection(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<ChangeKey> = if self.selected.is_empty() {
            self.cursor.into_iter().collect()
        } else {
            self.data
                .changes
                .iter()
                .map(key_of)
                .filter(|k| self.selected.contains(k))
                .collect()
        };
        self.reverse_keys(keys, cx);
    }

    /// Every change still in effect.
    pub(super) fn reverse_all(&mut self, cx: &mut Context<Self>) {
        let keys = self
            .data
            .changes
            .iter()
            .filter(|c| c.op != NetOp::Reversed)
            .map(key_of)
            .collect();
        self.reverse_keys(keys, cx);
    }

    /// Reverse (or, for reversed changes, re-apply) `keys`.
    pub(super) fn reverse_keys(&mut self, keys: Vec<ChangeKey>, cx: &mut Context<Self>) {
        let action_ids: Vec<i64> = self
            .data
            .changes
            .iter()
            .filter(|c| keys.contains(&key_of(c)))
            .flat_map(|c| c.action_ids.iter().copied())
            .collect();
        if action_ids.is_empty() {
            return;
        }
        self.reverse(action_ids, false, false, cx);
    }

    fn reverse(
        &mut self,
        action_ids: Vec<i64>,
        include_dependents: bool,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(conversation_id) = self.conversation_id else {
            return;
        };
        let Some(value) = self.command(InterviewCommand::ReverseConversationActions {
            conversation_id,
            action_ids: action_ids.clone(),
            include_dependents,
            force,
        }) else {
            cx.notify();
            return;
        };
        match serde_json::from_value::<ReverseOutcome>(value) {
            Ok(ReverseOutcome::Applied { new_action_ids }) => {
                self.confirm = None;
                self.selected.clear();
                let n = new_action_ids.len();
                self.status_line =
                    format!("Reversed {n} action{}", if n == 1 { "" } else { "s" }).into();
            }
            Ok(ReverseOutcome::NeedsConfirmation {
                conflicts,
                dependents,
            }) => {
                self.confirm = Some(PendingReverse {
                    action_ids,
                    conflicts,
                    dependents,
                });
            }
            Err(err) => self.error = Some(format!("Unexpected reverse result: {err}").into()),
        }
        self.reload();
        cx.notify();
    }

    /// Re-issue the pending reversal with what the user confirmed.
    pub(super) fn confirm_reverse(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.confirm.take() else {
            return;
        };
        self.reverse(
            pending.action_ids,
            !pending.dependents.is_empty(),
            !pending.conflicts.is_empty(),
            cx,
        );
    }

    pub(super) fn clear_flag(&mut self, key: ChangeKey, cx: &mut Context<Self>) {
        let Some(conversation_id) = self.conversation_id else {
            return;
        };
        if self.change(key).is_none_or(|c| c.flag.is_none()) {
            return;
        }
        self.command(InterviewCommand::UnflagConversationItem {
            conversation_id,
            entity: key.0,
            entity_id: key.1,
        });
        self.reload();
        cx.notify();
    }

    /// E: edit the highlighted change's text in place.
    pub(super) fn start_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.cursor else {
            return;
        };
        let Some(text) = self
            .change(key)
            .and_then(|c| c.current.as_ref())
            .map(|s| s.text().to_string())
        else {
            return;
        };
        self.pane = Pane::ChangeSet;
        self.picker = None;
        self.editing = Some(key);
        self.edit_input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.edit_input.update(cx, |input, cx| {
                // Focus with the caret after the existing text.
                let end = input.text().offset_to_position(input.text().len());
                input.set_cursor_position(end, window, cx);
            });
        });
    }

    pub(super) fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((entity, id)), Some(conversation_id)) = (self.editing, self.conversation_id)
        else {
            return;
        };
        let text = self.edit_input.read(cx).value().trim().to_string();
        if text.is_empty() {
            self.error = Some("The text can't be empty; reverse the change instead".into());
            cx.notify();
            return;
        }
        let mutation = match entity {
            ItemEntity::Node => OutlineMutation::UpdateNodeTitle {
                node_id: id,
                title: text,
            },
            ItemEntity::Obligation => OutlineMutation::UpdateObligationBody {
                obligation_id: id,
                body: text,
            },
            ItemEntity::PlanStep => OutlineMutation::UpdatePlanStepBody {
                step_id: id,
                body: text,
            },
            // Changed from the node's capability settings, not edited here.
            ItemEntity::Capabilities => return,
        };
        if self
            .command(InterviewCommand::ConversationEdit {
                conversation_id,
                mutation,
            })
            .is_some()
        {
            self.editing = None;
            self.focus_handle.focus(window, cx);
        }
        self.reload();
        cx.notify();
    }

    pub(super) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    // ----- rendering -------------------------------------------------------

    pub(super) fn render_change_set(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.pane == Pane::ChangeSet;
        let rows = display_rows(&self.data.changes, &self.change_filter);
        if std::mem::take(&mut self.scroll_to_cursor)
            && let Some(cursor) = self.cursor
            && let Some(ix) = rows.iter().position(|r| {
                matches!(r, DisplayRow::Change(ix)
                    if self.data.changes.get(*ix).map(key_of) == Some(cursor))
            })
        {
            self.change_scroll.scroll_to_item(ix);
        }

        let mut list: Vec<AnyElement> = Vec::new();
        for row in &rows {
            list.push(match *row {
                DisplayRow::Node(node) => {
                    let title = self
                        .data
                        .node_titles
                        .get(&node)
                        .cloned()
                        .unwrap_or_default();
                    style::text_dense_muted(h_flex())
                        .pt(style::space::RELATED)
                        .px(style::space::RELATED)
                        .gap(style::space::INLINE)
                        .child(Icon::new(IconName::Folder).xsmall())
                        .child(
                            selectable_text(
                                ElementId::Name(format!("change-group-{node}").into()),
                                title,
                                window,
                                cx,
                            )
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .overflow_hidden(),
                        )
                        .into_any_element()
                }
                DisplayRow::PlanLabel(_) => style::text_dense_muted(div())
                    .px(style::space::RELATED)
                    .pt(style::space::INLINE)
                    .child("Plan")
                    .into_any_element(),
                DisplayRow::Change(ix) => self.render_change_row(ix, window, cx),
            });
        }
        if list.is_empty() {
            list.push(
                style::empty_message(div())
                    .p(style::space::INSET)
                    .child(if self.data.changes.is_empty() {
                        "No changes yet"
                    } else {
                        "No changes in the chosen filters"
                    })
                    .into_any_element(),
            );
        }

        let filter = render_status_filter(
            "change-set",
            &change_counts(&self.data.changes),
            &self.change_filter,
            |this: &mut Self, toggle, _, cx| this.set_change_filter(toggle, cx),
            cx,
        );

        let selected = self.selected.len();
        let can_reverse_all = self.data.changes.iter().any(|c| c.op != NetOp::Reversed);
        let hint: SharedString = if selected > 0 {
            format!("{selected} selected").into()
        } else {
            "Enter show all · Space select · R reverse · E edit · F clear flag · → links · Ctrl+I context · Ctrl+J talk about it"
                .into()
        };

        v_flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .child(
                style::panel_header(h_flex())
                    .items_center()
                    .child(
                        if active {
                            style::text_title(div())
                        } else {
                            style::text_muted(div())
                        }
                        .flex_shrink_0()
                        .child("Changes"),
                    ),
            )
            .children(filter)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        v_flex()
                            .id("change-set-list")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.change_scroll)
                            .px(style::space::RELATED)
                            .pb(style::space::RELATED)
                            .children(list),
                    )
                    .child(
                        // A narrow strip, so the scrollbar's click handler
                        // does not cover the rows.
                        div()
                            .occlude()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(gpui::px(16.))
                            .child(Scrollbar::vertical(&self.change_scroll)),
                    ),
            )
            .child(
                style::panel_footer(h_flex())
                    .items_center()
                    .child(
                        style::text_dense_muted(div())
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(hint),
                    )
                    .child(
                        Button::new("reverse-selected")
                            .label("Reverse selected")
                            .small()
                            .disabled(selected == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.reverse_selection(cx))),
                    )
                    .child(
                        Button::new("reverse-all")
                            .label("Reverse all")
                            .small()
                            .tooltip("Shift+R")
                            .disabled(!can_reverse_all)
                            .on_click(cx.listener(|this, _, _, cx| this.reverse_all(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_change_row(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let change = self.data.changes[ix].clone();
        let key = key_of(&change);
        let highlighted = self.cursor == Some(key);
        let host = self.host.clone();

        let checkbox = Checkbox::new(("change-select", ix))
            .checked(self.selected.contains(&key))
            .tab_stop(false)
            .on_click({
                let host = host.clone();
                move |_, _, cx| host.push(ChangeAction::Toggle(key), cx)
            });
        let expanded = self.expanded.contains(&key);
        let disclosure = style::text_muted(div())
            .id(("change-expand", ix))
            .flex_shrink_0()
            .flex()
            .items_center()
            .cursor_pointer()
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall(),
            )
            .tooltip(|window, cx| Tooltip::new("Show all of it (Enter)").build(window, cx))
            .on_mouse_down(MouseButton::Left, {
                let host = host.clone();
                move |_, _, cx| {
                    host.push(ChangeAction::Select(ix), cx);
                    host.push(ChangeAction::Expand(key), cx);
                    cx.stop_propagation();
                }
            });
        let leading = h_flex()
            .flex_shrink_0()
            .items_center()
            .gap(style::space::INLINE)
            .child(disclosure)
            .child(checkbox)
            .child(op_icon(("change-op", ix), change.op))
            .into_any_element();

        let mut actions = Vec::new();
        let action = |id: &str, label: &str, icon: IconName, act: ChangeAction| {
            let host = host.clone();
            RowAction::new(id.to_string(), label.to_string(), move |_, cx| {
                host.push(act.clone(), cx)
            })
            .icon(icon)
        };
        if change.current.is_some()
            && change.op != NetOp::Reversed
            && change.entity != ItemEntity::Capabilities
        {
            actions.push(action(
                "edit",
                "Edit",
                IconName::Pencil,
                ChangeAction::Edit(key),
            ));
        }
        actions.push(if change.op == NetOp::Reversed {
            action(
                "reverse",
                "Re-apply",
                IconName::Redo2,
                ChangeAction::Reverse(key),
            )
        } else {
            action(
                "reverse",
                "Reverse",
                IconName::Undo2,
                ChangeAction::Reverse(key),
            )
        });
        if change.flag.is_some() {
            actions.push(action(
                "clear-flag",
                "Clear flag",
                IconName::FlagOff,
                ChangeAction::ClearFlag(key),
            ));
        }
        actions.push(
            action(
                "context",
                "Context",
                IconName::PanelRight,
                ChangeAction::Context(key),
            )
            .tooltip("Show it in its list (Ctrl+I)"),
        );
        actions.push(
            action(
                "talk",
                "Talk",
                IconName::MessagesSquare,
                ChangeAction::Talk(key),
            )
            .tooltip("Talk about this (Ctrl+J)"),
        );

        let opts = RowOptions {
            compact: true,
            wrap: expanded,
            leading: Some(leading),
            trailing_context: self.render_context_refs(ix, &change, window, cx),
            actions,
            detail: None,
            struck: matches!(change.op, NetOp::Deleted | NetOp::Reversed),
            flag: change.flag.clone(),
        };
        let editor = (self.editing == Some(key)).then_some(&self.edit_input);
        let row = match change.entity {
            ItemEntity::Node => {
                let title = snapshot_of(&change).map(|s| s.text()).unwrap_or_default();
                node_row(
                    NodeRowProps {
                        node_id: change.id,
                        title: &title,
                        row_ix: ix,
                        highlighted,
                        editor,
                        editable: true,
                    },
                    &host,
                    opts,
                    window,
                    cx,
                )
            }
            ItemEntity::Capabilities => {
                let title = capabilities_title(&change);
                node_row(
                    NodeRowProps {
                        node_id: change.id,
                        title: &title,
                        row_ix: ix,
                        highlighted,
                        editor: None,
                        editable: false,
                    },
                    &host,
                    opts,
                    window,
                    cx,
                )
            }
            ItemEntity::Obligation => match obligation_of(&change) {
                Some(obligation) => obligation_row(
                    ObligationRowProps {
                        obligation: &obligation,
                        row_ix: ix,
                        highlighted,
                        editor,
                    },
                    &host,
                    opts,
                    window,
                    cx,
                ),
                None => div().into_any_element(),
            },
            ItemEntity::PlanStep => match plan_step_of(&change) {
                Some(step) => plan_step_row(
                    PlanStepRowProps {
                        step: &step,
                        depends_on: &[],
                        satisfies: &[],
                        row_ix: ix,
                        highlighted,
                        editor,
                        // A change-set row is one compact line: the status
                        // dropdown belongs to the lists that work the plan.
                        status_menu: None,
                    },
                    &host,
                    opts,
                    window,
                    cx,
                ),
                None => div().into_any_element(),
            },
        };
        match expanded
            .then(|| self.render_detail(ix, &change, window, cx))
            .flatten()
        {
            Some(detail) => v_flex()
                .w_full()
                .child(row)
                .child(detail)
                .into_any_element(),
            None => row,
        }
    }

    /// The short context after a row: "from *Web client*". Each label is a
    /// link that shows its target in the context panel; the keyboard's
    /// highlighted link (`self.link`) is drawn highlighted.
    fn render_context_refs(
        &self,
        ix: usize,
        change: &NetChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if change.context.is_empty() {
            return None;
        }
        let highlighted = (self.pane == Pane::ChangeSet && self.cursor == Some(key_of(change)))
            .then_some(self.link)
            .flatten();
        let mut el = h_flex().items_center().gap(style::space::INLINE);
        for (n, reference) in change.context.iter().enumerate() {
            let label = link_label(&reference.target).to_string();
            let host = self.host.clone();
            let link = div()
                .id(ElementId::Name(format!("change-link-{ix}-{n}").into()))
                .flex_shrink_0()
                .px(style::space::HAIRLINE)
                .cursor_pointer()
                // Before the row's own mouse-down selects it, so the link
                // wins over the row's item in the context panel.
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    host.push(ChangeAction::Select(ix), cx);
                    host.push(ChangeAction::OpenLink { ix, link: n }, cx);
                    cx.stop_propagation();
                })
                .when(highlighted == Some(n), style::highlighted)
                .child(style::text_link(
                    selectable_text(
                        ElementId::Name(format!("change-ref-{ix}-{n}").into()),
                        label,
                        window,
                        cx,
                    )
                    .whitespace_nowrap()
                    .underline(),
                ));
            el = el.child(reference.phrase.clone()).child(link);
        }
        Some(el.into_any_element())
    }

    /// Under an expanded row (whose own text now wraps in full): what
    /// changed field by field, and why the agent was unsure. `None` when
    /// there is neither.
    fn render_detail(
        &self,
        ix: usize,
        change: &NetChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let diffs = field_diffs(
            change.before.as_ref(),
            change.current.as_ref(),
            &self.data.node_titles,
        );
        if diffs.is_empty() && change.flag.is_none() {
            return None;
        }
        let mut detail = v_flex()
            .w_full()
            .pl(style::space::SECTION)
            .pr(style::space::RELATED)
            .pb(style::space::RELATED)
            .gap(style::space::INLINE)
            .child(style::text_dense_muted(div()).child(super::header::op_summary(change.op)));
        for (n, diff) in diffs.into_iter().enumerate() {
            detail = detail.child(
                v_flex()
                    .w_full()
                    .gap(style::space::HAIRLINE)
                    .child(style::text_dense_muted(div()).child(diff.label))
                    .child(
                        style::text_muted(selectable_text(
                            ElementId::Name(format!("change-before-{ix}-{n}").into()),
                            diff.before,
                            window,
                            cx,
                        ))
                        .w_full()
                        .line_through(),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap(style::space::INLINE)
                            .child(
                                style::text_muted(div())
                                    .flex_shrink_0()
                                    .child(Icon::new(IconName::ArrowRight).xsmall()),
                            )
                            .child(
                                selectable_text(
                                    ElementId::Name(format!("change-after-{ix}-{n}").into()),
                                    diff.after,
                                    window,
                                    cx,
                                )
                                .flex_1()
                                .min_w_0(),
                            ),
                    ),
            );
        }
        if let Some(reason) = change.flag.clone() {
            detail = detail.child(
                v_flex()
                    .gap(style::space::HAIRLINE)
                    .child(style::text_dense_muted(div()).child("Unsure because"))
                    .child(selectable_text(
                        ("change-flag-reason", ix),
                        reason,
                        window,
                        cx,
                    )),
            );
        }
        Some(detail.into_any_element())
    }

    /// The confirmation for a reversal that needs one, over the whole view.
    pub(super) fn render_confirm(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let pending = self.confirm.as_ref()?;
        let list = |id: &str,
                    heading: &'static str,
                    changes: &[NetChange],
                    window: &mut Window,
                    cx: &mut Context<Self>| {
            (!changes.is_empty()).then(|| {
                v_flex()
                    .gap(style::space::INLINE)
                    .child(style::text_dense_muted(div()).child(heading))
                    .children(changes.iter().enumerate().map(|(n, change)| {
                        let text = snapshot_of(change)
                            .map(|s| s.text().to_string())
                            .unwrap_or_default();
                        style::row(h_flex())
                            .items_center()
                            .child(op_icon(
                                ElementId::Name(format!("{id}-op-{n}").into()),
                                change.op,
                            ))
                            .child(style::text_muted(div()).child(short_id(change.id)))
                            .child(
                                selectable_text(
                                    ElementId::Name(format!("{id}-text-{n}").into()),
                                    text,
                                    window,
                                    cx,
                                )
                                .min_w_0()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .overflow_hidden(),
                            )
                    }))
            })
        };
        let conflicts = list(
            "confirm-conflict",
            "Changed since",
            &pending.conflicts,
            window,
            cx,
        );
        let dependents = list(
            "confirm-dependent",
            "Also reversed",
            &pending.dependents,
            window,
            cx,
        );
        Some(
            style::scrim(div())
                .id("reverse-confirm")
                .occlude()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    style::floating_panel(v_flex())
                        .w(gpui::px(480.))
                        .max_h_full()
                        .child(style::text_title(div()).child("Reverse anyway?"))
                        .children(conflicts)
                        .children(dependents)
                        .child(
                            h_flex()
                                .justify_end()
                                .gap(style::space::RELATED)
                                .child(
                                    style::text_dense_muted(div())
                                        .flex_1()
                                        .child("Enter reverses · Esc cancels"),
                                )
                                .child(
                                    Button::new("reverse-confirm-cancel")
                                        .label("Cancel")
                                        .ghost()
                                        .small()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.confirm = None;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("reverse-confirm-ok")
                                        .label("Reverse")
                                        .primary()
                                        .small()
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.confirm_reverse(cx)),
                                        ),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}
