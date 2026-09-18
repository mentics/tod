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
//! pane Up/Down act on. The transcript pane's stops are [`Stop`]s, the last of
//! which hands Up/Down to the transcript panel's own stops; the change set's
//! are its rows. Text fields follow the navigation/edit-mode convention
//! (`ui::key_context`).

mod change_set;
mod context_panel;
mod header;
mod keyboard;
mod nav;
mod side_pane;
mod transcript;

/// What the picker offers to start on `focus`. An outline conversation and a
/// chat work anywhere; an implementation conversation needs a node that is
/// `active` and has a plan to work through — the same conditions the lifecycle
/// panel's Implement checks. Visual design is not offered until its protocol
/// exists.
fn new_kinds(
    conn: &rusqlite::Connection,
    node: Option<Uuid>,
    focus: Focus,
) -> anyhow::Result<Vec<ProtocolKind>> {
    let mut kinds = vec![ProtocolKind::Outline, ProtocolKind::Chat];
    if let (Focus::Node(_), Some(node)) = (focus, node) {
        let active = NodeRepo::new(conn).get_lifecycle(node)?.as_deref() == Some("active");
        if active && !PlanStepRepo::new(conn).list_for_node(node)?.is_empty() {
            kinds.push(ProtocolKind::Implementation);
        }
    }
    Ok(kinds)
}

#[cfg(test)]
mod tests;

pub use keyboard::register_conversation_keyboard_bindings;

use crate::interview::agent::SharedAgent;
use crate::interview::{TodPaths, TodSettings};
use crate::ui::agent_chat::OpenAgentChat;
use crate::ui::agent_conversation::{AgentConversationPanel, PanelStop};
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
    IntoElement, ParentElement, Pixels, Render, ScrollHandle, SharedString, Styled, Subscription,
    Task, Window, div, px,
};
use gpui_component::input::TextareaState;
use gpui_component::resizable::{h_resizable, resizable_panel};
use keyboard::*;
use nav::NavMenu;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tod_core::conversation::context::focus_selection;
use tod_core::conversation::{
    ConversationConfig, ConversationDriver, ConversationEvent, ConversationStatus,
};
use tod_store::conversation::{
    ConversationRepo, ConversationSummary, Entity as ItemEntity, EntitySnapshot, Focus, NetChange,
    ProtocolKind, Turn, net_changes,
};
use tod_store::fleet::FleetStore;
use tod_store::interview::{ACTOR_USER, InterviewCommand, short_id};
use tod_store::outline::PlanStep;
use tod_store::outline::repos::{NodeRepo, PlanStepRepo};
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
    Forward,
    Picker,
    /// The transcript panel, whose own highlight (a chunk, the input, or
    /// Stop) Up/Down move.
    Transcript,
}

/// Where Back returns to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HistoryEntry {
    pub focus: Focus,
    /// The conversation that was open; `None` for an unsaved one.
    pub conversation: Option<Uuid>,
}

/// The focuses the user came through, most recent last, and the trail they
/// stepped back out of.
#[derive(Debug, Default)]
pub(crate) struct FocusHistory {
    entries: Vec<HistoryEntry>,
    /// Where Forward goes, the next one last. Filled by stepping back and
    /// abandoned by navigating somewhere new.
    ahead: Vec<HistoryEntry>,
}

impl FocusHistory {
    /// Record `entry` as where a fresh navigation started from.
    pub fn push(&mut self, entry: HistoryEntry) {
        if self.entries.last() != Some(&entry) {
            self.entries.push(entry);
        }
        self.ahead.clear();
    }

    /// Step back out of `here`, which becomes the head of the forward trail.
    pub fn back(&mut self, here: HistoryEntry) -> Option<HistoryEntry> {
        let entry = self.entries.pop()?;
        self.ahead.push(here);
        Some(entry)
    }

    /// Step forward again, `here` going back onto the back trail.
    pub fn forward(&mut self, here: HistoryEntry) -> Option<HistoryEntry> {
        let entry = self.ahead.pop()?;
        self.entries.push(here);
        Some(entry)
    }

    pub fn can_go_forward(&self) -> bool {
        !self.ahead.is_empty()
    }
}

/// What the change-set rows report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChangeAction {
    /// Clicked the row for `changes[ix]`.
    Select(usize),
    Toggle(ChangeKey),
    /// Show all of the row, or back to one line.
    Expand(ChangeKey),
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

/// One step of the header path: a node the user can click to focus on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Crumb {
    pub node: Uuid,
    pub title: String,
}

/// Everything the view shows that comes from the database.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Snapshot {
    /// The clickable steps above the focused item, root first: the nodes
    /// above a focused node, or the chain down to the node an obligation or
    /// plan step lives on. Empty for the project.
    pub path: Vec<Crumb>,
    /// The node whose children the header's drill-down lists; `None` for the
    /// project, whose children are the top-level nodes of every list.
    pub focus_node: Option<Uuid>,
    /// Whether the drill-down has anything to show.
    pub has_children: bool,
    pub title: String,
    /// Conversations about the focus, newest first.
    pub conversations: Vec<ConversationSummary>,
    pub turns: Vec<Turn>,
    /// Grouped by node in tree order (see `net_changes`).
    pub changes: Vec<NetChange>,
    /// Titles of the nodes the changes live on.
    pub node_titles: HashMap<Uuid, String>,
    /// Which protocol runs the open conversation.
    pub protocol: ProtocolKind,
    /// The focus node's plan steps, for protocols whose side pane shows them.
    pub plan: Vec<PlanStep>,
    /// The latest report a reply-parsing protocol stored.
    pub report: Option<serde_json::Value>,
    /// The kinds of conversation the picker offers to start on this focus.
    pub new_kinds: Vec<ProtocolKind>,
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
    transcript: Entity<AgentConversationPanel>,
    /// Whether the transcript's input is being written in; mirrors the
    /// panel, so navigation can check it without the app.
    input_editing: bool,
    _transcript_events: Subscription,
    /// The highlighted picker entry while the picker is open. The last entry
    /// (`conversations.len()`) is "New conversation".
    picker: Option<usize>,
    /// The header's drill-down into the focused item's children, when open.
    nav: Option<NavMenu>,

    /// The protocol a conversation opened from here runs. A stored
    /// conversation carries its own; this is what a new one gets.
    protocol: ProtocolKind,
    /// Files the implementation protocol's worktree has changed, refreshed
    /// off the main thread when a turn ends.
    side_files: Vec<String>,
    /// Turns the protocol's loop has sent since the last user message.
    loop_turns: u32,
    /// The highlighted row of a side pane other than the change set: plan
    /// steps first, then changed files.
    side_cursor: Option<usize>,
    side_scroll: ScrollHandle,
    side_scroll_pending: bool,

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
        let (transcript, transcript_events) = Self::new_transcript(window, cx);
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
                let Ok(want_files) = this.update(cx, |this, cx| {
                    let (changed, want_files) = this.poll(committed);
                    if changed {
                        cx.notify();
                    }
                    want_files
                }) else {
                    break;
                };
                // Reading the worktree spawns `git`, so it happens here — on
                // the background executor, between polls — never in `poll`
                // itself, which runs on the main thread.
                if let Some(cwd) = want_files {
                    let files = cx
                        .background_executor()
                        .spawn(async move { worktree_files(&cwd) })
                        .await;
                    let Ok(()) = this.update(cx, |this, cx| {
                        if this.side_files != files {
                            this.side_files = files;
                            cx.notify();
                        }
                    }) else {
                        break;
                    };
                }
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
            stop: Stop::Transcript,
            transcript,
            input_editing: false,
            _transcript_events: transcript_events,
            picker: None,
            nav: None,
            protocol: ProtocolKind::Outline,
            side_files: Vec::new(),
            loop_turns: 0,
            tab: Tab::All,
            cursor: None,
            link: None,
            selected: HashSet::new(),
            expanded: HashSet::new(),
            editing: None,
            edit_input,
            confirm: None,
            change_scroll: ScrollHandle::new(),
            side_cursor: None,
            side_scroll: ScrollHandle::new(),
            side_scroll_pending: false,
            scroll_to_cursor: false,
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
        self.open_with(focus, ProtocolKind::Outline, record, window, cx);
    }

    /// [`Self::open`], on the focus's most recent conversation running
    /// `protocol` — for `Implementation`, the node's one implementation
    /// conversation, reopened however many times it is launched.
    pub fn open_with(
        &mut self,
        focus: Focus,
        protocol: ProtocolKind,
        record: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.protocol = protocol;
        let latest = self
            .fleet
            .read(|conn| {
                let repo = ConversationRepo::new(conn);
                match protocol {
                    ProtocolKind::Outline => repo.latest_for_focus(focus),
                    other => repo.latest_for_focus_with_protocol(focus, other),
                }
            })
            .ok()
            .flatten()
            .map(|c| c.id);
        self.show(focus, latest, record, cx);
        self.pane = Pane::Transcript;
        self.stop = Stop::Transcript;
        self.transcript
            .update(cx, |panel, cx| panel.set_highlight(PanelStop::Input, cx));
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
        let here = self.here();
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
            self.side_cursor = None;
            self.link = None;
            self.selected.clear();
            self.expanded.clear();
            self.editing = None;
            self.confirm = None;
            self.picker = None;
            self.nav = None;
            self.error = None;
            self.status_line = SharedString::default();
            self.tab = Tab::All;
            self.transcript.update(cx, |panel, cx| panel.reset(cx));
        }
        self.reload();
        self.status = self
            .current_driver()
            .map(|d| d.status())
            .unwrap_or_default();
        cx.notify();
    }

    /// Where the view is now, as the history records it.
    fn here(&self) -> HistoryEntry {
        HistoryEntry {
            focus: self.focus,
            conversation: self.conversation_id,
        }
    }

    /// Back to the previous focus, or leave the view when there is none.
    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.history.back(self.here()) {
            Some(entry) => {
                self.show(entry.focus, entry.conversation, false, cx);
                self.focus_handle.focus(window, cx);
            }
            None => cx.emit(ConversationViewEvent::Leave),
        }
    }

    /// Forward again along the trail Back came down; nothing when there is
    /// none.
    fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.history.forward(self.here()) {
            self.show(entry.focus, entry.conversation, false, cx);
            self.focus_handle.focus(window, cx);
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
            None => ConversationDriver::new(config, self.focus, self.protocol),
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
    /// whether anything visible changed, and the worktree to re-read the
    /// changed files from (the caller does that off the main thread).
    fn poll(&mut self, committed: bool) -> (bool, Option<PathBuf>) {
        let mut finished = false;
        let mut current_error = None;
        let mut loop_turns = None;
        if let Ok(mut agent) = self.agent.try_lock() {
            let current = self.conversation_id;
            for driver in &mut self.drivers {
                for event in driver.tick(&self.fleet, agent.as_mut()) {
                    finished = true;
                    match event {
                        ConversationEvent::TurnFinished { error: Some(error) } => {
                            current_error = Some(error);
                        }
                        // The loop sent another turn: nothing ended, but the
                        // transcript has a new marker and the side pane's
                        // counter moved.
                        ConversationEvent::Continued
                        | ConversationEvent::TurnFinished { error: None } => {}
                        ConversationEvent::Rotated => {}
                    }
                    if driver.conversation_id() == current && current.is_some() {
                        loop_turns = Some(driver.continuations());
                    }
                }
            }
        }
        if let Some(turns) = loop_turns {
            self.loop_turns = turns;
        }
        let want_files = finished.then(|| self.implementation_worktree()).flatten();
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
        (changed, want_files)
    }

    /// The worktree an implementation conversation is running in, when that
    /// is what is open.
    fn implementation_worktree(&self) -> Option<PathBuf> {
        if self.data.protocol != ProtocolKind::Implementation {
            return None;
        }
        let node = self.focus.node_id()?;
        tod_store::fleet::provision::resolve_launch_cwd(&self.fleet, &node.to_string()).ok()
    }

    /// Re-read everything shown; returns whether it changed.
    fn reload(&mut self) -> bool {
        let focus = self.focus;
        let id = self.conversation_id;
        let fallback_protocol = self.protocol;
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
            // Ids for the path the header makes clickable. A focused node's
            // own title is the header title, so `selection.path` is one step
            // shorter than its chain and the zip drops the node itself.
            let chain = match selection.node {
                Some(node) => tod_store::outline::ancestor_chain(conn, node)?,
                None => Vec::new(),
            };
            let path = chain
                .into_iter()
                .zip(selection.path)
                .map(|(node, title)| Crumb { node, title })
                .collect();
            let protocol = match id {
                Some(id) => repo
                    .get(id)?
                    .map(|c| c.protocol)
                    .unwrap_or(fallback_protocol),
                None => fallback_protocol,
            };
            // Only the protocols whose side pane shows them pay for these.
            let (plan, report) = match (protocol, selection.node, id) {
                (ProtocolKind::Implementation, Some(node), id) => (
                    PlanStepRepo::new(conn).list_for_node(node)?,
                    id.and_then(|id| repo.latest_report(id).ok().flatten()),
                ),
                _ => (Vec::new(), None),
            };
            let new_kinds = new_kinds(conn, selection.node, focus)?;
            Ok(Snapshot {
                new_kinds,
                path,
                has_children: nav::has_children(conn, selection.node)?,
                focus_node: selection.node,
                title,
                conversations,
                turns,
                changes,
                node_titles,
                protocol,
                plan,
                report,
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
        self.stop = Stop::Transcript;
        self.input_editing = true;
        self.picker = None;
        self.transcript
            .update(cx, |panel, cx| panel.start_editing(window, cx));
        cx.notify();
    }

    fn exit_input_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input_editing = false;
        self.transcript
            .update(cx, |panel, cx| panel.stop_editing(window, cx));
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn send(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
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
                self.transcript
                    .update(cx, |panel, cx| panel.clear_input(window, cx));
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
        vec![Stop::Back, Stop::Forward, Stop::Picker, Stop::Transcript]
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.confirm.is_some() || self.nav_move(delta, cx) {
            return;
        }
        if let Some(ix) = self.picker {
            let last = (self.data.conversations.len() + self.data.new_kinds.len()) as isize - 1;
            self.picker = Some((ix as isize + delta).clamp(0, last) as usize);
            cx.notify();
            return;
        }
        match self.pane {
            Pane::Transcript => {
                if self.stop == Stop::Transcript {
                    let moved = self
                        .transcript
                        .update(cx, |panel, cx| panel.move_highlight(delta, cx));
                    if moved || delta > 0 {
                        return;
                    }
                }
                let stops = self.stops();
                let ix = stops.iter().position(|s| *s == self.stop).unwrap_or(0) as isize;
                let ix = (ix + delta).clamp(0, stops.len() as isize - 1) as usize;
                if stops[ix] == Stop::Transcript && self.stop != Stop::Transcript {
                    // Entering the panel from above lands on its first stop.
                    self.transcript.update(cx, |panel, cx| {
                        if let Some(first) = panel.stops().first().copied() {
                            panel.set_highlight(first, cx);
                        }
                    });
                }
                self.stop = stops[ix];
                cx.notify();
            }
            Pane::Context => {}
            Pane::ChangeSet if self.data.protocol == ProtocolKind::Implementation => {
                self.move_side_cursor(delta, cx);
            }
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
        if self.nav_activate(window, cx) {
            return;
        }
        if let Some(ix) = self.picker {
            self.choose_picker_entry(ix, window, cx);
            return;
        }
        match self.pane {
            Pane::Transcript => match self.stop {
                Stop::Back => self.go_back(window, cx),
                Stop::Forward => self.go_forward(window, cx),
                Stop::Picker => self.open_picker(cx),
                Stop::Transcript => {
                    if self.transcript.read(cx).highlight() == PanelStop::Input {
                        self.enter_input_edit(window, cx);
                    } else {
                        self.transcript
                            .update(cx, |panel, cx| panel.activate(window, cx));
                    }
                }
            },
            Pane::Context => {}
            Pane::ChangeSet => {
                if self.open_highlighted_link(cx) {
                    return;
                }
                if let Some(key) = self.cursor {
                    self.toggle_expanded(key, cx);
                }
            }
        }
    }

    fn toggle_expanded(&mut self, key: ChangeKey, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        cx.notify();
    }

    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.take().is_some() {
            cx.notify();
        } else if self.close_nav_menu(cx) {
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
            self.transcript.update(cx, |panel, cx| panel.submit(cx));
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
                ChangeAction::Expand(key) => self.toggle_expanded(key, cx),
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
                // Another of whatever kind is open.
                this.protocol = this.data.protocol;
                this.new_conversation(window, cx)
            }))
            // Works from anywhere in the view, text fields included.
            .on_action(
                cx.listener(|this, _: &ConversationToggleContext, window, cx| {
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
                if this.nav_collapse(cx) {
                    return;
                }
                if this.text_editing() || !this.link_left(cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ConversationLinkRight, _, cx| {
                if this.nav_expand(cx) {
                    return;
                }
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
        let root = nav_action!(root, cx, ConversationForward, |this, window, cx| this
            .go_forward(window, cx));
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
        let changes = self.render_side_pane(window, cx);
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

/// The worktree's changed files, one `git status --porcelain` line each.
/// Empty when it is not a repository, or git is not on the path — the pane
/// simply shows nothing rather than an error.
fn worktree_files(cwd: &std::path::Path) -> Vec<String> {
    std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim_end)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
