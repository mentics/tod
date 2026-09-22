//! Obligations panel — edit direct requirements/constraints for a Spec node.

mod rows;

use crate::ui::actionable::{chrome_control_with_shortcut, render_shortcut_pill};
use crate::ui::agent_chat::OpenAgentChat;
use crate::ui::item_list::keyboard::{
    ItemListActivate, ItemListAddGroup, ItemListCollapse, ItemListCommitEdit, ItemListCreateAbove,
    ItemListCreateBelow, ItemListCreateChild, ItemListDelete, ItemListDown, ItemListEdit,
    ItemListEnd, ItemListExpand, ItemListFocusSearch, ItemListHome, ItemListMoveDown,
    ItemListMoveUp, ItemListPageDown, ItemListPageUp, ItemListSearchSpace, ItemListToggleMark,
    ItemListUp,
};
use crate::ui::item_list::{
    CollapseStep, GroupSpec, ItemList, ItemListKeys, ItemListRow, bind_item_list_keys,
    bind_single_line_commit, search,
};
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, bind_modified_pane_nav};
use crate::ui::status_filter::{StatusFilter, render_status_filter, status_counts};
use crate::views::rows::{RowAction, RowHost};
use rows::{
    ListAction, NO_SECTION, ObGroup, ObRow, ObligationItem, SECTION_EDIT_TAG, group_row_key,
    kind_label, new_section_row_key, obligation_section, phase_label, phase_row_key,
    section_row_key, static_kind,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, actions, div,
    px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState, TextareaState};
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tod_store::conversation::{Focus, NetOp};
use tod_store::fleet::FleetStore;
use tod_store::interview::{OBLIGATION_PHASES, PHASE_REQUIREMENTS, PHASE_UNKNOWN};
use tod_store::outline::{
    KIND_CONSTRAINT, KIND_REQUIREMENT, NodeObligation, OutlineMutation, ReorderDirection,
};
use tod_store::outline::repos::PlanStepRepo;
use tod_store::verification::{LISTING_STATUSES, STANDING_NOT_PLANNED, VerdictRepo};
use uuid::Uuid;

const OBLIGATIONS_CONTEXT: &str = "Obligations";
const INLINE_EDIT_ROWS: usize = 2;

actions!(obligations, [ObligationsClose]);

pub fn register_obligations_keyboard_bindings(cx: &mut App) {
    // The panel is an item list: navigation, editing, creation, reordering,
    // marking, grouping and search all come from the one key set.
    bind_item_list_keys(cx, OBLIGATIONS_CONTEXT, ItemListKeys::all());
    // A section's name is a single-line field, so plain Enter commits it.
    bind_single_line_commit(cx, OBLIGATIONS_CONTEXT, SECTION_EDIT_TAG);
    // Left/Right collapse/expand groups here, so crossing back to the tree
    // uses Ctrl+arrows.
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
    /// Ctrl+J — open the conversation about the selected obligation, or about
    /// the node when no obligation is selected.
    OpenAgentChat {
        node_id: Uuid,
        /// The specific obligation selected, when one is.
        obligation_id: Option<Uuid>,
    },
    /// Clicked the "Design"/"+ Design" affordance on a design-phase
    /// obligation row — create or open its associated visual-design mockup.
    OpenVisualDesign {
        node_id: Uuid,
        obligation_id: Uuid,
    },
}

/// What the cursor is on, cloned out of the list so the panel can act on it.
#[derive(Debug, Clone)]
enum CursorRow {
    Group(ObGroup),
    Item(ObligationItem),
}

pub struct ObligationsView {
    fleet: Arc<FleetStore>,
    node_id: Option<Uuid>,
    title: String,
    items: Vec<NodeObligation>,
    search_query: String,
    search_input: Entity<InputState>,
    /// The interview's current phase, when this panel is scoped to one — used
    /// only to seed which phase starts expanded and as the default phase for
    /// obligations created with no clearer phase context. `None` outside an
    /// interview (the standalone Obligations panel), where every phase starts
    /// expanded as before.
    active_phase: Option<String>,
    focus_handle: FocusHandle,
    /// The rows, the cursor, the selection, the collapsed groups and the
    /// scrolling: everything every list in the app shares.
    list: ItemList<ObligationItem, ObGroup>,
    host: RowHost<ListAction>,
    /// Hosted inside another view (the conversation view's context panel):
    /// no Close or chat button, and Escape / Ctrl+Left go to the host.
    embedded: bool,
    /// Items no longer on the node that the host still wants shown, struck
    /// through (the conversation's deleted items). Merged into `items` on
    /// every reload and never editable.
    removed: Vec<NodeObligation>,
    /// Which of `items` are those removed ones.
    struck: HashSet<Uuid>,
    /// Change-set operations by obligation id, shown as a leading op icon.
    change_markers: HashMap<Uuid, NetOp>,
    /// Each obligation's standing — its verdict, else whether a plan step
    /// satisfies it — which the status filter goes by.
    standing: HashMap<Uuid, String>,
    /// The standings the list shows; empty shows every obligation.
    filter: StatusFilter,
    editing_id: Option<Uuid>,
    draft_id: Option<Uuid>,
    edit_original_body: Option<String>,
    inline_edit_input: Entity<TextareaState>,
    pending_abandon_edit: bool,
    /// Phase/kind/original-name of the section currently being renamed.
    section_edit_target: Option<(String, &'static str, String)>,
    /// Set while a brand-new (not yet created) section's name is being typed.
    /// `(phase, kind)`.
    new_section_kind: Option<(String, &'static str)>,
    section_edit_input: Entity<InputState>,
    pending_abandon_section_edit: bool,
    pending_live_refresh: bool,
    _inline_edit_subscription: Subscription,
    _section_edit_subscription: Subscription,
}

impl ObligationsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, fleet: Arc<FleetStore>) -> Self {
        let host = RowHost::for_entity(cx.weak_entity());
        let inline_edit_input = cx.new(|cx| {
            TextareaState::new(window, cx)
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
        let _section_edit_subscription = cx.subscribe(&section_edit_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Blur) {
                this.pending_abandon_section_edit = true;
                cx.notify();
            }
        });

        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search obligations…"));

        let poll_entity = cx.weak_entity();
        let fleet_for_poll = fleet.clone();
        cx.spawn(async move |_, cx| {
            let mut fleet_rx = fleet_for_poll.subscribe_changes();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
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
            search_query: String::new(),
            search_input,
            active_phase: None,
            focus_handle: cx.focus_handle(),
            list: ItemList::new()
                .with_group_editor(section_edit_input.clone(), SECTION_EDIT_TAG)
                .with_marking()
                .with_drag(rows::draggable),
            host,
            embedded: false,
            removed: Vec::new(),
            struck: HashSet::new(),
            change_markers: HashMap::new(),
            standing: HashMap::new(),
            filter: StatusFilter::default(),
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
            _inline_edit_subscription,
            _section_edit_subscription,
        }
    }

    pub fn is_open(&self) -> bool {
        self.node_id.is_some()
    }

    /// Host this view inside another: hides Close and the chat button, and
    /// hands Escape and Ctrl+Left (`PaneFocusLeft`) to the host instead of
    /// closing or emitting `FocusTaskList`.
    pub fn set_embedded(&mut self, embedded: bool, cx: &mut Context<Self>) {
        self.embedded = embedded;
        cx.notify();
    }

    /// Scroll to obligation `id` and highlight it, expanding its phase, kind,
    /// and section, and clearing a search that hides it. Does nothing if the
    /// obligation is not on this node.
    pub fn highlight_item(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items.iter().find(|o| o.id == id) else {
            return;
        };
        let (phase, kind, section) = (
            item.phase.clone(),
            item.kind.clone(),
            obligation_section(item).to_string(),
        );
        self.list.set_collapsed(phase_row_key(&phase), false);
        self.list.set_collapsed(group_row_key(&phase, &kind), false);
        self.list
            .set_collapsed(section_row_key(&phase, &kind, &section), false);
        let key = id.to_string();
        if !self.search_matches().iter().any(|o| o.id == id) {
            self.filter.clear();
            self.search_query.clear();
            self.search_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
        }
        self.list.set_cursor_key(Some(key));
        self.rebuild_visible(window, cx);
        self.list.scroll_to_cursor();
    }

    /// Show a leading op icon on each obligation in `markers`.
    pub fn set_change_markers(&mut self, markers: HashMap<Uuid, NetOp>, cx: &mut Context<Self>) {
        self.change_markers = markers;
        self.list.set_rows(self.flat_rows());
        cx.notify();
    }

    /// Whether `id` is shown as a removed (struck-through) row.
    pub(crate) fn is_struck(&self, id: Uuid) -> bool {
        self.struck.contains(&id)
    }

    /// Also show `items`, which no longer exist, struck through at their old
    /// place. Ones that exist again (a reversed deletion) show as normal.
    /// Whether `id` is shown as a removed (struck-through) row.
    pub fn set_removed_items(
        &mut self,
        items: Vec<NodeObligation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.removed == items {
            return;
        }
        self.removed = items;
        self.reload(window, cx);
    }

    /// The conversation Ctrl+J opens here: the selected obligation, or the
    /// node when none (or a removed one) is selected.
    pub fn conversation_focus(&self) -> Option<Focus> {
        let node = self.node_id?;
        Some(
            match self
                .selected_obligation_id()
                .filter(|id| !self.is_struck(*id))
            {
                Some(id) => Focus::Obligation { node, id },
                None => Focus::Node(node),
            },
        )
    }

    pub fn open(
        &mut self,
        node_id: Uuid,
        title: &str,
        active_phase: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.node_id = Some(node_id);
        self.title = title.to_string();
        self.active_phase = active_phase.map(str::to_string);
        self.reset_phase_collapse();
        self.clear_inline_edit_state(window, cx);
        self.reload(window, cx);
        self.focus_list(window, cx);
        cx.notify();
    }

    /// `focus` controls whether keyboard focus moves into the panel — true
    /// for an explicit "open obligations" action, false when the panel is
    /// merely following tree selection and focus should stay put.
    ///
    /// `active_phase` is the interview's current phase, `None` outside an
    /// interview — only every phase starting expanded on open is affected.
    pub fn retarget(
        &mut self,
        node_id: Uuid,
        title: &str,
        active_phase: Option<&str>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.node_id == Some(node_id) {
            self.title = title.to_string();
            self.active_phase = active_phase.map(str::to_string);
            self.reload(window, cx);
            return;
        }
        self.node_id = Some(node_id);
        self.title = title.to_string();
        self.active_phase = active_phase.map(str::to_string);
        self.reset_phase_collapse();
        self.clear_inline_edit_state(window, cx);
        self.reload(window, cx);
        if focus {
            self.focus_list(window, cx);
        }
        cx.notify();
    }

    /// Collapse every phase except `active_phase`; expand everything when
    /// there is no active phase (outside an interview).
    fn reset_phase_collapse(&mut self) {
        self.list.expand_all();
        if self.active_phase.is_some() {
            for phase in Self::phase_order() {
                if Some(phase) != self.active_phase.as_deref() {
                    self.list.set_collapsed(phase_row_key(phase), true);
                }
            }
        }
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.node_id.is_none() {
            return;
        }
        self.clear_inline_edit_state(window, cx);
        self.node_id = None;
        self.title.clear();
        self.items.clear();
        self.list.set_cursor_key(None);
        self.list.clear_marks();
        self.search_query.clear();
        self.search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        cx.emit(ObligationsEvent::Close);
        cx.notify();
    }

    fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Id of the selected obligation, when the selection is an item rather than
    /// a group or section header.
    fn selected_obligation_id(&self) -> Option<Uuid> {
        Some(self.list.cursor_item()?.obligation.id)
    }

    fn open_agent_chat(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(node_id) = self.node_id else {
            return;
        };
        cx.emit(ObligationsEvent::OpenAgentChat {
            node_id,
            obligation_id: self.selected_obligation_id(),
        });
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
        self.standing = self
            .fleet
            .read(|conn| {
                let steps = PlanStepRepo::new(conn);
                let mut standing = HashMap::new();
                for s in VerdictRepo::new(conn).standings(node_id)? {
                    let planned = !steps.list_steps_for_obligation(s.obligation.id)?.is_empty();
                    standing.insert(s.obligation.id, s.listing_status(planned).to_string());
                }
                Ok(standing)
            })
            .unwrap_or_default();
        let mut struck = HashSet::new();
        for ghost in &self.removed {
            if ghost.node_id != node_id || self.items.iter().any(|o| o.id == ghost.id) {
                continue;
            }
            struck.insert(ghost.id);
            let at = self
                .items
                .iter()
                .position(|o| o.kind == ghost.kind && o.ordinal > ghost.ordinal)
                .unwrap_or(self.items.len());
            self.items.insert(at, ghost.clone());
        }
        self.struck = struck;
        self.rebuild_visible(window, cx);
    }

    /// Phases in display order: the two obligation-eligible phases, then
    /// `unknown` last (legacy/unclassified obligations trail behind real
    /// ones). `planning` is not a valid obligation phase — planning work is
    /// tracked as plan steps instead — so it never appears here.
    fn phase_order() -> [&'static str; OBLIGATION_PHASES.len()] {
        [
            PHASE_REQUIREMENTS,
            tod_store::interview::PHASE_DESIGN,
            PHASE_UNKNOWN,
        ]
    }

    /// An obligation's standing, as the status filter counts it.
    fn standing_of(&self, id: Uuid) -> &str {
        self.standing
            .get(&id)
            .map_or(STANDING_NOT_PLANNED, String::as_str)
    }

    /// Toggle `status` in the filter, or clear it (`None`, "All").
    fn set_filter(&mut self, status: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        match status {
            Some(status) => self.filter.toggle(status),
            None => {
                self.filter.clear();
            }
        }
        self.rebuild_visible(window, cx);
    }

    /// Items the status filter lets through that match the current search query (body or section text),
    /// case-insensitive fuzzy match. Space-separated terms are ANDed together:
    /// each term must fuzzily match some word in the item's text (typo-tolerant),
    /// but every term must match for the item to be included. Empty query
    /// matches everything.
    fn search_matches(&self) -> Vec<&NodeObligation> {
        self.items
            .iter()
            .filter(|o| self.filter.admits(self.standing_of(o.id)))
            .filter(|o| {
                search::matches_query(
                    &self.search_query,
                    &[&o.body, o.section.as_deref().unwrap_or("")],
                )
            })
            .collect()
    }

    /// The rows the list shows: phase, then kind, then section, then the
    /// obligations in that section.
    fn flat_rows(&self) -> Vec<ObRow> {
        let mut rows = Vec::new();
        let matching = self.search_matches();
        for phase in Self::phase_order() {
            let phase_items: Vec<_> = matching
                .iter()
                .filter(|o| o.phase == phase)
                .copied()
                .collect();
            if phase_items.is_empty() {
                continue;
            }
            let key = phase_row_key(phase);
            let collapsed = self.list.is_collapsed(&key);
            rows.push(ItemListRow::group(
                GroupSpec::new(key, 0, phase_label(phase))
                    .count(phase_items.len())
                    .collapsed(collapsed),
                ObGroup::Phase {
                    phase: phase.to_string(),
                },
            ));
            if collapsed {
                continue;
            }
            for kind in [KIND_REQUIREMENT, KIND_CONSTRAINT] {
                let items: Vec<NodeObligation> = phase_items
                    .iter()
                    .filter(|o| o.kind == kind)
                    .map(|o| (*o).clone())
                    .collect();
                self.append_kind_group(&mut rows, phase, kind, items);
            }
        }
        rows
    }

    fn append_kind_group(
        &self,
        rows: &mut Vec<ObRow>,
        phase: &str,
        kind: &'static str,
        items: Vec<NodeObligation>,
    ) {
        let kind_key = group_row_key(phase, kind);
        let collapsed = self.list.is_collapsed(&kind_key);
        let add_host = self.host.clone();
        let add_phase = phase.to_string();
        rows.push(ItemListRow::group(
            GroupSpec::new(kind_key, 1, kind_label(kind))
                .count(items.len())
                .collapsed(collapsed)
                .action(RowAction::new("add-section", "+ Section", move |_, cx| {
                    add_host.push(
                        ListAction::AddSection {
                            phase: add_phase.clone(),
                            kind,
                        },
                        cx,
                    );
                })),
            ObGroup::Kind {
                phase: phase.to_string(),
                kind,
            },
        ));
        if collapsed {
            return;
        }
        if self.new_section_kind.as_ref() == Some(&(phase.to_string(), kind)) {
            rows.push(ItemListRow::group(
                GroupSpec::new(new_section_row_key(phase, kind), 2, "")
                    .editing(true)
                    .chevron(false),
                ObGroup::Section {
                    phase: phase.to_string(),
                    kind,
                    section: String::new(),
                    is_new: true,
                },
            ));
        }
        for (section, section_items) in Self::group_by_section(items) {
            let key = section_row_key(phase, kind, &section);
            let collapsed = self.list.is_collapsed(&key);
            let editing = self.list.editing_key() == Some(key.as_str());
            let rename_host = self.host.clone();
            let (rename_phase, rename_section) = (phase.to_string(), section.clone());
            rows.push(ItemListRow::group(
                GroupSpec::new(key, 2, section.clone())
                    .count(section_items.len())
                    .collapsed(collapsed)
                    .editing(editing)
                    .on_rename(move |_, cx| {
                        rename_host.push(
                            ListAction::StartSectionEdit {
                                phase: rename_phase.clone(),
                                kind,
                                section: rename_section.clone(),
                            },
                            cx,
                        );
                    }),
                ObGroup::Section {
                    phase: phase.to_string(),
                    kind,
                    section: section.clone(),
                    is_new: false,
                },
            ));
            if collapsed {
                continue;
            }
            for obligation in section_items {
                let id = obligation.id;
                rows.push(ItemListRow::item(
                    id.to_string(),
                    ObligationItem {
                        struck: self.struck.contains(&id),
                        marker: self.change_markers.get(&id).copied(),
                        obligation,
                    },
                ));
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

    /// Rebuild the rows from the store data, keeping the cursor on whatever it
    /// was on.
    fn rebuild_visible(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.list.set_editing_key(self.editing_key());
        self.list.set_rows(self.flat_rows());
        cx.notify();
    }

    fn select_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        if self.list.cursor() == Some(row_ix) {
            return;
        }
        if self.editing_id.is_some() {
            self.pending_abandon_edit = true;
        }
        if self.is_editing_section() {
            self.pending_abandon_section_edit = true;
        }
        self.list.set_cursor(row_ix);
        cx.notify();
    }

    fn first_item_in_scope(&self, phase: &str, kind: &str, section: Option<&str>) -> Option<Uuid> {
        self.items
            .iter()
            .filter(|o| {
                o.phase == phase
                    && o.kind == kind
                    && section.map_or(true, |s| obligation_section(o) == s)
            })
            .min_by_key(|o| o.ordinal)
            .map(|o| o.id)
    }

    fn last_item_in_scope(&self, phase: &str, kind: &str, section: Option<&str>) -> Option<Uuid> {
        self.items
            .iter()
            .filter(|o| {
                o.phase == phase
                    && o.kind == kind
                    && section.map_or(true, |s| obligation_section(o) == s)
            })
            .max_by_key(|o| o.ordinal)
            .map(|o| o.id)
    }

    /// What the cursor is on, cloned out of the list so the view can act on it.
    fn cursor_row(&self) -> Option<CursorRow> {
        match self.list.cursor_row()? {
            ItemListRow::Group { group, .. } => Some(CursorRow::Group(group.clone())),
            ItemListRow::Item { item, .. } => Some(CursorRow::Item(item.clone())),
        }
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
        self.list.set_editing_key(self.editing_key());
        cx.notify();
    }

    /// The row key currently in edit mode, for either an obligation body or a
    /// section name (mutually exclusive).
    fn editing_key(&self) -> Option<String> {
        self.editing_id.map(|id| id.to_string()).or_else(|| {
            if let Some((phase, kind)) = &self.new_section_kind {
                Some(new_section_row_key(phase, kind))
            } else {
                self.section_edit_target
                    .as_ref()
                    .map(|(phase, kind, section)| section_row_key(phase, kind, section))
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
        if self.is_struck(id) {
            return;
        }
        let body = self
            .items
            .iter()
            .find(|o| o.id == id)
            .map(|o| o.body.clone())
            .unwrap_or_default();
        self.editing_id = Some(id);
        self.edit_original_body = Some(body.clone());
        self.list.set_cursor_key(Some(id.to_string()));
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
        self.list.set_cursor_key(Some(editing_id.to_string()));
        self.reload(window, cx);
        self.focus_list(window, cx);
        true
    }

    fn start_section_edit(
        &mut self,
        phase: &str,
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
        self.section_edit_target = Some((phase.to_string(), kind, section.to_string()));
        self.list.set_cursor_key(Some(section_row_key(phase, kind, section)));
        self.section_edit_input.update(cx, |input, cx| {
            input.set_value(&initial, window, cx);
            input.focus(window, cx);
        });
        self.rebuild_visible(window, cx);
    }

    fn add_section(
        &mut self,
        phase: &str,
        kind: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_editing() {
            let _ = self.commit_inline_edit(window, cx);
        }
        if self.section_edit_target.is_some() {
            self.abandon_section_edit(window, cx);
        }
        self.list
            .set_collapsed(group_row_key(phase, kind), false);
        self.new_section_kind = Some((phase.to_string(), kind));
        self.list.set_cursor_key(Some(new_section_row_key(phase, kind)));
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

        if let Some((phase, kind)) = self.new_section_kind.clone() {
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
                    phase,
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

        let Some((phase, kind, old_section)) = self.section_edit_target.clone() else {
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
        // Renaming a section spans every obligation in `node_id`/`kind` with
        // that name, regardless of phase — sections are user-defined labels,
        // not phase-scoped, so this intentionally isn't filtered by phase.
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
        self.list.rekey_collapsed(
            &section_row_key(&phase, kind, &old_section),
            section_row_key(&phase, kind, &new_name),
        );
        self.section_edit_target = None;
        self.section_edit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.list.set_cursor_key(Some(section_row_key(&phase, kind, &new_name)));
        self.reload(window, cx);
        self.focus_list(window, cx);
        true
    }

    /// Phase to create a new obligation in when there's no clearer context
    /// (an empty panel, or `create_relative` with nothing selected): the
    /// interview's active phase, or `requirements` outside an interview.
    fn default_creation_phase(&self) -> String {
        self.active_phase
            .clone()
            .unwrap_or_else(|| PHASE_REQUIREMENTS.to_string())
    }

    fn create_in_kind(
        &mut self,
        phase: &str,
        kind: &str,
        after_id: Option<Uuid>,
        before: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(node_id) = self.node_id else {
            return;
        };
        self.list
            .set_collapsed(group_row_key(phase, kind), false);
        self.list
            .set_collapsed(section_row_key(phase, kind, NO_SECTION), false);
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
                phase: phase.to_string(),
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
        match self.cursor_row() {
            Some(CursorRow::Group(ObGroup::Kind { phase, kind })) => {
                if before {
                    self.create_in_kind(&phase, kind, None, true, window, cx);
                } else {
                    match self.first_item_in_scope(&phase, kind, None) {
                        Some(id) => self.create_in_kind(&phase, kind, Some(id), true, window, cx),
                        None => self.create_in_kind(&phase, kind, None, false, window, cx),
                    }
                }
            }
            Some(CursorRow::Group(ObGroup::Section {
                phase,
                kind,
                section,
                ..
            })) => {
                self.ensure_section_expanded(&phase, kind, &section, window, cx);
                if before {
                    match self.first_item_in_scope(&phase, kind, Some(&section)) {
                        Some(id) => self.create_in_kind(&phase, kind, Some(id), true, window, cx),
                        None => self.create_in_kind(&phase, kind, None, false, window, cx),
                    }
                } else {
                    match self.last_item_in_scope(&phase, kind, Some(&section)) {
                        Some(id) => self.create_in_kind(&phase, kind, Some(id), false, window, cx),
                        None => self.create_in_kind(&phase, kind, None, false, window, cx),
                    }
                }
            }
            Some(CursorRow::Item(item)) => {
                let phase = item.obligation.phase.clone();
                // A removed item is no anchor: add at the end of its kind.
                let anchor = Some(item.obligation.id).filter(|_| !item.struck);
                self.create_in_kind(&phase, &item.obligation.kind, anchor, before, window, cx);
            }
            Some(CursorRow::Group(ObGroup::Phase { phase })) => {
                self.create_in_kind(&phase, KIND_REQUIREMENT, None, false, window, cx);
            }
            None => {
                let phase = self.default_creation_phase();
                self.create_in_kind(&phase, KIND_REQUIREMENT, None, false, window, cx);
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
                self.list.set_cursor_key(Some(id.to_string()));
                self.create_relative(false, window, cx);
            }
            return;
        }
        match self.cursor_row() {
            Some(CursorRow::Item(item)) => {
                self.start_inline_edit(item.obligation.id, window, cx);
            }
            Some(CursorRow::Group(_)) | None => {
                self.create_relative(false, window, cx);
            }
        }
    }

    /// Collapse or expand the group with `key`, then rebuild the rows.
    fn set_group_collapsed(
        &mut self,
        key: String,
        collapsed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.list.set_collapsed(key, collapsed);
        self.rebuild_visible(window, cx);
    }

    /// Expand a section and everything above it, so a row created there shows.
    fn ensure_section_expanded(
        &mut self,
        phase: &str,
        kind: &str,
        section: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.list.set_collapsed(phase_row_key(phase), false);
        self.list.set_collapsed(group_row_key(phase, kind), false);
        self.list
            .set_collapsed(section_row_key(phase, kind, section), false);
        self.rebuild_visible(window, cx);
    }

    /// The obligations an action works on: the marked ones, or the one under
    /// the cursor when none is marked. Group headings are not obligations and
    /// drop out here, as do removed rows.
    fn selected_obligations(&self) -> Vec<Uuid> {
        self.list
            .selection()
            .iter()
            .filter_map(|key| Uuid::parse_str(key).ok())
            .filter(|id| !self.is_struck(*id))
            .collect()
    }

    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.selected_obligations();
        if ids.is_empty() {
            return;
        }
        let next_key = self.key_after_deleting(&ids);
        for id in &ids {
            if let Err(err) = self
                .fleet
                .enqueue_outline(OutlineMutation::DeleteObligation {
                    obligation_id: *id,
                })
            {
                crate::ui::toast::error_toast(window, cx, format!("Delete failed: {err}"));
                return;
            }
        }
        let _ = self.fleet.writer().flush();
        self.list.clear_marks();
        self.list.set_cursor_key(next_key);
        self.reload(window, cx);
        self.focus_list(window, cx);
    }

    /// Where the cursor lands once `ids` are gone: the next obligation in the
    /// last one's section, else the last one left there, else the section
    /// heading itself.
    fn key_after_deleting(&self, ids: &[Uuid]) -> Option<String> {
        let last = ids.last()?;
        let gone = self.items.iter().find(|o| o.id == *last)?;
        let (phase, kind, section) = (
            gone.phase.clone(),
            gone.kind.clone(),
            obligation_section(gone).to_string(),
        );
        let siblings: Vec<&NodeObligation> = self
            .items
            .iter()
            .filter(|o| {
                o.phase == phase
                    && o.kind == kind
                    && obligation_section(o) == section
                    && !ids.contains(&o.id)
            })
            .collect();
        siblings
            .iter()
            .find(|o| o.ordinal > gone.ordinal)
            .or(siblings.last())
            .map(|o| o.id.to_string())
            .or_else(|| Some(section_row_key(&phase, &kind, &section)))
    }

    fn move_selected(
        &mut self,
        direction: ReorderDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(CursorRow::Item(item)) = self.cursor_row() else {
            return;
        };
        if item.struck {
            return;
        }
        let id = item.obligation.id;
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
        self.list.set_cursor_key(Some(id.to_string()));
        self.reload(window, cx);
        self.focus_list(window, cx);
    }

    fn move_selection(&mut self, delta: i32, _window: &mut Window, cx: &mut Context<Self>) {
        if self.list.is_empty() {
            return;
        }
        let current = self.list.cursor().unwrap_or(0);
        let next = if delta < 0 {
            current.saturating_sub((-delta) as usize)
        } else {
            (current + delta as usize).min(self.list.len() - 1)
        };
        self.select_row(next, cx);
    }

    /// Apply what the rows and the list reported, in order.
    fn drain_row_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for action in self.host.drain() {
            match action {
                ListAction::Select { row_ix } => {
                    self.select_row(row_ix, cx);
                }
                ListAction::ToggleGroup { key } => {
                    self.list.toggle_collapsed(&key);
                    self.rebuild_visible(window, cx);
                }
                ListAction::ToggleMark { row_ix } => {
                    self.list.toggle_mark_at(row_ix);
                    cx.notify();
                }
                ListAction::StartEdit { obligation_id } => {
                    self.start_inline_edit(obligation_id, window, cx);
                }
                ListAction::StartSectionEdit {
                    phase,
                    kind,
                    section,
                } => {
                    self.start_section_edit(&phase, kind, &section, window, cx);
                }
                ListAction::AddSection { phase, kind } => {
                    self.add_section(&phase, kind, window, cx);
                }
                ListAction::OpenVisualDesign { obligation_id } => {
                    if let Some(node_id) = self.node_id {
                        cx.emit(ObligationsEvent::OpenVisualDesign {
                            node_id,
                            obligation_id,
                        });
                    }
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
        if self.embedded {
            cx.propagate();
            return;
        }
        self.close(window, cx);
    }

    fn on_enter(&mut self, _: &ItemListActivate, window: &mut Window, cx: &mut Context<Self>) {
        self.on_smart_enter(window, cx);
    }

    fn on_create_below(
        &mut self,
        _: &ItemListCreateBelow,
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
        _: &ItemListCreateAbove,
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
        _: &ItemListCreateChild,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_editing() {
            let _ = self.commit_inline_edit(window, cx);
        }
        match self.cursor_row() {
            Some(CursorRow::Group(ObGroup::Kind { phase, kind })) => {
                self.set_group_collapsed(group_row_key(&phase, kind), false, window, cx);
                self.create_in_kind(&phase, kind, None, false, window, cx);
            }
            Some(CursorRow::Group(ObGroup::Section {
                phase,
                kind,
                section,
                ..
            })) => {
                self.ensure_section_expanded(&phase, kind, &section, window, cx);
                match self.last_item_in_scope(&phase, kind, Some(&section)) {
                    Some(id) => self.create_in_kind(&phase, kind, Some(id), false, window, cx),
                    None => self.create_in_kind(&phase, kind, None, false, window, cx),
                }
            }
            _ => self.create_relative(false, window, cx),
        }
    }

    fn on_move_up(&mut self, _: &ItemListMoveUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selected(ReorderDirection::Up, window, cx);
    }

    fn on_move_down(&mut self, _: &ItemListMoveDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selected(ReorderDirection::Down, window, cx);
    }

    fn on_edit(&mut self, _: &ItemListEdit, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(CursorRow::Item(item)) = self.cursor_row() {
            self.start_inline_edit(item.obligation.id, window, cx);
        }
    }

    fn on_commit_edit(
        &mut self,
        _: &ItemListCommitEdit,
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

    /// Left: collapse the group under the cursor, else step out to the group
    /// that encloses it. The list decides; the panel only rebuilds its rows.
    fn on_collapse(&mut self, _: &ItemListCollapse, window: &mut Window, cx: &mut Context<Self>) {
        match self.list.collapse_step() {
            CollapseStep::Collapsed => self.rebuild_visible(window, cx),
            CollapseStep::MovedToParent => cx.notify(),
            CollapseStep::Nothing => {}
        }
    }

    fn on_expand(&mut self, _: &ItemListExpand, window: &mut Window, cx: &mut Context<Self>) {
        if self.list.expand_step() {
            self.rebuild_visible(window, cx);
        }
    }

    fn on_toggle_mark(
        &mut self,
        _: &ItemListToggleMark,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_editing() || self.is_editing_section() {
            return;
        }
        self.list.toggle_mark();
        cx.notify();
    }

    fn on_delete(&mut self, _: &ItemListDelete, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            return;
        }
        if self.selected_obligations().is_empty() {
            cx.emit(ObligationsEvent::DeleteSelectedTask);
            return;
        }
        self.delete_selected(window, cx);
    }

    fn on_add_section(&mut self, _: &ItemListAddGroup, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            return;
        }
        let (phase, kind) = match self.cursor_row() {
            Some(CursorRow::Group(ObGroup::Kind { phase, kind })) => (phase, kind),
            Some(CursorRow::Group(ObGroup::Section { phase, kind, .. })) => (phase, kind),
            Some(CursorRow::Item(item)) => (
                item.obligation.phase.clone(),
                static_kind(&item.obligation.kind),
            ),
            Some(CursorRow::Group(ObGroup::Phase { phase })) => (phase, KIND_REQUIREMENT),
            None => (self.default_creation_phase(), KIND_REQUIREMENT),
        };
        self.add_section(&phase, kind, window, cx);
    }

    fn sync_search_from_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).text().to_string();
        if query != self.search_query {
            self.search_query = query;
            self.rebuild_visible(window, cx);
        }
    }

    fn on_focus_search(
        &mut self,
        _: &ItemListFocusSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
    }

    fn on_search_space(
        &mut self,
        _: &ItemListSearchSpace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_input.update(cx, |input, cx| {
            input.insert(" ", window, cx);
        });
    }

    fn on_arrow_up(&mut self, _: &ItemListUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, window, cx);
    }

    fn on_arrow_down(&mut self, _: &ItemListDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, window, cx);
    }

    fn on_page_up(&mut self, _: &ItemListPageUp, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<ObligationItem, ObGroup>::page_rows(window.viewport_size().height);
        self.move_selection(-(page as i32), window, cx);
    }

    fn on_page_down(&mut self, _: &ItemListPageDown, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<ObligationItem, ObGroup>::page_rows(window.viewport_size().height);
        self.move_selection(page as i32, window, cx);
    }

    fn on_home(&mut self, _: &ItemListHome, _window: &mut Window, cx: &mut Context<Self>) {
        if self.list.cursor_home() {
            cx.notify();
        }
    }

    fn on_end(&mut self, _: &ItemListEnd, _window: &mut Window, cx: &mut Context<Self>) {
        if self.list.cursor_end() {
            cx.notify();
        }
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
        self.sync_search_from_input(window, cx);
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
        let embedded = self.embedded;

        v_flex()
            .key_context(OBLIGATIONS_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .border_l_2()
            .border_color(accent)
            .on_action(cx.listener(|this, _: &PaneFocusLeft, _, cx| {
                if this.editing_id.is_some() || this.embedded {
                    cx.propagate();
                    return;
                }
                cx.emit(ObligationsEvent::FocusTaskList);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &OpenAgentChat, window, cx| {
                // Embedded, the host owns the conversation it opens.
                if this.embedded {
                    cx.propagate();
                    return;
                }
                this.open_agent_chat(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(Self::on_search_space))
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
            .on_action(cx.listener(Self::on_toggle_mark))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_add_section))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_arrow_up))
            .on_action(cx.listener(Self::on_arrow_down))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
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
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child("Obligations"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(
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
                    .child({
                        let mut search =
                            Input::new(&self.search_input)
                            .cleanable(true)
                            .w(px(220.))
                            .flex_shrink_0();
                        if let Some(pill) = render_shortcut_pill(
                            window,
                            &ItemListFocusSearch,
                            OBLIGATIONS_CONTEXT,
                            cx,
                        ) {
                            search = search.suffix(pill);
                        }
                        search
                    })
                    .when(!embedded, |row| {
                        row.child(chrome_control_with_shortcut(
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
                    ))
                    }),
            )
            .children(render_status_filter(
                "obligations",
                &status_counts(
                    &LISTING_STATUSES,
                    self.items.iter().map(|o| self.standing_of(o.id)),
                ),
                &self.filter,
                |this: &mut Self, status, window, cx| this.set_filter(status, window, cx),
                cx,
            ))
            .child({
                let editor = self.inline_edit_input.clone();
                let row_host = self.host.clone();
                self.list.render(
                    "obligations-scroll",
                    &self.host,
                    move |item, state, window, cx| {
                        rows::render_obligation(item, state, &editor, &row_host, window, cx)
                    },
                    window,
                    cx,
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
                    .child(match self.list.marked_count() {
                        0 => SharedString::from("↑/↓ navigate · Enter edits · N adds · S adds section · Space selects · Del deletes · Cmd/Ctrl+↑/↓ reorders · ←/→ collapse/expand · Ctrl+J talks about it · Esc closes"),
                        n => SharedString::from(format!("{n} selected · Del deletes them · Space deselects")),
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::rows::fixture::Fixture;
    use gpui::{TestAppContext, VisualTestContext};
    use gpui_component::Root;
    use std::cell::RefCell;
    use std::rc::Rc;

    type Events = Rc<RefCell<Vec<ObligationsEvent>>>;

    fn open_view<'a>(
        fixture: &Fixture,
        embedded: bool,
        cx: &'a mut TestAppContext,
    ) -> (Entity<ObligationsView>, Events, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let slot = Rc::new(RefCell::new(None));
        let events: Events = Rc::new(RefCell::new(Vec::new()));
        let (store, node_id) = (fixture.store.clone(), fixture.node_id);
        let (slot_in, events_in) = (slot.clone(), events.clone());
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| ObligationsView::new(window, cx, store));
            cx.subscribe(&view, move |_, _, event: &ObligationsEvent, _| {
                events_in.borrow_mut().push(event.clone());
            })
            .detach();
            view.update(cx, |view, cx| {
                view.set_embedded(embedded, cx);
                view.open(node_id, "Web client", None, window, cx);
            });
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        draw(cx);
        (view, events, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    fn selected_obligation(
        view: &Entity<ObligationsView>,
        cx: &mut VisualTestContext,
    ) -> Option<Uuid> {
        view.read_with(cx, |view, _| view.selected_obligation_id())
    }

    #[gpui::test]
    fn obligations_status_filter_narrows_by_standing(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, _, cx) = open_view(&fixture, true, cx);
        let items = |view: &Entity<ObligationsView>, cx: &mut VisualTestContext| {
            view.read_with(cx, |v, _| v.list.items().count())
        };
        assert_eq!(items(&view, cx), 3);
        view.update_in(cx, |v, window, cx| v.set_filter(Some("planned"), window, cx));
        draw(cx);
        assert_eq!(items(&view, cx), 0);
        view.update_in(cx, |v, window, cx| v.set_filter(Some("not planned"), window, cx));
        assert_eq!(items(&view, cx), 3);
        view.update_in(cx, |v, window, cx| v.set_filter(None, window, cx));
        assert_eq!(items(&view, cx), 3);
    }

    #[gpui::test]
    fn obligations_highlight_item_expands_and_selects_it(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, _, cx) = open_view(&fixture, true, cx);
        let target = fixture.offline_obligation;
        view.update_in(cx, |view, window, cx| {
            view.list
                .set_collapsed(phase_row_key(PHASE_REQUIREMENTS), true);
            view.list
                .set_collapsed(group_row_key(PHASE_REQUIREMENTS, KIND_REQUIREMENT), true);
            view.list.set_collapsed(
                section_row_key(PHASE_REQUIREMENTS, KIND_REQUIREMENT, "Offline"),
                true,
            );
            view.search_input.update(cx, |input, cx| {
                input.set_value("nothing matches this", window, cx);
            });
            view.sync_search_from_input(window, cx);
            assert!(
                view.list
                    .rows()
                    .iter()
                    .all(|row| row.key() != target.to_string())
            );
            view.highlight_item(target, window, cx);
        });
        assert_eq!(selected_obligation(&view, cx), Some(target));
        view.read_with(cx, |view, cx| {
            assert!(view.search_query.is_empty());
            assert!(view.search_input.read(cx).text().to_string().is_empty());
        });

        // Items on another node are ignored.
        view.update_in(cx, |view, window, cx| {
            view.highlight_item(Uuid::new_v4(), window, cx);
        });
        assert_eq!(selected_obligation(&view, cx), Some(target));
    }

    #[gpui::test]
    fn obligations_change_markers_render(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, _, cx) = open_view(&fixture, true, cx);
        view.update_in(cx, |view, window, cx| {
            view.highlight_item(fixture.design_obligation, window, cx);
            view.set_change_markers(
                HashMap::from([
                    (fixture.offline_obligation, NetOp::Edited),
                    (fixture.design_obligation, NetOp::Added),
                ]),
                cx,
            );
        });
        draw(cx);
        assert_eq!(
            selected_obligation(&view, cx),
            Some(fixture.design_obligation)
        );
    }

    #[gpui::test]
    fn obligations_embedded_hands_escape_and_left_to_the_host(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, events, cx) = open_view(&fixture, true, cx);
        cx.dispatch_action(ObligationsClose);
        cx.dispatch_action(PaneFocusLeft);
        assert!(view.read_with(cx, |view, _| view.is_open()));
        assert!(events.borrow().is_empty());
    }

    #[gpui::test]
    fn obligations_standalone_closes_and_returns_to_the_tree(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, events, cx) = open_view(&fixture, false, cx);
        cx.dispatch_action(PaneFocusLeft);
        cx.dispatch_action(ObligationsClose);
        assert!(!view.read_with(cx, |view, _| view.is_open()));
        assert!(matches!(
            events.borrow().as_slice(),
            [ObligationsEvent::FocusTaskList, ObligationsEvent::Close]
        ));
    }

    #[gpui::test]
    fn obligations_ctrl_j_opens_the_selection_without_an_agent_capability(cx: &mut TestAppContext) {
        // The fixture node has no Agent capability: Ctrl+J is not gated on it.
        let fixture = Fixture::new();
        let (view, events, cx) = open_view(&fixture, false, cx);
        let target = fixture.offline_obligation;
        view.update_in(cx, |view, window, cx| {
            view.highlight_item(target, window, cx)
        });
        draw(cx);
        let selected = selected_obligation(&view, cx);
        assert_eq!(selected, Some(target));
        cx.dispatch_action(OpenAgentChat);
        assert!(matches!(
            events.borrow().as_slice(),
            [ObligationsEvent::OpenAgentChat { node_id, obligation_id }]
                if *node_id == fixture.node_id && *obligation_id == selected
        ));
    }

    #[gpui::test]
    fn obligations_embedded_leaves_ctrl_j_to_the_host(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (_, events, cx) = open_view(&fixture, true, cx);
        cx.dispatch_action(OpenAgentChat);
        assert!(events.borrow().is_empty());
    }

    #[gpui::test]
    fn obligations_row_click_selects_through_the_row_host(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, _, cx) = open_view(&fixture, false, cx);
        // The row a click would report: the first obligation, which is not
        // the one selected on open.
        let (host, target_ix, target) = view.read_with(cx, |view, _| {
            let (ix, id) = view
                .list
                .rows()
                .iter()
                .enumerate()
                .find_map(|(ix, row)| Some((ix, row.as_item()?.obligation.id)))
                .unwrap();
            (view.host.clone(), ix, id)
        });
        assert_ne!(selected_obligation(&view, cx), Some(target));
        // Row handlers run outside any entity update, with only `&mut App`.
        cx.update(|_, cx| host.push(ListAction::Select { row_ix: target_ix }, cx));
        draw(cx);
        assert_eq!(selected_obligation(&view, cx), Some(target));
    }
}
