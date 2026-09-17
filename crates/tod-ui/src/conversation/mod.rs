//! The conversation view: one conversation about a focus (the project, a
//! node, an obligation, or a plan step), its transcript, and the change set
//! of everything the agent did during it. Spec: `doc/conversation/spec.md`.
//!
//! Layout: a one-line header ([`header`]), then the transcript pane
//! ([`transcript`]), the change-set pane ([`change_set`]), and, when open,
//! the context pane ([`context_panel`]) side by side. The context pane
//! follows the change-set cursor ([`ConversationView::set_cursor`]).
//!
//! Keyboard: one focus handle for the whole view, and [`Pane`] says which
//! pane Up/Down act on. The transcript pane's stops are [`Stop`]s; the change
//! set's are its rows. Text fields follow the navigation/edit-mode
//! convention (`ui::key_context`).

mod change_set;
mod context_panel;
mod header;
mod keyboard;
mod transcript;

#[cfg(test)]
mod tests;

pub use keyboard::register_conversation_keyboard_bindings;

use crate::interview::agent::SharedAgent;
use crate::interview::{TodPaths, TodSettings};
use crate::ui::agent_chat::OpenAgentChat;
use crate::ui::agent_permission::queue_permission_request;
use crate::ui::app_nav::{AppDestination, AppNavMenu, HasAppNav, on_app_nav_toggle};
use crate::ui::key_context::set_input_tab_stop;
use crate::ui::pane_nav::{PaneFocusLeft, PaneFocusRight};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::views::rows::{NodeRowEvent, ObligationRowEvent, PlanStepRowEvent, RowHost};
use change_set::{ChangeKey, PendingReverse, Tab};
use context_panel::{ContextPanel, ContextTab};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Pixels, Render, ScrollHandle, SharedString, Styled, Task, Window,
    div, px,
};
use gpui_component::input::TextareaState;
use gpui_component::resizable::{h_resizable, resizable_panel};
use keyboard::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tod_core::conversation::context::focus_selection;
use tod_core::conversation::{
    ConversationConfig, ConversationDriver, ConversationEvent, ConversationStatus,
};
use tod_store::conversation::{
    ConversationRepo, ConversationSummary, Entity as ItemEntity, EntitySnapshot, Focus, NetChange,
    Turn, net_changes,
};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_USER, InterviewCommand, short_id};
use tod_store::outline::repos::NodeRepo;
use uuid::Uuid;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Polls between reloads when the store has not signalled a commit.
const FALLBACK_POLLS: u32 = 8;
const TRANSCRIPT_WIDTH: f32 = 420.;
const CONTEXT_WIDTH: f32 = 420.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationViewEvent {
    /// Back with no earlier focus: return to the view the user came from.
    Leave,
    /// "Go to Tasks" in the context panel: show the node in the Tasks view,
    /// with the obligation highlighted when there is one.
    GoToTasks {
        node_id: Uuid,
        obligation_id: Option<Uuid>,
    },
}

/// Which pane Up/Down act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pane {
    Transcript,
    ChangeSet,
    /// The context panel; keyboard focus is in its hosted list.
    Context,
}

/// A keyboard stop in the transcript pane, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Back,
    Picker,
    Input,
    /// Stop the turn in flight (only while one runs).
    Stop,
}

/// Where Back returns to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HistoryEntry {
    pub focus: Focus,
    /// The conversation that was open; `None` for an unsaved one.
    pub conversation: Option<Uuid>,
}

/// The focuses the user came through, most recent last.
#[derive(Debug, Default)]
pub(crate) struct FocusHistory {
    entries: Vec<HistoryEntry>,
}

impl FocusHistory {
    pub fn push(&mut self, entry: HistoryEntry) {
        if self.entries.last() != Some(&entry) {
            self.entries.push(entry);
        }
    }

    pub fn pop(&mut self) -> Option<HistoryEntry> {
        self.entries.pop()
    }
}

/// What the change-set rows report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChangeAction {
    /// Clicked the row for `changes[ix]`.
    Select(usize),
    Toggle(ChangeKey),
    Edit(ChangeKey),
    Reverse(ChangeKey),
    ClearFlag(ChangeKey),
    Talk(ChangeKey),
    /// Show the row's item in the context panel.
    Context(ChangeKey),
    /// Follow reference `link` of `changes[ix]`.
    OpenLink {
        ix: usize,
        link: usize,
    },
    Ignore,
}

impl From<ObligationRowEvent> for ChangeAction {
    fn from(event: ObligationRowEvent) -> Self {
        match event {
            ObligationRowEvent::Select { row_ix } => Self::Select(row_ix),
            ObligationRowEvent::StartEdit { obligation_id } => {
                Self::Edit((ItemEntity::Obligation, obligation_id))
            }
            ObligationRowEvent::OpenVisualDesign { .. } => Self::Ignore,
        }
    }
}

impl From<PlanStepRowEvent> for ChangeAction {
    fn from(event: PlanStepRowEvent) -> Self {
        match event {
            PlanStepRowEvent::Select { row_ix } => Self::Select(row_ix),
            PlanStepRowEvent::StartEdit { step_id } => Self::Edit((ItemEntity::PlanStep, step_id)),
        }
    }
}

impl From<NodeRowEvent> for ChangeAction {
    fn from(event: NodeRowEvent) -> Self {
        match event {
            NodeRowEvent::Select { row_ix } => Self::Select(row_ix),
            NodeRowEvent::StartEdit { node_id } => Self::Edit((ItemEntity::Node, node_id)),
        }
    }
}

/// Everything the view shows that comes from the database.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Snapshot {
    /// Titles from the root down to the focus's node; empty for the project.
    pub path: Vec<String>,
    pub title: String,
    /// Conversations about the focus, newest first.
    pub conversations: Vec<ConversationSummary>,
    pub turns: Vec<Turn>,
    /// Grouped by node in tree order (see `net_changes`).
    pub changes: Vec<NetChange>,
    /// Titles of the nodes the changes live on.
    pub node_titles: HashMap<Uuid, String>,
}

/// An underline tab bar whose underline moves in the same frame as the
/// selection. `TabBar`'s sliding indicator keeps per-id state and springs
/// from the tab it left, so the underline trails the label; keyed by the
/// selected tab, the bar starts fresh on every switch and the selected tab
/// paints its own underline until the indicator (already on target) takes
/// over.
pub(crate) fn underline_tab_bar(id: &str, selected: usize) -> gpui_component::tab::TabBar {
    use gpui_component::Sizable;
    gpui_component::tab::TabBar::new(SharedString::from(format!("{id}-{selected}")))
        .underline()
        .small()
        .selected_index(selected)
}

pub struct ConversationView {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    focus_handle: FocusHandle,
    focus: Focus,
    /// Whether anything was opened yet; the initial project focus is not
    /// history.
    opened: bool,
    /// The open conversation; `None` for an unsaved new one.
    conversation_id: Option<Uuid>,
    history: FocusHistory,
    /// Drivers with work in flight, plus the current conversation's. Idle
    /// ones for other conversations are dropped: everything they know is in
    /// the database.
    drivers: Vec<ConversationDriver>,
    status: ConversationStatus,
    data: Snapshot,

    pane: Pane,
    stop: Stop,
    input: Entity<TextareaState>,
    input_editing: bool,
    /// The highlighted picker entry while the picker is open. The last entry
    /// (`conversations.len()`) is "New conversation".
    picker: Option<usize>,

    tab: Tab,
    cursor: Option<ChangeKey>,
    /// The highlighted reference link in the cursor's row, while the
    /// keyboard is on the links.
    link: Option<usize>,
    selected: HashSet<ChangeKey>,
    expanded: HashSet<ChangeKey>,
    editing: Option<ChangeKey>,
    edit_input: Entity<TextareaState>,
    confirm: Option<PendingReverse>,
    host: RowHost<ChangeAction>,
    change_scroll: ScrollHandle,
    /// Scroll the cursor's row into view on the next render.
    scroll_to_cursor: bool,
    transcript_scroll: ScrollHandle,
    /// Turns shown last render; more means scroll to the newest.
    rendered_turns: usize,
    context: ContextPanel,

    error: Option<SharedString>,
    status_line: SharedString,
    app_nav: AppNavMenu,
    _poll_task: Task<()>,
}

impl EventEmitter<ConversationViewEvent> for ConversationView {}

impl ConversationView {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        agent: SharedAgent,
        fleet: Arc<FleetStore>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .placeholder("Give direction — Enter to write, Ctrl+Enter to send")
        });
        let edit_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(2)
                .placeholder("Ctrl+Enter to save, Esc to cancel")
        });
        let poll_fleet = fleet.clone();
        let poll_task = cx.spawn(async move |this, cx| {
            let mut changes = poll_fleet.subscribe_changes();
            let mut idle = 0;
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let mut committed = false;
                loop {
                    match changes.try_recv() {
                        Ok(()) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                            committed = true
                        }
                        Err(_) => break,
                    }
                }
                idle += 1;
                if idle >= FALLBACK_POLLS {
                    committed = true;
                }
                if committed {
                    idle = 0;
                }
                let Ok(()) = this.update(cx, |this, cx| {
                    if this.poll(committed) {
                        cx.notify();
                    }
                }) else {
                    break;
                };
            }
        });
        let context = ContextPanel::new(fleet.clone(), window, cx);
        Self {
            host: RowHost::for_entity(cx.weak_entity()),
            fleet,
            agent,
            focus_handle: cx.focus_handle().tab_stop(true),
            focus: Focus::Project,
            opened: false,
            conversation_id: None,
            history: FocusHistory::default(),
            drivers: Vec::new(),
            status: ConversationStatus::default(),
            data: Snapshot::default(),
            pane: Pane::Transcript,
            stop: Stop::Input,
            input,
            input_editing: false,
            picker: None,
            tab: Tab::All,
            cursor: None,
            link: None,
            selected: HashSet::new(),
            expanded: HashSet::new(),
            editing: None,
            edit_input,
            confirm: None,
            change_scroll: ScrollHandle::new(),
            scroll_to_cursor: false,
            transcript_scroll: ScrollHandle::new(),
            rendered_turns: 0,
            context,
            error: None,
            status_line: SharedString::default(),
            app_nav: AppNavMenu::default(),
            _poll_task: poll_task,
        }
    }

    pub fn close_app_nav(&mut self) {
        self.app_nav.close();
    }

    // Read by tests.
    #[allow(dead_code)]
    pub fn focus(&self) -> Focus {
        self.focus
    }

    #[allow(dead_code)]
    pub fn conversation_id(&self) -> Option<Uuid> {
        self.conversation_id
    }

    /// Open the latest conversation about `focus`, or an unsaved empty one.
    /// A focus different from the current one is pushed onto the history,
    /// unless `record` is false.
    pub fn open(
        &mut self,
        focus: Focus,
        record: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let latest = self
            .fleet
            .read(|conn| ConversationRepo::new(conn).latest_for_focus(focus))
            .ok()
            .flatten()
            .map(|c| c.id);
        self.show(focus, latest, record, cx);
        self.pane = Pane::Transcript;
        self.stop = Stop::Input;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Switch to `conversation` (or a new one) about `focus`.
    fn show(
        &mut self,
        focus: Focus,
        conversation: Option<Uuid>,
        record: bool,
        cx: &mut Context<Self>,
    ) {
        let here = HistoryEntry {
            focus: self.focus,
            conversation: self.conversation_id,
        };
        if record && self.opened && here.focus != focus {
            self.history.push(here);
        }
        let switching = !self.opened || self.focus != focus || self.conversation_id != conversation;
        self.opened = true;
        self.focus = focus;
        self.conversation_id = conversation;
        if switching {
            self.data = Snapshot::default();
            self.cursor = None;
            self.link = None;
            self.selected.clear();
            self.expanded.clear();
            self.editing = None;
            self.confirm = None;
            self.picker = None;
            self.error = None;
            self.status_line = SharedString::default();
            self.tab = Tab::All;
            self.rendered_turns = 0;
        }
        self.reload();
        self.status = self
            .current_driver()
            .map(|d| d.status())
            .unwrap_or_default();
        cx.notify();
    }

    /// Back to the previous focus, or leave the view when there is none.
    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.history.pop() {
            Some(entry) => {
                self.show(entry.focus, entry.conversation, false, cx);
                self.focus_handle.focus(window, cx);
            }
            None => cx.emit(ConversationViewEvent::Leave),
        }
    }

    /// Talk about `key`'s item: refocus on it, remembering where we were.
    fn talk_about(&mut self, key: ChangeKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(focus) = self.change(key).and_then(change_set::focus_of) else {
            return;
        };
        self.open(focus, true, window, cx);
    }

    fn new_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show(self.focus, None, false, cx);
        self.pane = Pane::Transcript;
        self.enter_input_edit(window, cx);
    }

    // ----- drivers and data ----------------------------------------------

    /// Tool-agnostic config for a new driver. Built on demand, so the view
    /// works without a resolved install (and in tests) until the first send.
    fn driver_config(&self) -> Result<ConversationConfig, String> {
        let paths = TodPaths::discover().map_err(|e| format!("{e:#}"))?;
        let settings = TodSettings::load(&paths).unwrap_or_default();
        let media =
            tod_core::media::MediaPaths::discover().map_err(|e| format!("Media bundle: {e}"))?;
        Ok(ConversationConfig {
            data_root: self.fleet.paths().root().to_path_buf(),
            media,
            launch: settings.interview_launch_options(),
            context: settings.interview_context.clone(),
        })
    }

    fn is_current(&self, driver: &ConversationDriver) -> bool {
        match self.conversation_id {
            Some(id) => driver.conversation_id() == Some(id),
            None => driver.conversation_id().is_none() && driver.focus() == self.focus,
        }
    }

    fn current_driver(&self) -> Option<&ConversationDriver> {
        self.drivers.iter().find(|d| self.is_current(d))
    }

    fn ensure_current_driver(&mut self) -> Result<usize, String> {
        if let Some(ix) = self.drivers.iter().position(|d| self.is_current(d)) {
            return Ok(ix);
        }
        let config = self.driver_config()?;
        let driver = match self.conversation_id {
            Some(id) => {
                ConversationDriver::open(config, &self.fleet, id).map_err(|e| format!("{e:#}"))?
            }
            None => ConversationDriver::new(config, self.focus),
        };
        self.drivers.push(driver);
        Ok(self.drivers.len() - 1)
    }

    /// Work in flight, for the close-window warning.
    pub fn running_work(&self) -> Vec<String> {
        self.drivers
            .iter()
            .filter(|d| d.status().running)
            .map(|d| {
                let id = d.conversation_id().map(short_id).unwrap_or_default();
                format!("Conversation agent running: {id}")
            })
            .collect()
    }

    /// Advance the drivers, and reload when the store changed. Returns
    /// whether anything visible changed.
    fn poll(&mut self, committed: bool) -> bool {
        let mut finished = false;
        let mut current_error = None;
        if let Ok(mut agent) = self.agent.try_lock() {
            for driver in &mut self.drivers {
                for event in driver.tick(&self.fleet, agent.as_mut()) {
                    finished = true;
                    if let ConversationEvent::TurnFinished { error: Some(error) } = event {
                        current_error = Some(error);
                    }
                }
            }
        }
        let current = self
            .current_driver()
            .map(|d| d.status())
            .unwrap_or_default();
        if let Some(request) = current.permission.clone() {
            queue_permission_request(self.agent.clone(), request);
        }
        // A finished run for another conversation leaves nothing to show.
        let conversation_id = self.conversation_id;
        let focus = self.focus;
        self.drivers.retain(|d| {
            let current = match conversation_id {
                Some(id) => d.conversation_id() == Some(id),
                None => d.conversation_id().is_none() && d.focus() == focus,
            };
            current || d.status().running
        });
        let mut changed = false;
        if current != self.status {
            self.status = current;
            changed = true;
        }
        if finished && current_error.is_none() && self.status.last_error.is_none() {
            self.status_line = SharedString::default();
        }
        if (committed || finished) && self.reload() {
            changed = true;
        }
        changed
    }

    /// Re-read everything shown; returns whether it changed.
    fn reload(&mut self) -> bool {
        let focus = self.focus;
        let id = self.conversation_id;
        let data = self.fleet.read(|conn| {
            let selection = focus_selection(conn, focus)?;
            let repo = ConversationRepo::new(conn);
            let conversations = repo.list_for_focus(focus)?;
            let (turns, mut changes, actions) = match id {
                Some(id) => (repo.turns(id)?, net_changes(conn, id)?, repo.actions(id)?),
                None => (Vec::new(), Vec::new(), Vec::new()),
            };
            // An item the conversation added and then reversed has neither a
            // `before` nor a `current`; show it as the log last saw it.
            let mut last_known: HashMap<ChangeKey, EntitySnapshot> = HashMap::new();
            for action in actions {
                if let Some(snapshot) = action.after.or(action.before) {
                    last_known.insert((action.entity, action.entity_id), snapshot);
                }
            }
            for change in &mut changes {
                if change.before.is_none() && change.current.is_none() {
                    change.before = last_known.get(&change_set::key_of(change)).cloned();
                }
            }
            let nodes = NodeRepo::new(conn);
            let mut node_titles = HashMap::new();
            for change in &changes {
                let Some(node_id) = change_set::node_of(change) else {
                    continue;
                };
                if node_titles.contains_key(&node_id) {
                    continue;
                }
                let title = match nodes.get(node_id)? {
                    Some(node) => node.title,
                    None => match last_known.get(&(ItemEntity::Node, node_id)) {
                        Some(snapshot) => format!("{} (removed)", snapshot.text()),
                        None => format!("(removed node {})", short_id(node_id)),
                    },
                };
                node_titles.insert(node_id, title);
            }
            let title = header::display_title(&selection);
            Ok(Snapshot {
                path: selection.path,
                title,
                conversations,
                turns,
                changes,
                node_titles,
            })
        });
        let data = match data {
            Ok(data) => data,
            Err(err) => {
                let message: SharedString = format!("{err:#}").into();
                let changed = self.error.as_ref() != Some(&message);
                self.error = Some(message);
                return changed;
            }
        };
        if data == self.data {
            return false;
        }
        self.data = data;
        let keys: HashSet<ChangeKey> = self.data.changes.iter().map(change_set::key_of).collect();
        self.selected.retain(|k| keys.contains(k));
        self.expanded.retain(|k| keys.contains(k));
        if self.editing.is_some_and(|k| !keys.contains(&k)) {
            self.editing = None;
        }
        self.clamp_cursor();
        // Markers and removed items may have changed.
        self.context.stale = true;
        true
    }

    fn change(&self, key: ChangeKey) -> Option<&NetChange> {
        self.data
            .changes
            .iter()
            .find(|c| change_set::key_of(c) == key)
    }

    /// The highlighted change; the context panel follows it.
    pub(crate) fn highlighted_change(&self) -> Option<&NetChange> {
        self.change(self.cursor?)
    }

    /// Move the change-set highlight. Every cursor change goes through here
    /// (or [`Self::clamp_cursor`]), so it is where the context panel
    /// retargets.
    pub(crate) fn set_cursor(&mut self, key: Option<ChangeKey>, cx: &mut Context<Self>) {
        if self.cursor != key {
            self.cursor = key;
            self.link = None;
            self.scroll_to_cursor = true;
            if self.editing.is_some() && self.editing != key {
                self.editing = None;
            }
            self.follow_cursor();
            cx.notify();
        }
    }

    /// Keep the cursor on a visible change.
    fn clamp_cursor(&mut self) {
        let visible = self.visible_keys();
        if self.cursor.is_none_or(|k| !visible.contains(&k)) {
            self.cursor = visible.first().copied();
            self.link = None;
            self.follow_cursor();
        }
        if self.link.is_some_and(|n| n >= self.cursor_links()) {
            self.link = None;
        }
    }

    fn visible_keys(&self) -> Vec<ChangeKey> {
        self.data
            .changes
            .iter()
            .filter(|c| self.tab.shows(c))
            .map(change_set::key_of)
            .collect()
    }

    /// Run `command` as the user, showing any error.
    fn command(&mut self, command: InterviewCommand) -> Option<serde_json::Value> {
        match self.fleet.interview(ACTOR_USER, command) {
            Ok(value) => {
                self.error = None;
                Some(value)
            }
            Err(err) => {
                self.error = Some(format!("{err:#}").into());
                None
            }
        }
    }

    // ----- the message input ---------------------------------------------

    fn text_editing(&self) -> bool {
        self.input_editing || self.editing.is_some()
    }

    fn enter_input_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pane = Pane::Transcript;
        self.stop = Stop::Input;
        self.input_editing = true;
        self.picker = None;
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.input.update(cx, |input, cx| input.focus(window, cx));
        });
    }

    fn exit_input_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input_editing = false;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let ix = match self.ensure_current_driver() {
            Ok(ix) => ix,
            Err(err) => {
                self.error = Some(err.into());
                cx.notify();
                return;
            }
        };
        let result = match self.agent.lock() {
            Ok(mut agent) => self.drivers[ix]
                .send(&self.fleet, agent.as_mut(), &text)
                .map_err(|e| format!("{e:#}")),
            Err(_) => Err("the agent is unavailable".to_string()),
        };
        match result {
            Ok(()) => {
                self.error = None;
                self.conversation_id = self.drivers[ix].conversation_id();
                self.status = self.drivers[ix].status();
                self.input
                    .update(cx, |input, cx| input.set_value("", window, cx));
            }
            Err(err) => self.error = Some(err.into()),
        }
        self.reload();
        cx.notify();
    }

    fn stop_turn(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.drivers.iter().position(|d| self.is_current(d)) else {
            return;
        };
        if let Ok(mut agent) = self.agent.lock()
            && let Err(err) = self.drivers[ix].cancel(&self.fleet, agent.as_mut())
        {
            self.error = Some(format!("{err:#}").into());
        }
        self.status = self.drivers[ix].status();
        self.reload();
        cx.notify();
    }

    // ----- navigation ------------------------------------------------------

    /// The transcript pane's stops, top to bottom.
    pub(crate) fn stops(&self) -> Vec<Stop> {
        let mut stops = vec![Stop::Back, Stop::Picker, Stop::Input];
        if self.status.running {
            stops.push(Stop::Stop);
        }
        stops
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.confirm.is_some() {
            return;
        }
        if let Some(ix) = self.picker {
            let last = self.data.conversations.len() as isize;
            self.picker = Some((ix as isize + delta).clamp(0, last) as usize);
            cx.notify();
            return;
        }
        match self.pane {
            Pane::Transcript => {
                let stops = self.stops();
                let ix = stops.iter().position(|s| *s == self.stop).unwrap_or(0) as isize;
                let ix = (ix + delta).clamp(0, stops.len() as isize - 1) as usize;
                self.stop = stops[ix];
                cx.notify();
            }
            Pane::Context => {}
            Pane::ChangeSet => {
                let keys = self.visible_keys();
                if keys.is_empty() {
                    return;
                }
                let ix = self
                    .cursor
                    .and_then(|k| keys.iter().position(|v| *v == k))
                    .map_or(0, |ix| {
                        (ix as isize + delta).clamp(0, keys.len() as isize - 1)
                    }) as usize;
                self.set_cursor(Some(keys[ix]), cx);
            }
        }
    }

    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.is_some() {
            self.confirm_reverse(cx);
            return;
        }
        if let Some(ix) = self.picker {
            self.choose_picker_entry(ix, window, cx);
            return;
        }
        match self.pane {
            Pane::Transcript => match self.stop {
                Stop::Back => self.go_back(window, cx),
                Stop::Picker => self.open_picker(cx),
                Stop::Input => self.enter_input_edit(window, cx),
                Stop::Stop => self.stop_turn(cx),
            },
            Pane::Context => {}
            Pane::ChangeSet => {
                if self.open_highlighted_link(cx) {
                    return;
                }
                if let Some(key) = self.cursor {
                    if !self.expanded.remove(&key) {
                        self.expanded.insert(key);
                    }
                    cx.notify();
                }
            }
        }
    }

    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.take().is_some() {
            cx.notify();
        } else if self.picker.take().is_some() {
            cx.notify();
        } else if self.pane == Pane::Context {
            self.focus_pane(Pane::ChangeSet, window, cx);
        } else if self.link.take().is_some() {
            cx.notify();
        } else if self.editing.is_some() {
            self.cancel_edit(window, cx);
        } else if self.input_editing {
            self.exit_input_edit(window, cx);
        } else if !self.selected.is_empty() {
            self.selected.clear();
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.save_edit(window, cx);
        } else if self.input_editing {
            self.send(window, cx);
        }
    }

    fn focus_pane(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        if pane == Pane::Context && !self.context.open {
            return;
        }
        self.pane = pane;
        self.picker = None;
        if pane != Pane::ChangeSet {
            self.link = None;
        }
        match pane {
            Pane::ChangeSet => {
                self.clamp_cursor();
                self.focus_handle.focus(window, cx);
            }
            Pane::Context => {
                if self.context.target.is_none() {
                    self.follow_cursor();
                }
                // Land on the target item, even if the list's selection
                // wandered since.
                self.context.stale = true;
                self.focus_context_list(window, cx);
            }
            Pane::Transcript => self.focus_handle.focus(window, cx),
        }
        cx.notify();
    }

    /// The pane to the left or right of the current one, if any.
    fn neighbor_pane(&self, right: bool) -> Option<Pane> {
        match (self.pane, right) {
            (Pane::Transcript, true) => Some(Pane::ChangeSet),
            (Pane::ChangeSet, true) => self.context.open.then_some(Pane::Context),
            (Pane::ChangeSet, false) => Some(Pane::Transcript),
            (Pane::Context, false) => Some(Pane::ChangeSet),
            _ => None,
        }
    }

    /// Ctrl+J inside the view: talk about the highlighted change, or about
    /// the item selected in the context panel. Anywhere else in the view it
    /// does nothing — the view is already the conversation.
    fn on_open_agent_chat(
        &mut self,
        _: &OpenAgentChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        if self.text_editing() {
            return;
        }
        match self.pane {
            Pane::ChangeSet => {
                if let Some(key) = self.cursor {
                    self.talk_about(key, window, cx);
                }
            }
            Pane::Context => {
                if let Some(focus) = self.context.conversation_focus(cx) {
                    self.open(focus, true, window, cx);
                }
            }
            Pane::Transcript => {}
        }
    }

    /// Nav-mode handlers yield while a text field is being edited.
    fn nav_guard(&self) -> bool {
        !self.text_editing()
    }

    fn drain_row_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for action in self.host.drain() {
            match action {
                ChangeAction::Select(ix) => {
                    let key = self.data.changes.get(ix).map(change_set::key_of);
                    if key.is_some() {
                        self.pane = Pane::ChangeSet;
                        self.picker = None;
                        self.set_cursor(key, cx);
                        if !self.text_editing() {
                            self.focus_handle.focus(window, cx);
                        }
                    }
                }
                ChangeAction::Toggle(key) => self.toggle_selected(key, cx),
                ChangeAction::Edit(key) => {
                    self.set_cursor(Some(key), cx);
                    self.start_edit(window, cx);
                }
                ChangeAction::Reverse(key) => self.reverse_keys(vec![key], cx),
                ChangeAction::ClearFlag(key) => self.clear_flag(key, cx),
                ChangeAction::Talk(key) => self.talk_about(key, window, cx),
                ChangeAction::Context(key) => self.show_change_in_context(key, window, cx),
                ChangeAction::OpenLink { ix, link } => {
                    self.link = Some(link);
                    self.open_link(ix, link, cx);
                }
                ChangeAction::Ignore => {}
            }
        }
    }
}

impl Focusable for ConversationView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl HasAppNav for ConversationView {
    fn app_nav_mut(&mut self) -> &mut AppNavMenu {
        &mut self.app_nav
    }

    fn app_nav_current(&self) -> Option<AppDestination> {
        Some(AppDestination::Conversation)
    }

    fn app_nav_fallback_focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }
}

macro_rules! nav_action {
    ($el:expr, $cx:expr, $action:ty, |$this:ident, $window:ident, $c:ident| $body:expr) => {
        $el.on_action($cx.listener(|$this, _: &$action, $window, $c| {
            if !$this.nav_guard() {
                $c.propagate();
                return;
            }
            let _ = &$window;
            $body;
            $c.stop_propagation();
        }))
    };
}

impl Render for ConversationView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_row_actions(window, cx);
        if self.context_has_focus(window, cx) {
            // A click into the hosted list moves the pane with it.
            self.pane = Pane::Context;
            self.link = None;
        } else if self.pane == Pane::Context && !self.context.open {
            self.pane = Pane::ChangeSet;
        }
        self.sync_context(window, cx);
        set_input_tab_stop(&self.input, self.input_editing, cx);
        set_input_tab_stop(&self.edit_input, self.editing.is_some(), cx);

        let root = div()
            .key_context(CONVERSATION_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .relative()
            .flex()
            .flex_col()
            .on_action(cx.listener(on_app_nav_toggle::<Self>))
            .on_action(cx.listener(Self::on_open_agent_chat))
            .on_action(
                cx.listener(|this, _: &ConversationSubmit, window, cx| this.submit(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ConversationEscape, window, cx| this.escape(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ConversationNew, window, cx| {
                this.cancel_edit(window, cx);
                this.new_conversation(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ConversationToggleContext, window, cx| {
                    if this.text_editing() {
                        cx.propagate();
                        return;
                    }
                    this.toggle_context(window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &ConversationGoToTasks, _, cx| {
                if this.text_editing() || !this.context.open {
                    cx.propagate();
                    return;
                }
                this.go_to_tasks(cx);
            }))
            .on_action(cx.listener(|this, _: &PaneFocusLeft, window, cx| {
                match this.neighbor_pane(false).filter(|_| !this.text_editing()) {
                    Some(pane) => this.focus_pane(pane, window, cx),
                    None => cx.propagate(),
                }
            }))
            .on_action(cx.listener(|this, _: &PaneFocusRight, window, cx| {
                match this.neighbor_pane(true).filter(|_| !this.text_editing()) {
                    Some(pane) => this.focus_pane(pane, window, cx),
                    None => cx.propagate(),
                }
            }))
            // Plain Left/Right walk the highlighted row's links first, and
            // move panes (the next binding) when they have nowhere to go.
            .on_action(cx.listener(|this, _: &ConversationLinkLeft, _, cx| {
                if this.text_editing() || !this.link_left(cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ConversationLinkRight, _, cx| {
                if this.text_editing() || !this.link_right(cx) {
                    cx.propagate();
                }
            }));
        let root = nav_action!(root, cx, ConversationUp, |this, window, cx| this
            .move_highlight(-1, cx));
        let root = nav_action!(root, cx, ConversationDown, |this, window, cx| this
            .move_highlight(1, cx));
        let root = nav_action!(root, cx, ConversationActivate, |this, window, cx| this
            .activate(window, cx));
        let root = nav_action!(root, cx, ConversationBack, |this, window, cx| this
            .go_back(window, cx));
        let root = nav_action!(root, cx, ConversationToggleSelect, |this, window, cx| {
            if this.pane == Pane::ChangeSet
                && let Some(key) = this.cursor
            {
                this.toggle_selected(key, cx)
            }
        });
        let root = nav_action!(root, cx, ConversationReverse, |this, window, cx| {
            if this.pane == Pane::ChangeSet {
                this.reverse_selection(cx)
            }
        });
        let root = nav_action!(root, cx, ConversationReverseAll, |this, window, cx| {
            if this.pane != Pane::Context {
                this.reverse_all(cx)
            }
        });
        let root = nav_action!(root, cx, ConversationEdit, |this, window, cx| {
            if this.pane == Pane::ChangeSet {
                this.start_edit(window, cx)
            }
        });
        let root = nav_action!(root, cx, ConversationClearFlag, |this, window, cx| {
            if this.pane == Pane::ChangeSet
                && let Some(key) = this.cursor
            {
                this.clear_flag(key, cx)
            }
        });
        // In the context pane, 1 and 2 pick its Obligations and Plan tabs.
        let root = nav_action!(root, cx, ConversationTabAll, |this, window, cx| {
            if this.pane == Pane::Context {
                this.set_context_tab(ContextTab::Obligations, window, cx)
            } else {
                this.set_tab(Tab::All, cx)
            }
        });
        let root = nav_action!(root, cx, ConversationTabUnsure, |this, window, cx| {
            if this.pane == Pane::Context {
                this.set_context_tab(ContextTab::Plan, window, cx)
            } else {
                this.set_tab(Tab::Unsure, cx)
            }
        });
        let root = nav_action!(root, cx, ConversationTabDeleted, |this, window, cx| {
            if this.pane != Pane::Context {
                this.set_tab(Tab::Deleted, cx)
            }
        });

        let header = self.render_header(window, cx);
        let transcript = self.render_transcript(window, cx);
        let changes = self.render_change_set(window, cx);
        let context = self
            .context
            .open
            .then(|| self.render_context_panel(window, cx));
        let confirm = self.render_confirm(window, cx);

        root.child(header)
            .when_some(self.error.clone(), |el, message| {
                el.child(
                    style::panel_header(div()).child(style::text_error(selectable_text(
                        "conversation-error",
                        message,
                        window,
                        cx,
                    ))),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .overflow_hidden()
                    .child(
                        h_resizable("conversation-panes")
                            .child(
                                resizable_panel()
                                    .size(px(TRANSCRIPT_WIDTH))
                                    .size_range(style::size::PANE_MIN..Pixels::MAX)
                                    .child(transcript),
                            )
                            .child(
                                resizable_panel()
                                    .size_range(style::size::PANE_MIN..Pixels::MAX)
                                    .child(changes),
                            )
                            .when_some(context, |panes, context| {
                                panes.child(
                                    resizable_panel()
                                        .size(px(CONTEXT_WIDTH))
                                        .size_range(style::size::PANE_MIN..Pixels::MAX)
                                        .child(context),
                                )
                            }),
                    ),
            )
            .children(confirm)
    }
}
