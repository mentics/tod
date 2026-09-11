//! Obligations panel — edit direct requirements/constraints for a Spec node.

mod delegate;

use crate::ui::actionable::{
    chrome_control_with_shortcut, chrome_control_with_shortcut_in_context,
};
use crate::ui::agent_chat::OpenAgentChat;
use crate::ui::key_context;
use crate::ui::list::{
    ListArrowDown, ListArrowUp, ListEnd, ListHome, ListPageDown, ListPageUp, viewport_row_count,
};
use crate::ui::pane_nav::{PaneFocusLeft, bind_modified_pane_nav};
use delegate::{
    NO_SECTION, ObligationListDelegate, ObligationRow, RowAction, SECTION_EDIT_TAG,
    new_section_row_key, obligation_section, section_row_key,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Corner, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, ScrollHandle,
    StatefulInteractiveElement, Styled, Subscription, Timer, Window, actions, anchored, deferred,
    div, px,
};
use gpui_component::IconName;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use tod_store::fleet::FleetStore;
use tod_store::outline::types::Capability;
use tod_store::outline::{
    KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation, OutlineMutation, ReorderDirection,
};
use uuid::Uuid;

const OBLIGATIONS_CONTEXT: &str = "Obligations";
const INLINE_EDIT_ROWS: usize = 2;

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
        ObligationsCommitEdit,
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
        KeyBinding::new("secondary-up", ObligationsMoveUp, context),
        KeyBinding::new("secondary-down", ObligationsMoveDown, context),
        KeyBinding::new("backspace", ObligationsDelete, context),
        KeyBinding::new("delete", ObligationsDelete, context),
        // Inline edit is a multi-line text area: arrows move the cursor as usual,
        // Escape abandons the edit, and Ctrl+Enter commits it.
        KeyBinding::new(
            "ctrl-enter",
            ObligationsCommitEdit,
            Some(key_context::including_input(OBLIGATIONS_CONTEXT)),
        ),
        // The section-name field is single-line, so plain Enter commits it.
        KeyBinding::new(
            "enter",
            ObligationsCommitEdit,
            Some(key_context::including_tag(OBLIGATIONS_CONTEXT, SECTION_EDIT_TAG)),
        ),
    ]);
    // Left/Right collapse/expand rows here, so crossing back to the tree uses Ctrl+arrows.
    bind_modified_pane_nav(cx, OBLIGATIONS_CONTEXT);
    key_context::bind_panel_escape(cx, ObligationsClose, OBLIGATIONS_CONTEXT);
}

#[derive(Debug, Clone)]
pub enum ObligationsEvent {
    Close,
    /// Ctrl+Left — move keyboard focus back to the task tree, leaving the panel open.
    FocusTaskList,
    /// Delete key with no obligation item selected — delete the task in the tree.
    DeleteSelectedTask,
    /// Chat icon — open an agent conversation scoped to this panel. Carries the
    /// live selection so the shell can assemble the agent's first message.
    OpenAgentChat {
        node_id: Uuid,
        /// The specific obligation selected, when one is.
        obligation_id: Option<Uuid>,
        /// Action config to run. `None` means "resolve it" — no configs exist
        /// yet, so the shell creates a default from app settings.
        config_id: Option<String>,
    },
    /// Picker's "New action config…" entry.
    OpenAgentConfig {
        node_id: Uuid,
    },
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
    pending_abandon_edit: bool,
    /// Kind/original-name of the section currently being renamed.
    section_edit_target: Option<(&'static str, String)>,
    /// Set while a brand-new (not yet created) section's name is being typed.
    new_section_kind: Option<&'static str>,
    section_edit_input: Entity<InputState>,
    pending_abandon_section_edit: bool,
    pending_live_refresh: bool,
    selected_key: Option<String>,
    /// Whether the current node has the Agent capability. Cached because
    /// `render` consults it every frame and the lookup hits SQLite.
    node_has_agent: bool,
    /// Open action-config picker, shown when the node has more than one.
    agent_menu: Option<Entity<PopupMenu>>,
    _agent_menu_subscription: Option<Subscription>,
    _inline_edit_subscription: Subscription,
    _section_edit_subscription: Subscription,
}

impl ObligationsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, fleet: Arc<FleetStore>) -> Self {
        let action_sink = Rc::new(RefCell::new(Vec::new()));
        let inline_edit_input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(INLINE_EDIT_ROWS, INLINE_EDIT_ROWS)
                .placeholder("Obligation text… (Ctrl+Enter to save, Esc to cancel)")
        });
        let _inline_edit_subscription = cx.subscribe(&inline_edit_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur) {
                this.pending_abandon_edit = true;
                cx.notify();
            }
        });

        let section_edit_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Section name… (Enter to save, Esc to cancel)")
        });
        let _section_edit_subscription =
            cx.subscribe(&section_edit_input, |this, _, event, cx| {
                if matches!(event, InputEvent::Blur) {
                    this.pending_abandon_section_edit = true;
                    cx.notify();
                }
            });

        let delegate = ObligationListDelegate::new(Vec::new(), action_sink.clone(), cx.weak_entity());

        let poll_entity = cx.weak_entity();
        let fleet_for_poll = fleet.clone();
        cx.spawn(async move |_, cx| {
            let mut fleet_rx = fleet_for_poll.subscribe_changes();
            loop {
                Timer::after(std::time::Duration::from_millis(200)).await;
                let mut changed = false;
                while fleet_rx.try_recv().is_ok() {
                    changed = true;
                }
                if changed {
                    let Ok(()) = poll_entity.update(cx, |this, cx| {
                        this.pending_live_refresh = true;
                        cx.notify();
                    }) else {
                        break;
                    };
                }
            }
        })
        .detach();

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
            pending_abandon_edit: false,
            section_edit_target: None,
            new_section_kind: None,
            section_edit_input,
            pending_abandon_section_edit: false,
            pending_live_refresh: false,
            selected_key: None,
            node_has_agent: false,
            agent_menu: None,
            _agent_menu_subscription: None,
            _inline_edit_subscription,
            _section_edit_subscription,
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
        self.refresh_node_has_agent();
        self.title = title.to_string();
        self.req_collapsed = false;
        self.con_collapsed = false;
        self.section_collapsed.clear();
        self.clear_inline_edit_state(window, cx);
        self.reload(window, cx);
        self.focus_list(window, cx);
        cx.notify();
    }

    /// `focus` controls whether keyboard focus moves into the panel — true
    /// for an explicit "open obligations" action, false when the panel is
    /// merely following tree selection and focus should stay put.
    pub fn retarget(
        &mut self,
        node_id: Uuid,
        title: &str,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.node_id == Some(node_id) {
            self.title = title.to_string();
            self.reload(window, cx);
            return;
        }
        self.node_id = Some(node_id);
        self.refresh_node_has_agent();
        self.title = title.to_string();
        self.req_collapsed = false;
        self.con_collapsed = false;
        self.section_collapsed.clear();
        self.clear_inline_edit_state(window, cx);
        self.reload(window, cx);
        if focus {
            self.focus_list(window, cx);
        }
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.node_id.is_none() {
            return;
        }
        self.clear_inline_edit_state(window, cx);
        self.node_id = None;
        self.node_has_agent = false;
        self.close_agent_menu(cx);
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

    /// Refresh the cached Agent-capability flag, which gates the chat icon.
    fn refresh_node_has_agent(&mut self) {
        self.node_has_agent = self
            .node_id
            .and_then(|node_id| self.fleet.list_node_capabilities(node_id).ok())
            .is_some_and(|caps| caps.contains(&Capability::Agent));
    }

    /// Id of the selected obligation, when the selection is an item rather than
    /// a group or section header.
    fn selected_obligation_id(&self) -> Option<Uuid> {
        match self.delegate.selected_row()? {
            ObligationRow::Item { obligation } => Some(obligation.id),
            _ => None,
        }
    }

    fn open_agent_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_id else {
            return;
        };
        if self.agent_menu.is_some() {
            self.close_agent_menu(cx);
            return;
        }
        let configs = self
            .fleet
            .list_agent_configs_for_task(&node_id.to_string())
            .unwrap_or_default();
        match configs.len() {
            // No configs yet: the shell creates a default from app settings
            // rather than making the user fill in a form first.
            0 => self.emit_agent_chat(node_id, None, cx),
            1 => {
                let config_id = configs[0].id.clone();
                self.emit_agent_chat(node_id, Some(config_id), cx);
            }
            // Several: pick one, same as the node tree does.
            _ => self.open_agent_menu(node_id, configs, window, cx),
        }
    }

    fn emit_agent_chat(
        &mut self,
        node_id: Uuid,
        config_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(ObligationsEvent::OpenAgentChat {
            node_id,
            obligation_id: self.selected_obligation_id(),
            config_id,
        });
    }

    fn open_agent_menu(
        &mut self,
        node_id: Uuid,
        configs: Vec<tod_store::fleet::AgentConfigRow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.weak_entity();
        let focus = self.focus_handle.clone();
        let menu = PopupMenu::build(window, cx, move |mut menu, _window, _cx| {
            menu = menu.action_context(focus).min_w(px(180.));
            for config in &configs {
                let view = view.clone();
                let config_id = config.id.clone();
                let label = config.id.clone();
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    let config_id = config_id.clone();
                    let _ = view.update(cx, |this, cx| {
                        this.close_agent_menu(cx);
                        this.emit_agent_chat(node_id, Some(config_id), cx);
                    });
                }));
            }
            let view = view.clone();
            menu.item(
                PopupMenuItem::new("New action config…").on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.close_agent_menu(cx);
                        cx.emit(ObligationsEvent::OpenAgentConfig { node_id });
                    });
                }),
            )
        });
        self._agent_menu_subscription =
            Some(cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
                this.close_agent_menu(cx);
            }));
        self.agent_menu = Some(menu);
        cx.notify();
    }

    fn close_agent_menu(&mut self, cx: &mut Context<Self>) {
        if self.agent_menu.take().is_some() {
            self._agent_menu_subscription = None;
            cx.notify();
        }
    }

    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_id else {
            return;
        };
        // The capability can be toggled elsewhere while this panel is open.
        self.refresh_node_has_agent();
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
            self.new_section_kind == Some(KIND_REQUIREMENT),
        );
        Self::append_kind_group(
            &mut rows,
            KIND_CONSTRAINT,
            cons,
            self.con_collapsed,
            &self.section_collapsed,
            self.new_section_kind == Some(KIND_CONSTRAINT),
        );
        rows
    }

    fn append_kind_group(
        rows: &mut Vec<ObligationRow>,
        kind: &'static str,
        items: Vec<NodeObligation>,
        kind_collapsed: bool,
        section_collapsed: &HashSet<String>,
        show_new_section: bool,
    ) {
        rows.push(ObligationRow::Group {
            kind,
            collapsed: kind_collapsed,
            count: items.len(),
        });
        if kind_collapsed {
            return;
        }
        if show_new_section {
            rows.push(ObligationRow::Section {
                kind,
                section: String::new(),
                collapsed: false,
                count: 0,
                is_new: true,
            });
        }
        for (section, section_items) in Self::group_by_section(items) {
            let key = section_row_key(kind, &section);
            let collapsed = section_collapsed.contains(&key);
            rows.push(ObligationRow::Section {
                kind,
                section: section.clone(),
                collapsed,
                count: section_items.len(),
                is_new: false,
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

    fn rebuild_visible(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.flat_rows();
        let selected = self.selected_key.clone();
        let previous_index = self.selected_index;
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
            self.delegate_editing_key(),
            self.inline_edit_input.clone(),
            self.section_edit_input.clone(),
        );
        if let Some(ix) = selected_ix {
            if previous_index != selected_ix {
                self.scroll_handle.scroll_to_top_of_item(ix);
            }
        }
        cx.notify();
    }

    fn select_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let key = self.delegate.rows().get(row_ix).map(|r| r.key());
        if self.selected_index != Some(row_ix) {
            if self.editing_id.is_some() {
                self.pending_abandon_edit = true;
            }
            if self.is_editing_section() {
                self.pending_abandon_section_edit = true;
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
        self.new_section_kind = None;
        self.section_edit_target = None;
        self.section_edit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.sync_delegate_editing(cx);
    }

    fn sync_delegate_editing(&mut self, cx: &mut Context<Self>) {
        self.delegate.set_inline_edit(
            self.delegate_editing_key(),
            self.inline_edit_input.clone(),
            self.section_edit_input.clone(),
        );
        cx.notify();
    }

    /// The row key currently in edit mode, for either an obligation body or a
    /// section name (mutually exclusive).
    fn delegate_editing_key(&self) -> Option<String> {
        self.editing_id.map(|id| id.to_string()).or_else(|| {
            if let Some(kind) = self.new_section_kind {
                Some(new_section_row_key(kind))
            } else {
                self.section_edit_target
                    .as_ref()
                    .map(|(kind, section)| section_row_key(kind, section))
            }
        })
    }

    fn is_editing(&self) -> bool {
        self.editing_id.is_some()
    }

    fn is_editing_section(&self) -> bool {
        self.new_section_kind.is_some() || self.section_edit_target.is_some()
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

    fn start_section_edit(
        &mut self,
        kind: &'static str,
        section: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_editing() {
            let _ = self.commit_inline_edit(window, cx);
        }
        if self.new_section_kind.is_some() {
            self.cancel_new_section(window, cx);
        }
        let initial = if section == NO_SECTION {
            String::new()
        } else {
            section.to_string()
        };
        self.section_edit_target = Some((kind, section.to_string()));
        self.selected_key = Some(section_row_key(kind, section));
        self.section_edit_input.update(cx, |input, cx| {
            input.set_value(&initial, window, cx);
            input.focus(window, cx);
        });
        self.rebuild_visible(window, cx);
    }

    fn add_section(&mut self, kind: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            let _ = self.commit_inline_edit(window, cx);
        }
        if self.section_edit_target.is_some() {
            self.abandon_section_edit(window, cx);
        }
        if kind == KIND_REQUIREMENT {
            self.req_collapsed = false;
        } else {
            self.con_collapsed = false;
        }
        self.new_section_kind = Some(kind);
        self.selected_key = Some(new_section_row_key(kind));
        self.section_edit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.rebuild_visible(window, cx);
    }

    fn abandon_section_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_section_kind.is_some() {
            self.cancel_new_section(window, cx);
            return;
        }
        if self.section_edit_target.is_none() {
            return;
        }
        self.section_edit_target = None;
        self.section_edit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.rebuild_visible(window, cx);
        self.focus_list(window, cx);
    }

    fn cancel_new_section(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_section_kind = None;
        self.section_edit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.rebuild_visible(window, cx);
        self.focus_list(window, cx);
    }

    fn commit_section_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let new_name = self
            .section_edit_input
            .read(cx)
            .text()
            .to_string()
            .trim()
            .to_string();

        if let Some(kind) = self.new_section_kind {
            if new_name.is_empty() {
                self.cancel_new_section(window, cx);
                return true;
            }
            let Some(node_id) = self.node_id else {
                return false;
            };
            let obligation_id = Uuid::new_v4();
            if let Err(err) = self
                .fleet
                .enqueue_outline(OutlineMutation::CreateObligation {
                    obligation_id: Some(obligation_id),
                    node_id,
                    kind: kind.to_string(),
                    after_id: None,
                    before: false,
                    section: Some(new_name),
                    body: String::new(),
                })
            {
                crate::ui::toast::error_toast(window, cx, format!("Create failed: {err}"));
                return false;
            }
            if let Err(err) = self.fleet.writer().flush() {
                crate::ui::toast::error_toast(window, cx, format!("Create failed: {err}"));
                return false;
            }
            self.new_section_kind = None;
            self.section_edit_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
            self.draft_id = Some(obligation_id);
            self.reload(window, cx);
            self.start_inline_edit(obligation_id, window, cx);
            return true;
        }

        let Some((kind, old_section)) = self.section_edit_target.clone() else {
            return false;
        };
        if new_name.is_empty() {
            crate::ui::toast::error_toast(window, cx, "Section name cannot be empty");
            self.section_edit_input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
            return false;
        }
        if new_name == old_section {
            self.abandon_section_edit(window, cx);
            return true;
        }
        let Some(node_id) = self.node_id else {
            return false;
        };
        let old_section_opt = if old_section == NO_SECTION {
            None
        } else {
            Some(old_section.clone())
        };
        if let Err(err) = self
            .fleet
            .enqueue_outline(OutlineMutation::RenameObligationSection {
                node_id,
                kind: kind.to_string(),
                old_section: old_section_opt,
                new_section: new_name.clone(),
            })
        {
            crate::ui::toast::error_toast(window, cx, format!("Rename failed: {err}"));
            return false;
        }
        if let Err(err) = self.fleet.writer().flush() {
            crate::ui::toast::error_toast(window, cx, format!("Rename failed: {err}"));
            return false;
        }
        let old_key = section_row_key(kind, &old_section);
        if self.section_collapsed.remove(&old_key) {
            self.section_collapsed.insert(section_row_key(kind, &new_name));
        }
        self.section_edit_target = None;
        self.section_edit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.selected_key = Some(section_row_key(kind, &new_name));
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
                section: None,
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
                RowAction::StartSectionEdit { kind, section } => {
                    let kind = if kind == KIND_REQUIREMENT {
                        KIND_REQUIREMENT
                    } else {
                        KIND_CONSTRAINT
                    };
                    self.start_section_edit(kind, &section, window, cx);
                }
                RowAction::AddSection { kind } => {
                    let kind = if kind == KIND_REQUIREMENT {
                        KIND_REQUIREMENT
                    } else {
                        KIND_CONSTRAINT
                    };
                    self.add_section(kind, window, cx);
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
        if self.is_editing_section() {
            self.abandon_section_edit(window, cx);
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

    fn on_commit_edit(
        &mut self,
        _: &ObligationsCommitEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_editing_section() {
            let _ = self.commit_section_edit(window, cx);
            return;
        }
        if !self.is_editing() {
            return;
        }
        let _ = self.commit_inline_edit(window, cx);
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
        if matches!(self.selected_row(), Some(ObligationRow::Item { .. })) {
            self.delete_selected(window, cx);
        } else {
            cx.emit(ObligationsEvent::DeleteSelectedTask);
        }
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
        if self.pending_abandon_section_edit {
            self.pending_abandon_section_edit = false;
            self.abandon_section_edit(window, cx);
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
            .on_action(cx.listener(|this, _: &PaneFocusLeft, _, cx| {
                if this.editing_id.is_some() {
                    cx.propagate();
                    return;
                }
                cx.emit(ObligationsEvent::FocusTaskList);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &OpenAgentChat, window, cx| {
                if !this.node_has_agent {
                    cx.propagate();
                    return;
                }
                this.open_agent_chat(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_enter))
            .on_action(cx.listener(Self::on_create_below))
            .on_action(cx.listener(Self::on_create_above))
            .on_action(cx.listener(Self::on_create_child))
            .on_action(cx.listener(Self::on_move_up))
            .on_action(cx.listener(Self::on_move_down))
            .on_action(cx.listener(Self::on_edit))
            .on_action(cx.listener(Self::on_commit_edit))
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
                                div().text_xs().text_color(muted).overflow_hidden().child(
                                    crate::ui::selectable_text::selectable_text(
                                        "obligations-title",
                                        self.title.clone(),
                                        window,
                                        cx,
                                    )
                                    .text_color(muted),
                                ),
                            ),
                    )
                    .when(self.node_has_agent, |row| {
                        row.child(div().relative().when_some(
                            self.agent_menu.clone(),
                            |el, menu| {
                                el.child(
                                    deferred(
                                        anchored()
                                            .anchor(Corner::TopRight)
                                            .snap_to_window_with_margin(px(8.))
                                            .child(div().occlude().mt_1().child(menu)),
                                    )
                                    .with_priority(1),
                                )
                            },
                        ))
                        .child(chrome_control_with_shortcut_in_context(
                            Button::new("obligations-agent-chat")
                                .icon(IconName::Bot)
                                .label("Chat")
                                .outline()
                                .compact()
                                .tooltip("Chat with an agent about these obligations")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_agent_chat(window, cx);
                                })),
                            window,
                            &OpenAgentChat,
                            None,
                            cx,
                        ))
                    })
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
                        // Narrow right-edge strip, not the full row area: the
                        // Scrollbar element installs a click-to-jump handler
                        // across its entire bounds, which would otherwise
                        // swallow every mouse click meant for the rows below.
                        div()
                            .occlude()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.))
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
                    .child("↑/↓ navigate · Enter edits · N adds · Cmd/Ctrl+↑/↓ reorders · ←/→ collapse/expand · Ctrl+J chats · Esc closes"),
            )
            .into_any_element()
    }
}
