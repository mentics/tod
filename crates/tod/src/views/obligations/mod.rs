//! Obligations panel — edit direct requirements/constraints for a Spec node.

mod delegate;

use tod_store::fleet::FleetStore;
use tod_store::outline::{
    KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation, OutlineMutation, ReorderDirection,
};
use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::key_context;
use crate::ui::list::{
    ListArrowDown, ListArrowUp, ListEnd, ListHome, ListPageDown, ListPageUp, viewport_row_count,
};
use delegate::{
    NO_SECTION, ObligationListDelegate, ObligationRow, RowAction, obligation_section,
    section_row_key,
};
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, ScrollHandle, StatefulInteractiveElement,
    Styled, Subscription, Window, actions, div, prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use uuid::Uuid;

const OBLIGATIONS_CONTEXT: &str = "Obligations";

actions!(
    obligations,
    [
        ObligationsClose,
        ObligationsEnter,
        ObligationsCreateBelow,
        ObligationsCreateAbove,
        ObligationsCreateChild,
        ObligationsMoveUp,
        ObligationsMoveDown,
        ObligationsEdit,
        ObligationsEditNavUp,
        ObligationsEditNavDown,
        ObligationsCollapse,
        ObligationsExpand,
        ObligationsDelete,
    ]
);

pub fn register_obligations_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(OBLIGATIONS_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("up", ListArrowUp, context),
        KeyBinding::new("down", ListArrowDown, context),
        KeyBinding::new("pageup", ListPageUp, context),
        KeyBinding::new("pagedown", ListPageDown, context),
        KeyBinding::new("home", ListHome, context),
        KeyBinding::new("end", ListEnd, context),
        KeyBinding::new("enter", ObligationsEnter, context),
        KeyBinding::new("n", ObligationsCreateBelow, context),
        KeyBinding::new("f2", ObligationsEdit, context),
        KeyBinding::new("left", ObligationsCollapse, context),
        KeyBinding::new("right", ObligationsExpand, context),
        KeyBinding::new("shift-enter", ObligationsCreateChild, context),
        KeyBinding::new(
            "shift-enter",
            ObligationsCreateChild,
            Some(key_context::including_input(OBLIGATIONS_CONTEXT)),
        ),
        KeyBinding::new("alt-enter", ObligationsCreateAbove, context),
        KeyBinding::new("ctrl-up", ObligationsMoveUp, context),
        KeyBinding::new("ctrl-down", ObligationsMoveDown, context),
        KeyBinding::new("backspace", ObligationsDelete, context),
        KeyBinding::new("delete", ObligationsDelete, context),
        // Inline edit: Escape closes panel; arrows leave the field and move selection.
        KeyBinding::new(
            "up",
            ObligationsEditNavUp,
            Some(key_context::including_input(OBLIGATIONS_CONTEXT)),
        ),
        KeyBinding::new(
            "down",
            ObligationsEditNavDown,
            Some(key_context::including_input(OBLIGATIONS_CONTEXT)),
        ),
    ]);
    key_context::bind_panel_escape(cx, ObligationsClose, OBLIGATIONS_CONTEXT);
}

#[derive(Debug, Clone)]
pub enum ObligationsEvent {
    Close,
}

pub struct ObligationsView {
    fleet: Arc<FleetStore>,
    node_id: Option<Uuid>,
    title: String,
    items: Vec<NodeObligation>,
    req_collapsed: bool,
    con_collapsed: bool,
    section_collapsed: HashSet<String>,
    focus_handle: FocusHandle,
    delegate: ObligationListDelegate,
    scroll_handle: ScrollHandle,
    selected_index: Option<usize>,
    action_sink: Rc<RefCell<Vec<RowAction>>>,
    editing_id: Option<Uuid>,
    draft_id: Option<Uuid>,
    edit_original_body: Option<String>,
    inline_edit_input: Entity<InputState>,
    pending_inline_commit: bool,
    inline_enter_generation: u64,
    pending_abandon_edit: bool,
    pending_live_refresh: bool,
    selected_key: Option<String>,
    _inline_edit_subscription: Subscription,
}

impl ObligationsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, fleet: Arc<FleetStore>) -> Self {
        let action_sink = Rc::new(RefCell::new(Vec::new()));
        let inline_edit_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Obligation text…"));
        let _inline_edit_subscription = cx.subscribe(&inline_edit_input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.inline_enter_generation = this.inline_enter_generation.saturating_add(1);
                this.pending_inline_commit = true;
                cx.notify();
            } else if matches!(event, InputEvent::Blur) {
                this.pending_abandon_edit = true;
                cx.notify();
            }
        });

        let delegate = ObligationListDelegate::new(Vec::new(), action_sink.clone());

        Self {
            fleet,
            node_id: None,
            title: String::new(),
            items: Vec::new(),
            req_collapsed: false,
            con_collapsed: false,
            section_collapsed: HashSet::new(),
            focus_handle: cx.focus_handle(),
            delegate,
            scroll_handle: ScrollHandle::new(),
            selected_index: None,
            action_sink,
            editing_id: None,
            draft_id: None,
            edit_original_body: None,
            inline_edit_input,
            pending_inline_commit: false,
            inline_enter_generation: 0,
            pending_abandon_edit: false,
            pending_live_refresh: false,
            selected_key: None,
            _inline_edit_subscription,
        }
    }

    pub fn is_open(&self) -> bool {
        self.node_id.is_some()
    }

    pub fn open(
        &mut self,
        node_id: Uuid,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.node_id = Some(node_id);
        self.title = title.to_string();
        self.req_collapsed = false;
        self.con_collapsed = false;
        self.section_collapsed.clear();
        self.clear_inline_edit_state(window, cx);
        self.reload(window, cx);
        self.focus_list(window, cx);
        cx.notify();
    }

    pub fn retarget(
        &mut self,
        node_id: Uuid,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.node_id == Some(node_id) {
            self.title = title.to_string();
            self.reload(window, cx);
            return;
        }
        self.open(node_id, title, window, cx);
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.node_id.is_none() {
            return;
        }
        self.clear_inline_edit_state(window, cx);
        self.node_id = None;
        self.title.clear();
        self.items.clear();
        self.selected_key = None;
        cx.emit(ObligationsEvent::Close);
        cx.notify();
    }

    fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window);
        cx.notify();
    }

    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_id else {
            return;
        };
        let _ = self.fleet.reload_if_stale();
        self.items = self
            .fleet
            .list_obligations_for_node(node_id)
            .unwrap_or_default();
        self.rebuild_visible(window, cx);
    }

    fn flat_rows(&self) -> Vec<ObligationRow> {
        let mut rows = Vec::new();
        let reqs: Vec<_> = self
            .items
            .iter()
            .filter(|o| o.kind == KIND_REQUIREMENT)
            .cloned()
            .collect();
        let cons: Vec<_> = self
            .items
            .iter()
            .filter(|o| o.kind == KIND_CONSTRAINT)
            .cloned()
            .collect();

        Self::append_kind_group(
            &mut rows,
            KIND_REQUIREMENT,
            reqs,
            self.req_collapsed,
            &self.section_collapsed,
        );
        Self::append_kind_group(
            &mut rows,
            KIND_CONSTRAINT,
            cons,
            self.con_collapsed,
            &self.section_collapsed,
        );
        rows
    }

    fn append_kind_group(
        rows: &mut Vec<ObligationRow>,
        kind: &'static str,
        items: Vec<NodeObligation>,
        kind_collapsed: bool,
        section_collapsed: &HashSet<String>,
    ) {
        rows.push(ObligationRow::Group {
            kind,
            collapsed: kind_collapsed,
            count: items.len(),
        });
        if kind_collapsed {
            return;
        }
        for (section, section_items) in Self::group_by_section(items) {
            let key = section_row_key(kind, &section);
            let collapsed = section_collapsed.contains(&key);
            rows.push(ObligationRow::Section {
                kind,
                section: section.clone(),
                collapsed,
                count: section_items.len(),
            });
            if !collapsed {
                for item in section_items {
                    rows.push(ObligationRow::Item { obligation: item });
                }
            }
        }
    }

    fn group_by_section(items: Vec<NodeObligation>) -> Vec<(String, Vec<NodeObligation>)> {
        let mut sections: Vec<(String, Vec<NodeObligation>)> = Vec::new();
        let mut index_by_section: HashMap<String, usize> = HashMap::new();
        for item in items {
            let label = obligation_section(&item).to_string();
            if let Some(ix) = index_by_section.get(&label).copied() {
                sections[ix].1.push(item);
            } else {
                index_by_section.insert(label.clone(), sections.len());
                sections.push((label, vec![item]));
            }
        }
        sections
    }

    fn rebuild_visible(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.flat_rows();
        let selected = self.selected_key.clone();
        let selected_ix = selected
            .as_ref()
            .and_then(|key| rows.iter().position(|r| r.key() == *key))
            .or(Some(0).filter(|_| !rows.is_empty()));

        if let Some(ix) = selected_ix {
            self.selected_key = Some(rows[ix].key());
            self.selected_index = Some(ix);
        } else {
            self.selected_key = None;
            self.selected_index = None;
        }

        self.delegate.set_rows(rows);
        self.delegate.set_selected_index(self.selected_index);
        self.delegate.set_inline_edit(
            self.editing_id.map(|id| id.to_string()),
            self.inline_edit_input.clone(),
        );
        if let Some(ix) = selected_ix {
            self.scroll_handle.scroll_to_top_of_item(ix);
        }
        cx.notify();
    }

    fn select_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let key = self.delegate.rows().get(row_ix).map(|r| r.key());
        if self.selected_index != Some(row_ix) {
            if self.editing_id.is_some() {
                self.pending_abandon_edit = true;
            }
            self.selected_index = Some(row_ix);
            self.selected_key = key;
            self.delegate.set_selected_index(self.selected_index);
            self.scroll_handle.scroll_to_item(row_ix);
            cx.notify();
        }
    }

    fn select_parent_group(&mut self, kind: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_key = Some(format!("group:{kind}"));
        self.rebuild_visible(window, cx);
        self.focus_list(window, cx);
    }

    fn select_parent_section(
        &mut self,
        kind: &str,
        section: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_key = Some(section_row_key(kind, section));
        self.rebuild_visible(window, cx);
        self.focus_list(window, cx);
    }

    fn first_item_in_scope(&self, kind: &str, section: Option<&str>) -> Option<Uuid> {
        self.items
            .iter()
            .filter(|o| o.kind == kind && section.map_or(true, |s| obligation_section(o) == s))
            .min_by_key(|o| o.ordinal)
            .map(|o| o.id)
    }

    fn last_item_in_scope(&self, kind: &str, section: Option<&str>) -> Option<Uuid> {
        self.items
            .iter()
            .filter(|o| o.kind == kind && section.map_or(true, |s| obligation_section(o) == s))
            .max_by_key(|o| o.ordinal)
            .map(|o| o.id)
    }

    fn selected_row(&self) -> Option<ObligationRow> {
        self.delegate.selected_row().cloned().or_else(|| {
            let key = self.selected_key.as_ref()?;
            self.delegate
                .rows()
                .iter()
                .find(|r| r.key() == *key)
                .cloned()
        })
    }

    fn clear_inline_edit_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_id = None;
        self.draft_id = None;
        self.edit_original_body = None;
        self.inline_edit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.sync_delegate_editing(cx);
    }

    fn sync_delegate_editing(&mut self, cx: &mut Context<Self>) {
        self.delegate.set_inline_edit(
            self.editing_id.map(|id| id.to_string()),
            self.inline_edit_input.clone(),
        );
        cx.notify();
    }

    fn is_editing(&self) -> bool {
        self.editing_id.is_some()
    }

    fn is_draft_edit(&self) -> bool {
        match (self.draft_id, self.editing_id) {
            (Some(draft), Some(editing)) => draft == editing,
            _ => false,
        }
    }

    fn edit_body(&self, cx: &Context<Self>) -> String {
        self.inline_edit_input
            .read(cx)
            .text()
            .to_string()
            .trim()
            .to_string()
    }

    fn start_inline_edit(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let body = self
            .items
            .iter()
            .find(|o| o.id == id)
            .map(|o| o.body.clone())
            .unwrap_or_default();
        self.editing_id = Some(id);
        self.edit_original_body = Some(body.clone());
        self.selected_key = Some(id.to_string());
        self.inline_edit_input.update(cx, |input, cx| {
            input.set_value(&body, window, cx);
            input.focus(window, cx);
        });
        self.rebuild_visible(window, cx);
    }

    fn abandon_inline_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        force_delete_draft: bool,
    ) {
        self.pending_inline_commit = false;
        self.inline_enter_generation = self.inline_enter_generation.saturating_add(1);
        let Some(editing_id) = self.editing_id else {
            return;
        };
        let body = self.edit_body(cx);
        let is_draft = self.is_draft_edit();

        if is_draft && (force_delete_draft || body.is_empty()) {
            self.clear_inline_edit_state(window, cx);
            let _ = self
                .fleet
                .enqueue_outline(OutlineMutation::DeleteObligation {
                    obligation_id: editing_id,
                });
            let _ = self.fleet.writer().flush();
            self.reload(window, cx);
            self.focus_list(window, cx);
            return;
        }

        if is_draft && !body.is_empty() {
            let _ = self.commit_inline_edit(window, cx);
            return;
        }

        if let Some(original) = self.edit_original_body.take() {
            if let Some(item) = self.items.iter_mut().find(|o| o.id == editing_id) {
                item.body = original;
            }
        }
        self.clear_inline_edit_state(window, cx);
        self.rebuild_visible(window, cx);
        self.focus_list(window, cx);
    }

    fn commit_inline_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editing_id) = self.editing_id else {
            return false;
        };
        let body = self.edit_body(cx);
        if body.is_empty() {
            if self.is_draft_edit() {
                self.clear_inline_edit_state(window, cx);
                let _ = self
                    .fleet
                    .enqueue_outline(OutlineMutation::DeleteObligation {
                        obligation_id: editing_id,
                    });
                let _ = self.fleet.writer().flush();
                self.reload(window, cx);
                self.focus_list(window, cx);
                return true;
            }
            crate::ui::toast::error_toast(window, cx, "Obligation cannot be empty");
            self.inline_edit_input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
            return false;
        }
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::UpdateObligationBody {
                obligation_id: editing_id,
                body: body.clone(),
            })
        {
            crate::ui::toast::error_toast(window, cx, format!("Save failed: {err}"));
            return false;
        }
        if let Err(err) = self.fleet.writer().flush() {
            crate::ui::toast::error_toast(window, cx, format!("Save failed: {err}"));
            return false;
        }
        if let Some(item) = self.items.iter_mut().find(|o| o.id == editing_id) {
            item.body = body;
        }
        self.draft_id = None;
        self.clear_inline_edit_state(window, cx);
        self.selected_key = Some(editing_id.to_string());
        self.reload(window, cx);
        self.focus_list(window, cx);
        true
    }

    fn create_in_kind(
        &mut self,
        kind: &str,
        after_id: Option<Uuid>,
        before: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(node_id) = self.node_id else {
            return;
        };
        if kind == KIND_REQUIREMENT {
            self.req_collapsed = false;
        } else {
            self.con_collapsed = false;
        }
        self.section_collapsed
            .remove(&section_row_key(kind, NO_SECTION));
        let obligation_id = Uuid::new_v4();
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::CreateObligation {
                obligation_id: Some(obligation_id),
                node_id,
                kind: kind.to_string(),
                after_id,
                before,
                body: String::new(),
            })
        {
            crate::ui::toast::error_toast(window, cx, format!("Create failed: {err}"));
            return;
        }
        if let Err(err) = self.fleet.writer().flush() {
            crate::ui::toast::error_toast(window, cx, format!("Create failed: {err}"));
            return;
        }
        self.draft_id = Some(obligation_id);
        self.reload(window, cx);
        self.start_inline_edit(obligation_id, window, cx);
    }

    fn create_relative(&mut self, before: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.selected_row() {
            Some(ObligationRow::Group { kind, .. }) => {
                if before {
                    self.create_in_kind(kind, None, true, window, cx);
                } else {
                    match self.first_item_in_scope(kind, None) {
                        Some(id) => self.create_in_kind(kind, Some(id), true, window, cx),
                        None => self.create_in_kind(kind, None, false, window, cx),
                    }
                }
            }
            Some(ObligationRow::Section { kind, section, .. }) => {
                self.ensure_section_expanded(kind, &section, window, cx);
                if before {
                    match self.first_item_in_scope(kind, Some(&section)) {
                        Some(id) => self.create_in_kind(kind, Some(id), true, window, cx),
                        None => self.create_in_kind(kind, None, false, window, cx),
                    }
                } else {
                    match self.last_item_in_scope(kind, Some(&section)) {
                        Some(id) => self.create_in_kind(kind, Some(id), false, window, cx),
                        None => self.create_in_kind(kind, None, false, window, cx),
                    }
                }
            }
            Some(ObligationRow::Item { obligation }) => {
                self.create_in_kind(&obligation.kind, Some(obligation.id), before, window, cx);
            }
            None => {
                self.create_in_kind(KIND_REQUIREMENT, None, false, window, cx);
            }
        }
    }

    fn on_smart_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            let saved = self.editing_id;
            if !self.commit_inline_edit(window, cx) {
                return;
            }
            if let Some(id) = saved {
                self.selected_key = Some(id.to_string());
                self.create_relative(false, window, cx);
            }
            return;
        }
        match self.selected_row() {
            Some(ObligationRow::Item { obligation }) => {
                self.start_inline_edit(obligation.id, window, cx);
            }
            Some(ObligationRow::Group { .. }) | Some(ObligationRow::Section { .. }) | None => {
                self.create_relative(false, window, cx);
            }
        }
    }

    fn toggle_group(&mut self, kind: &str, window: &mut Window, cx: &mut Context<Self>) {
        if kind == KIND_REQUIREMENT {
            self.req_collapsed = !self.req_collapsed;
        } else if kind == KIND_CONSTRAINT {
            self.con_collapsed = !self.con_collapsed;
        }
        self.rebuild_visible(window, cx);
    }

    fn toggle_section(
        &mut self,
        kind: &str,
        section: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = section_row_key(kind, section);
        if self.section_collapsed.contains(&key) {
            self.section_collapsed.remove(&key);
        } else {
            self.section_collapsed.insert(key);
        }
        self.rebuild_visible(window, cx);
    }

    fn set_group_collapsed(
        &mut self,
        kind: &str,
        collapsed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if kind == KIND_REQUIREMENT {
            self.req_collapsed = collapsed;
        } else if kind == KIND_CONSTRAINT {
            self.con_collapsed = collapsed;
        }
        self.rebuild_visible(window, cx);
    }

    fn set_section_collapsed(
        &mut self,
        kind: &str,
        section: &str,
        collapsed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = section_row_key(kind, section);
        if collapsed {
            self.section_collapsed.insert(key);
        } else {
            self.section_collapsed.remove(&key);
        }
        self.rebuild_visible(window, cx);
    }

    fn ensure_section_expanded(
        &mut self,
        kind: &str,
        section: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_group_collapsed(kind, false, window, cx);
        self.set_section_collapsed(kind, section, false, window, cx);
    }

    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ObligationRow::Item { obligation }) = self.selected_row() else {
            return;
        };
        let id = obligation.id;
        let kind = obligation.kind.clone();
        let section = obligation_section(&obligation).to_string();
        let next_key = self
            .items
            .iter()
            .filter(|o| o.kind == kind && obligation_section(o) == section && o.id != id)
            .find(|o| o.ordinal > obligation.ordinal)
            .map(|o| o.id.to_string())
            .or_else(|| {
                self.items
                    .iter()
                    .filter(|o| o.kind == kind && obligation_section(o) == section && o.id != id)
                    .last()
                    .map(|o| o.id.to_string())
            })
            .or_else(|| Some(section_row_key(&kind, &section)));
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::DeleteObligation { obligation_id: id })
        {
            crate::ui::toast::error_toast(window, cx, format!("Delete failed: {err}"));
            return;
        }
        let _ = self.fleet.writer().flush();
        self.selected_key = next_key;
        self.reload(window, cx);
        self.focus_list(window, cx);
    }

    fn move_selected(
        &mut self,
        direction: ReorderDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ObligationRow::Item { obligation }) = self.selected_row() else {
            return;
        };
        let id = obligation.id;
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::ReorderObligation {
                obligation_id: id,
                direction,
            })
        {
            crate::ui::toast::error_toast(window, cx, format!("Move failed: {err}"));
            return;
        }
        let _ = self.fleet.writer().flush();
        self.selected_key = Some(id.to_string());
        self.reload(window, cx);
        self.focus_list(window, cx);
    }

    fn move_selection(&mut self, delta: i32, _window: &mut Window, cx: &mut Context<Self>) {
        let count = self.delegate.rows().len();
        if count == 0 {
            return;
        }
        let current = self.selected_index.unwrap_or(0);
        let next = if delta < 0 {
            current.saturating_sub((-delta) as usize)
        } else {
            (current + delta as usize).min(count.saturating_sub(1))
        };
        self.select_row(next, cx);
    }

    fn drain_row_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let actions: Vec<_> = self.action_sink.borrow_mut().drain(..).collect();
        for action in actions {
            match action {
                RowAction::ToggleGroup { kind } => {
                    self.toggle_group(&kind, window, cx);
                }
                RowAction::ToggleSection { kind, section } => {
                    self.toggle_section(&kind, &section, window, cx);
                }
                RowAction::StartEdit { obligation_id } => {
                    self.start_inline_edit(obligation_id, window, cx);
                }
                RowAction::Select { row_ix } => {
                    self.select_row(row_ix, cx);
                }
            }
        }
    }

    fn on_close(&mut self, _: &ObligationsClose, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            self.abandon_inline_edit(window, cx, true);
            return;
        }
        self.close(window, cx);
    }

    fn on_enter(&mut self, _: &ObligationsEnter, window: &mut Window, cx: &mut Context<Self>) {
        self.on_smart_enter(window, cx);
    }

    fn on_create_below(
        &mut self,
        _: &ObligationsCreateBelow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_editing() {
            return;
        }
        self.create_relative(false, window, cx);
    }

    fn on_create_above(
        &mut self,
        _: &ObligationsCreateAbove,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_editing() {
            return;
        }
        self.create_relative(true, window, cx);
    }

    fn on_create_child(
        &mut self,
        _: &ObligationsCreateChild,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_editing() {
            let _ = self.commit_inline_edit(window, cx);
        }
        match self.selected_row() {
            Some(ObligationRow::Group { kind, .. }) => {
                self.set_group_collapsed(kind, false, window, cx);
                self.create_in_kind(kind, None, false, window, cx);
            }
            Some(ObligationRow::Section { kind, section, .. }) => {
                self.ensure_section_expanded(kind, &section, window, cx);
                match self.last_item_in_scope(kind, Some(&section)) {
                    Some(id) => self.create_in_kind(kind, Some(id), false, window, cx),
                    None => self.create_in_kind(kind, None, false, window, cx),
                }
            }
            _ => self.create_relative(false, window, cx),
        }
    }

    fn on_move_up(&mut self, _: &ObligationsMoveUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selected(ReorderDirection::Up, window, cx);
    }

    fn on_move_down(
        &mut self,
        _: &ObligationsMoveDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selected(ReorderDirection::Down, window, cx);
    }

    fn on_edit(&mut self, _: &ObligationsEdit, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ObligationRow::Item { obligation }) = self.selected_row() {
            self.start_inline_edit(obligation.id, window, cx);
        }
    }

    fn on_edit_nav_up(
        &mut self,
        _: &ObligationsEditNavUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_editing() {
            return;
        }
        self.abandon_inline_edit(window, cx, false);
        self.move_selection(-1, window, cx);
        self.focus_list(window, cx);
    }

    fn on_edit_nav_down(
        &mut self,
        _: &ObligationsEditNavDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_editing() {
            return;
        }
        self.abandon_inline_edit(window, cx, false);
        self.move_selection(1, window, cx);
        self.focus_list(window, cx);
    }

    fn on_collapse(
        &mut self,
        _: &ObligationsCollapse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selected_row() {
            Some(ObligationRow::Group {
                kind, collapsed, ..
            }) if !collapsed => {
                self.set_group_collapsed(kind, true, window, cx);
            }
            Some(ObligationRow::Section {
                kind,
                section,
                collapsed,
                ..
            }) if !collapsed => {
                self.set_section_collapsed(kind, &section, true, window, cx);
            }
            Some(ObligationRow::Section { kind, .. }) => {
                self.select_parent_group(kind, window, cx);
            }
            Some(ObligationRow::Item { obligation }) => {
                self.select_parent_section(
                    &obligation.kind,
                    obligation_section(&obligation),
                    window,
                    cx,
                );
            }
            _ => {}
        }
    }

    fn on_expand(&mut self, _: &ObligationsExpand, window: &mut Window, cx: &mut Context<Self>) {
        match self.selected_row() {
            Some(ObligationRow::Group {
                kind, collapsed, ..
            }) if collapsed => {
                self.set_group_collapsed(kind, false, window, cx);
            }
            Some(ObligationRow::Section {
                kind,
                section,
                collapsed,
                ..
            }) if collapsed => {
                self.set_section_collapsed(kind, &section, false, window, cx);
            }
            _ => {}
        }
    }

    fn on_delete(&mut self, _: &ObligationsDelete, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            return;
        }
        self.delete_selected(window, cx);
    }

    fn on_arrow_up(&mut self, _: &ListArrowUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, window, cx);
    }

    fn on_arrow_down(&mut self, _: &ListArrowDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, window, cx);
    }

    fn on_page_up(&mut self, _: &ListPageUp, window: &mut Window, cx: &mut Context<Self>) {
        let page = viewport_row_count(window.viewport_size().height).max(1);
        self.move_selection(-(page as i32), window, cx);
    }

    fn on_page_down(&mut self, _: &ListPageDown, window: &mut Window, cx: &mut Context<Self>) {
        let page = viewport_row_count(window.viewport_size().height).max(1);
        self.move_selection(page as i32, window, cx);
    }

    fn on_home(&mut self, _: &ListHome, _window: &mut Window, cx: &mut Context<Self>) {
        let count = self.delegate.rows().len();
        if count == 0 {
            return;
        }
        self.select_row(0, cx);
        self.scroll_handle.scroll_to_top_of_item(0);
    }

    fn on_end(&mut self, _: &ListEnd, _window: &mut Window, cx: &mut Context<Self>) {
        let count = self.delegate.rows().len();
        if count == 0 {
            return;
        }
        let last = count - 1;
        self.select_row(last, cx);
        self.scroll_handle.scroll_to_top_of_item(last);
    }
}

impl EventEmitter<ObligationsEvent> for ObligationsView {}

impl Focusable for ObligationsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ObligationsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_live_refresh {
            self.pending_live_refresh = false;
            self.reload(window, cx);
        }
        if self.pending_abandon_edit {
            self.pending_abandon_edit = false;
            self.abandon_inline_edit(window, cx, false);
        }
        if self.pending_inline_commit {
            self.pending_inline_commit = false;
            let generation = self.inline_enter_generation;
            cx.defer_in(window, move |this, window, cx| {
                if this.inline_enter_generation != generation {
                    return;
                }
                this.on_smart_enter(window, cx);
            });
        }
        self.drain_row_actions(window, cx);

        if !self.is_open() {
            return div().into_any_element();
        }

        let theme = cx.theme();
        let border = theme.border;
        let accent = theme.primary;
        let muted = theme.muted_foreground;

        v_flex()
            .key_context(OBLIGATIONS_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .border_l_2()
            .border_color(accent)
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_enter))
            .on_action(cx.listener(Self::on_create_below))
            .on_action(cx.listener(Self::on_create_above))
            .on_action(cx.listener(Self::on_create_child))
            .on_action(cx.listener(Self::on_move_up))
            .on_action(cx.listener(Self::on_move_down))
            .on_action(cx.listener(Self::on_edit))
            .on_action(cx.listener(Self::on_edit_nav_up))
            .on_action(cx.listener(Self::on_edit_nav_down))
            .on_action(cx.listener(Self::on_collapse))
            .on_action(cx.listener(Self::on_expand))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_arrow_up))
            .on_action(cx.listener(Self::on_arrow_down))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .bg(theme.secondary)
                    .child(
                        v_flex()
                            .gap_0p5()
                            .min_w_0()
                            .flex_1()
                            .child(div().text_sm().font_semibold().child("Obligations"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .overflow_hidden()
                                    .child(self.title.clone()),
                            ),
                    )
                    .child(chrome_control_with_shortcut(
                        Button::new("obligations-close")
                            .label("Close")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close(window, cx);
                            })),
                        window,
                        &ObligationsClose,
                        OBLIGATIONS_CONTEXT,
                        cx,
                    )),
            )
            .child({
                let row_count = self.delegate.rows().len();
                let mut rows = Vec::with_capacity(row_count);
                for ix in 0..row_count {
                    if let Some(row) = self.delegate.render_row(ix, window, cx) {
                        rows.push(row);
                    }
                }
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("obligations-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                            .children(rows),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .left_0()
                            .child(Scrollbar::vertical(&self.scroll_handle)),
                    )
            })
            .child(
                div()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child("↑/↓ navigate · Enter edits · N adds · Ctrl+↑/↓ reorders · ←/→ collapse/expand · Esc closes"),
            )
            .into_any_element()
    }
}
