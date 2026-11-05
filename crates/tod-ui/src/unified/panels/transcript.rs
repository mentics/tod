//! The transcript column panel: one conversation's turns, read-only, reusing
//! `conversation/transcript.rs`'s entry rendering
//! ([`AgentConversationPanel`]).
//!
//! Watching a node it is the node's **activity** panel: a list, grouped by
//! stay in a phase, of the node's agent sessions, gate checks and rounds of
//! the phase's configured commands (`activity_timeline`), and beside it
//! what the chosen one shows: a session's transcript, a gate check's
//! criteria, or one collapsible chunk per command that ran.

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Render, ScrollHandle, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Window, div, px,
};
use gpui_component::ActiveTheme;
use gpui_component::scroll::Scrollbar;
use tod_store::conversation::{Conversation, ConversationRepo, Focus, ProtocolKind, Turn, TurnRole};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::ui::agent_conversation::{
    AgentConversationEvent, AgentConversationPanel, Entry, EntryKind, bind_panel_host_keys,
    forward_panel_keys,
};
use crate::ui::agent_runs::AgentRuns;
use crate::ui::style;
use crate::unified::panel::ColumnPanel;
use crate::unified::panels::activity_timeline::{self as timeline, Group, Item, ItemKind};

pub const TRANSCRIPT_PANEL_CONTEXT: &str = "TranscriptPanel";

/// Up/Down, Page Up/Down, and Left/Right (expand/collapse) on the turns.
pub fn register_transcript_panel_bindings(cx: &mut App) {
    bind_panel_host_keys(cx, TRANSCRIPT_PANEL_CONTEXT);
}

/// What the transcript panel shows about a turn — mirrors
/// `conversation/transcript.rs::entry_of`, without the gate-check YAML
/// summarizing this read-only view has no protocol context for.
fn entry_of(turn: &Turn, root: &std::path::Path) -> Entry {
    if turn.role == TurnRole::Continuation {
        return Entry::raw(true, "Sent automatically", turn.body.clone());
    }
    Entry {
        kind: match turn.role {
            TurnRole::User => EntryKind::User,
            TurnRole::Agent => EntryKind::Agent,
            TurnRole::Error => EntryKind::Error,
            TurnRole::Rotation | TurnRole::Continuation => EntryKind::Marker,
        },
        body: turn.body.clone(),
        parts: turn.parts.clone(),
        label: None,
        summary: None,
        live: false,
        images: turn.attachments.iter().map(|a| a.path(root)).collect(),
    }
}

pub struct TranscriptPanel {
    fleet: Arc<FleetStore>,
    conversation_id: Uuid,
    title: SharedString,
    focus_handle: FocusHandle,
    panel: Entity<AgentConversationPanel>,
    _subscription: Subscription,
    /// Reloads the turns when the store changes, so a running conversation
    /// is watched as it goes.
    _follow: gpui::Task<()>,
    /// Set when watching a node's lifecycle processor: the panel moves to
    /// whichever of the node's conversations is running, and shows the turn
    /// in flight as it streams.
    watching: Option<Watching>,
    /// What the cloud says of the node being watched, when it runs there:
    /// the transcript is only what its supervisor has synced so far, so the
    /// panel says whether anything is running and when it was last heard.
    cloud: Option<tod_core::cloud_sync::CloudStatus>,
    /// The gate check or command round shown instead of a transcript
    /// (`Watching::items`), when the node's activity is on one.
    shown_activity: Option<Uuid>,
    /// The stored turns, as entries. Shared so a load can compare against
    /// them off the UI thread.
    stored: Arc<Vec<Entry>>,
    /// `stored` changed since the panel was last given it whole.
    stored_dirty: bool,
    /// The turns have been read at least once.
    loaded: bool,
    /// A load is running; a change meanwhile asks for one more after it.
    loading: bool,
    reload_again: bool,
    /// The list of the node's conversations is being read.
    items_loading: bool,
    /// A `sync_watch` is waiting out its throttle.
    sync_scheduled: bool,
    _load: gpui::Task<()>,
    _items: gpui::Task<()>,
    /// Scroll position of the list of the watched node's conversations.
    runs_scroll: ScrollHandle,
    _sync: gpui::Task<()>,
}

/// The longest a streamed update waits to be shown, so a burst of them is
/// shown as one.
const WATCH_THROTTLE: std::time::Duration = std::time::Duration::from_millis(60);

/// How wide the list of the node's conversations is, beside the transcript.
const RUNS_WIDTH: f32 = 210.0;

/// What a lifecycle conversation is called in the list: the work it does
/// (and for a phase or evaluation, the state), not its protocol's name.
fn run_label(c: &Conversation) -> String {
    format!("{}{}", run_label_base(c), c.group_suffix())
}

fn run_label_base(c: &Conversation) -> String {
    if let Some(label) = c.transition_label() {
        return match c.protocol {
            ProtocolKind::Phase | ProtocolKind::Evaluate => {
                let mut chars = label.chars();
                chars.next().map(|f| f.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or(label)
            }
            _ => label,
        };
    }
    match c.protocol {
        ProtocolKind::Implementation => "Implement",
        ProtocolKind::Verification => "Verify",
        ProtocolKind::Review => "Review",
        ProtocolKind::Fix => "Fix review findings",
        ProtocolKind::Pr => "Pull request",
        ProtocolKind::Incoming => "Incoming changes",
        ProtocolKind::VisualDesign => "Visual design",
        ProtocolKind::Chat => "Chat",
        ProtocolKind::Outline => "Outline",
        ProtocolKind::GateCheck | ProtocolKind::OnEntry | ProtocolKind::Phase | ProtocolKind::Evaluate => "Lifecycle",
    }
    .to_string()
}

/// A timestamp as the list shows it: the time today, else the date and time.
fn format_time(ms: i64) -> String {
    use chrono::{Local, TimeZone};
    let Some(at) = Local.timestamp_millis_opt(ms).single() else {
        return String::new();
    };
    if at.date_naive() == Local::now().date_naive() {
        at.format("%H:%M").to_string()
    } else {
        at.format("%b %-d %H:%M").to_string()
    }
}

/// What the panel shows: a session's transcript or one gate check or round
/// of commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Session(Uuid),
    Entry(Uuid),
}

struct Watching {
    node: Uuid,
    /// The user chose a conversation from the list: stay on it instead of
    /// following the node's running (or latest) one.
    pinned: bool,
    /// The node's sessions, gate checks and command rounds, oldest first, as
    /// last read, and the same grouped by stay in a phase.
    items: Vec<Item>,
    groups: Vec<Group>,
    /// Stays the user opened; the rest are collapsed.
    expanded_groups: HashSet<i64>,
    /// Command chunks the user opened, by round and position.
    expanded_chunks: HashSet<(Uuid, usize)>,
    /// The conversation the node is running now (here or in the cloud).
    live_id: Option<Uuid>,
    runs: Entity<AgentRuns>,
    /// The streamed reply of the turn in flight.
    live: Vec<tod_agent::ReplyPart>,
    _sub: Subscription,
}

impl TranscriptPanel {
    pub fn new(
        conversation_id: Uuid,
        fleet: Arc<FleetStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let panel = cx.new(|cx| {
            let mut panel = AgentConversationPanel::new("Transcript", "", window, cx);
            panel.set_active(false, cx);
            // A run's conversation is driven by its runner (here or in the
            // cloud), never sent to from here.
            panel.set_read_only(true, cx);
            panel
        });
        let _subscription = cx.subscribe(&panel, |_, _, _: &AgentConversationEvent, _| {});
        let mut this = Self {
            fleet,
            conversation_id,
            title: "Transcript".into(),
            focus_handle: cx.focus_handle(),
            panel,
            _subscription,
            _follow: gpui::Task::ready(()),
            watching: None,
            cloud: None,
            shown_activity: None,
            stored: Arc::new(Vec::new()),
            stored_dirty: false,
            loaded: false,
            loading: false,
            reload_again: false,
            items_loading: false,
            sync_scheduled: false,
            _load: gpui::Task::ready(()),
            _items: gpui::Task::ready(()),
            runs_scroll: ScrollHandle::new(),
            _sync: gpui::Task::ready(()),
        };
        let mut fleet_rx = this.fleet.subscribe_changes();
        this._follow = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                let mut changed = false;
                while fleet_rx.try_recv().is_ok() {
                    changed = true;
                }
                if changed
                    && this
                        .update(cx, |this, cx| {
                            this.reload(cx);
                            this.load_items(cx);
                            // A watched node's cloud supervisor starts new
                            // conversations that arrive through the store.
                            this.sync_watch(cx);
                        })
                        .is_err()
                {
                    break;
                }
            }
        });
        this.reload(cx);
        this
    }

    /// A panel that follows `node`'s lifecycle processor from one agent
    /// session to the next, live.
    pub fn watching(
        node: Uuid,
        fleet: Arc<FleetStore>,
        runs: Entity<AgentRuns>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new(Uuid::nil(), fleet, window, cx);
        let _sub = cx.observe(&runs, |this, _, cx| this.schedule_sync(cx));
        this.watching = Some(Watching {
            node,
            pinned: false,
            items: Vec::new(),
            groups: Vec::new(),
            expanded_groups: HashSet::new(),
            expanded_chunks: HashSet::new(),
            live_id: None,
            runs,
            live: Vec::new(),
            _sub,
        });
        this.load_items(cx);
        this.sync_watch(cx);
        this
    }

    /// `sync_watch`, at most once per [`WATCH_THROTTLE`]: the run notifies on
    /// every streamed chunk, and each sync copies the reply so far.
    fn schedule_sync(&mut self, cx: &mut Context<Self>) {
        if self.sync_scheduled {
            return;
        }
        self.sync_scheduled = true;
        self._sync = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(WATCH_THROTTLE).await;
            this.update(cx, |this, cx| {
                this.sync_scheduled = false;
                this.sync_watch(cx);
            })
            .ok();
        });
    }

    /// Move to the node's running conversation (else, when none is showing
    /// yet, its latest activity), and take its streamed reply.
    fn sync_watch(&mut self, cx: &mut Context<Self>) {
        let Some(watching) = &self.watching else {
            return;
        };
        let node = watching.node;
        let focus = Focus::Node(node);
        let running = {
            let runs = watching.runs.read(cx);
            let mut found = None;
            let mut ix = 0;
            while let Some(slot) = runs.slot_by_index(ix) {
                ix += 1;
                if slot.focus == focus
                    && slot.status.running
                    && !matches!(slot.protocol, ProtocolKind::Outline | ProtocolKind::Chat)
                    && let Some(conversation) = slot.conversation_id
                {
                    found = Some((conversation, slot.status.parts.clone()));
                    break;
                }
            }
            found
        };
        self.cloud = tod_core::cloud_sync::cloud_status(&self.fleet, node, tod_store::outline::uuid_blob::now_ms());
        let pinned = self.watching.as_ref().is_some_and(|w| w.pinned);
        let running = running.or_else(|| self.cloud_live_turn(node));
        let live_id = running.as_ref().map(|(conversation, _)| *conversation);
        let chosen = match self.shown_activity {
            Some(entry) => Target::Entry(entry),
            None => Target::Session(self.conversation_id),
        };
        let (target, live) = match running {
            // The one the user chose stays; its reply streams in only while
            // it is the one running.
            Some((conversation, parts)) if pinned => (
                Some(chosen),
                if chosen == Target::Session(conversation) { parts } else { Vec::new() },
            ),
            // Mid-turn, here or in the node's cloud sandbox (whose reply
            // in flight arrives through the store): show it as it streams.
            Some((conversation, parts)) => (Some(Target::Session(conversation)), parts),
            None if pinned => (Some(chosen), Vec::new()),
            // Nothing running (a cloud node's work arrives through the
            // store): follow the latest thing that happened, a session, a
            // gate check or a round of commands.
            None => (self.latest_target(), Vec::new()),
        };
        if let Some(watching) = &mut self.watching {
            watching.live = live;
            watching.live_id = live_id;
        }
        match target {
            Some(Target::Session(id)) => {
                self.shown_activity = None;
                if id != self.conversation_id {
                    self.retarget(id, cx)
                } else {
                    self.show(cx)
                }
            }
            Some(Target::Entry(id)) => {
                self.shown_activity = Some(id);
                self.show(cx)
            }
            None => self.show(cx),
        }
    }

    /// The most recent of the watched node's sessions, gate checks and command
    /// rounds.
    fn latest_target(&self) -> Option<Target> {
        let watching = self.watching.as_ref()?;
        let latest = watching.items.iter().max_by_key(|i| i.at)?;
        Some(if latest.entry().is_some() { Target::Entry(latest.id) } else { Target::Session(latest.id) })
    }

    /// Read what the watched node has done (its sessions, gate checks and
    /// command rounds) off the UI thread, for the list.
    fn load_items(&mut self, cx: &mut Context<Self>) {
        let Some(node) = self.watching.as_ref().map(|w| w.node) else {
            return;
        };
        if self.items_loading {
            return;
        }
        self.items_loading = true;
        let fleet = self.fleet.clone();
        self._items = cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    let (mut conversations, entries) = fleet.read(|conn| {
                        anyhow::Ok((
                            ConversationRepo::new(conn).conversations_for_focus(Focus::Node(node))?,
                            tod_store::node_timeline::TimelineRepo::new(conn).list(node)?,
                        ))
                    })?;
                    conversations.retain(|c| !matches!(c.protocol, ProtocolKind::Outline | ProtocolKind::Chat));
                    conversations.sort_by_key(|c| (c.created_at, c.updated_at));
                    let current = tod_core::lifecycle::current_state(&fleet, node).unwrap_or_default();
                    let mut items: Vec<Item> = conversations
                        .iter()
                        .map(|c| Item {
                            id: c.id,
                            kind: ItemKind::Session,
                            label: run_label(c),
                            at: c.updated_at,
                            started: c.created_at,
                            failed: false,
                        })
                        .collect();
                    items.extend(timeline::entry_items(&entries));
                    items.sort_by_key(|i| (i.started, i.at));
                    let groups = timeline::build_groups(items.clone(), &entries, &current);
                    anyhow::Ok((items, groups))
                })
                .await;
            this.update(cx, |this, cx| {
                this.items_loading = false;
                match found {
                    Ok((items, groups)) => {
                        if let Some(watching) = &mut this.watching
                            && (watching.items != items || watching.groups != groups)
                        {
                            watching.items = items;
                            watching.groups = groups;
                            cx.notify();
                            // What is latest may have changed.
                            this.sync_watch(cx);
                        }
                    }
                    Err(err) => tracing::warn!("reading the node's activity: {err:#}"),
                }
            })
            .ok();
        });
    }

    /// Show the session or entry `id` and stay on it; `None` goes back to
    /// following the node's running (else latest) one.
    fn choose(&mut self, id: Option<Uuid>, cx: &mut Context<Self>) {
        let Some(watching) = &mut self.watching else {
            return;
        };
        watching.pinned = id.is_some();
        if let Some(id) = id {
            if watching.items.iter().any(|i| i.id == id && i.entry().is_some()) {
                self.shown_activity = Some(id);
            } else {
                self.shown_activity = None;
                if id != self.conversation_id {
                    self.retarget(id, cx);
                }
            }
        }
        // Takes the streamed reply only if this is the running one.
        self.sync_watch(cx);
        cx.notify();
    }

    fn toggle_group(&mut self, key: i64, cx: &mut Context<Self>) {
        if let Some(watching) = &mut self.watching
            && !watching.expanded_groups.remove(&key)
        {
            watching.expanded_groups.insert(key);
        }
        cx.notify();
    }

    fn toggle_chunk(&mut self, key: (Uuid, usize), cx: &mut Context<Self>) {
        if let Some(watching) = &mut self.watching
            && !watching.expanded_chunks.remove(&key)
        {
            watching.expanded_chunks.insert(key);
        }
        cx.notify();
    }

    /// The list, a header per stay in a phase (collapsed until opened) over
    /// its rows, with "Follow latest" above it.
    fn render_runs(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let watching = self.watching.as_ref()?;
        // Rows keep their own height (`flex_none`): a flex column shrinks its
        // children to fit, which squashed them all into the list's height
        // instead of scrolling. The scrollbar shows only when they overflow.
        let mut rows = div()
            .id("transcript-runs-scroll")
            .size_full()
            .flex()
            .flex_col()
            .p_1()
            .overflow_y_scroll()
            .track_scroll(&self.runs_scroll)
            .child(
                style::menu_item(div().id("transcript-run-follow"), !watching.pinned)
                    .flex_none()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| this.choose(None, cx)),
                    )
                    .child("Follow latest"),
            );
        for group in &watching.groups {
            let key = group.key;
            let open = watching.expanded_groups.contains(&key);
            let live = group.items.iter().any(|i| watching.live_id == Some(i.id));
            rows = rows.child(
                style::menu_item(div().id(SharedString::from(format!("transcript-group-{key}"))), false)
                    .flex_none()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| this.toggle_group(key, cx)),
                    )
                    .child(
                        style::list_group_chevron(div())
                            .child(if open { "\u{25be}" } else { "\u{25b8}" }),
                    )
                    .child(
                        div().flex_1().min_w_0().overflow_hidden().text_ellipsis().child(format!(
                            "{}{}",
                            if live && !open { "\u{25cf} " } else { "" },
                            group.title()
                        )),
                    )
                    .child(style::text_dense_muted(div()).flex_shrink_0().child(group.items.len().to_string())),
            );
            if !open {
                continue;
            }
            for item in &group.items {
                let id = item.id;
                let current = watching.pinned
                    && match item.entry() {
                        Some(_) => self.shown_activity == Some(id),
                        None => self.shown_activity.is_none() && self.conversation_id == id,
                    };
                let live = watching.live_id == Some(id);
                let mut label = div().flex_1().min_w_0().overflow_hidden().text_ellipsis();
                if item.failed {
                    label = label.text_color(style::color::danger());
                }
                rows = rows.child(
                    style::menu_item(div().id(SharedString::from(format!("transcript-run-{id}"))), current)
                        .flex_none()
                        .flex()
                        .items_center()
                        .pl_4()
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| this.choose(Some(id), cx)),
                        )
                        .child(label.child(format!("{}{}", if live { "\u{25cf} " } else { "" }, item.label)))
                        .child(style::text_dense_muted(div()).flex_shrink_0().child(format_time(item.at))),
                );
            }
        }
        Some(
            div()
                .id("transcript-runs")
                .relative()
                .flex_none()
                .w(px(RUNS_WIDTH))
                .h_full()
                .border_r_1()
                .border_color(cx.theme().border)
                .child(rows)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .child(Scrollbar::vertical(&self.runs_scroll)),
                )
                .into_any_element(),
        )
    }

    /// What a gate check or round of commands found: the criteria, or one
    /// chunk per command (collapsed until opened, red when it did not pass).
    fn render_entry(&self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let watching = self.watching.as_ref()?;
        let item = watching.items.iter().find(|i| i.id == id)?;
        let entry = item.entry()?;
        let mut body = div().id("activity-entry").size_full().overflow_y_scroll().flex().flex_col().gap_2().p_2();
        body = body.child(
            style::text_dense_muted(div())
                .child(format!("{} \u{b7} {}", entry.title, format_time(entry.at))),
        );
        match &item.kind {
            ItemKind::Gate(_) => {
                for (n, line) in entry.gate_lines().into_iter().enumerate() {
                    let clear = line.outcome == tod_store::outline::OUTCOME_PASS
                        || line.outcome == tod_store::outline::OUTCOME_WAIVED;
                    let mut row = style::chunk(div()).px_2().py_1().flex().flex_col();
                    if !clear {
                        row = style::log_row_error(row);
                    }
                    row = row.child(div().child(format!("{} \u{b7} {}", line.criterion, line.outcome)));
                    if !line.detail.is_empty() {
                        row = row.child(crate::ui::selectable_text::selectable_text(
                            SharedString::from(format!("activity-gate-{id}-{n}")),
                            line.detail,
                            window,
                            cx,
                        ));
                    }
                    body = body.child(row);
                }
            }
            ItemKind::Commands(_) => {
                for (n, command) in entry.commands().into_iter().enumerate() {
                    let key = (id, n);
                    let open = watching.expanded_chunks.contains(&key);
                    let failed = command.is_failure();
                    let mut header = style::chunk_header(div().id(SharedString::from(format!("activity-chunk-{id}-{n}"))))
                        .flex()
                        .w_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| this.toggle_chunk(key, cx)),
                        );
                    if failed {
                        header = style::log_row_error(header);
                    }
                    let verdict = match command.verdict.as_str() {
                        "broken" => "could not run".to_string(),
                        other => other.to_string(),
                    };
                    let mut chunk = style::chunk(div()).flex().flex_col().child(
                        header
                            .child(style::list_group_chevron(div()).child(if open { "\u{25be}" } else { "\u{25b8}" }))
                            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().child(command.name.clone()))
                            .child(style::text_dense_muted(div()).flex_shrink_0().child(verdict)),
                    );
                    if failed {
                        chunk = chunk.border_color(style::color::danger());
                    }
                    if open {
                        let text = if command.output.trim().is_empty() {
                            "(no output)".to_string()
                        } else {
                            command.output.clone()
                        };
                        chunk = chunk.child(style::chunk_body(div()).child(
                            crate::ui::selectable_text::selectable_text(
                                SharedString::from(format!("activity-output-{id}-{n}")),
                                text,
                                window,
                                cx,
                            ),
                        ));
                    }
                    body = body.child(chunk);
                }
            }
            ItemKind::Session => return None,
        }
        Some(body.into_any_element())
    }

    /// The turn `node`'s cloud supervisor has in flight, as it last sent it
    /// (`tod_store::conversation_live`). Only while the supervisor is heard
    /// from: a row left by one that died is not a turn in progress.
    fn cloud_live_turn(&self, node: Uuid) -> Option<(Uuid, Vec<tod_agent::ReplyPart>)> {
        if !matches!(self.cloud, Some(tod_core::cloud_sync::CloudStatus::Working { .. })) {
            return None;
        }
        match self.fleet.read(|conn| tod_store::conversation_live::get(conn, node)) {
            Ok(live) => live.map(|live| (live.conversation_id, live.parts)),
            Err(err) => {
                tracing::warn!("reading the cloud node's reply in flight: {err:#}");
                None
            }
        }
    }

    #[cfg(test)]
    pub fn conversation_id(&self) -> Uuid {
        self.conversation_id
    }

    /// Point this column at a different conversation, in place.
    pub fn retarget(&mut self, conversation_id: Uuid, cx: &mut Context<Self>) {
        self.conversation_id = conversation_id;
        self.reload(cx);
    }

    /// Read the turns on the background executor, and show them if they
    /// differ from what is shown. Asking while a read is running asks for one
    /// more after it, not one each, so a busy store costs one read at a time.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let id = self.conversation_id;
        if id.is_nil() {
            // Nothing to read yet: a watching panel is still looking.
            self.loaded = true;
            self.show(cx);
            return;
        }
        if self.loading {
            self.reload_again = true;
            return;
        }
        self.loading = true;
        let fleet = self.fleet.clone();
        let known = self.stored.clone();
        self._load = cx.spawn(async move |this, cx| {
            let (entries, session_name) = cx
                .background_executor()
                .spawn(async move {
                    let (turns, session_name) = fleet
                        .read(|conn| {
                            let repo = ConversationRepo::new(conn);
                            let turns = repo.turns(id)?;
                            let session_name = repo.get(id)?.and_then(|c| c.session_name);
                            anyhow::Ok((turns, session_name))
                        })
                        .unwrap_or_default();
                    let root = fleet.paths().root();
                    let entries: Vec<Entry> = turns.iter().map(|turn| entry_of(turn, root)).collect();
                    // `None`: the same turns as are shown.
                    ((*known != entries).then_some(entries), session_name)
                })
                .await;
            this.update(cx, |this, cx| this.loaded_turns(id, entries, session_name, cx)).ok();
        });
    }

    fn loaded_turns(
        &mut self,
        id: Uuid,
        entries: Option<Vec<Entry>>,
        session_name: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.loading = false;
        if id == self.conversation_id {
            self.loaded = true;
            if let Some(entries) = entries {
                self.stored = Arc::new(entries);
                self.stored_dirty = true;
            }
            self.title = session_name.unwrap_or_else(|| "Transcript".to_string()).into();
            self.show(cx);
        } else {
            // Pointed elsewhere while it read.
            self.reload_again = true;
        }
        if std::mem::take(&mut self.reload_again) {
            self.reload(cx);
        }
    }

    /// Push the stored turns, and the turn in flight, to the panel. The
    /// stored turns go whole only when they changed; a streamed update sends
    /// just the reply in flight.
    fn show(&mut self, cx: &mut Context<Self>) {
        let live = self
            .watching
            .as_ref()
            .filter(|watching| !watching.live.is_empty())
            .map(|watching| Entry::live_reply(watching.live.clone()));
        let message = if self.loaded { "No turns recorded yet." } else { "Loading…" };
        let whole = std::mem::take(&mut self.stored_dirty);
        let stored = self.stored.clone();
        self.panel.update(cx, |panel, cx| {
            panel.set_title(self.title.clone(), cx);
            if whole {
                let mut entries = Vec::with_capacity(stored.len() + 1);
                entries.extend(stored.iter().cloned());
                entries.extend(live);
                panel.set_entries(entries, cx);
            } else {
                panel.set_tail(stored.len(), live, cx);
            }
            panel.set_empty_message(message, cx);
        });
        cx.notify();
    }
}

impl ColumnPanel for TranscriptPanel {
    fn title(&self, _cx: &App) -> SharedString {
        if self.watching.is_some() { "Activity".into() } else { "Transcript".into() }
    }

}

impl Focusable for TranscriptPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TranscriptPanel {
    /// One line above the turns for a node that runs in the cloud: whether
    /// its supervisor is working, and when it was last heard, so a stale
    /// transcript is not mistaken for a quiet node. Times are clock times,
    /// not ages: the line only changes when the store does.
    fn render_cloud_status(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        use tod_core::cloud_sync::CloudStatus;
        let at = |ms: i64| {
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
                .unwrap_or_default()
        };
        let (text, error) = match self.cloud.as_ref()? {
            CloudStatus::Working { heard_at, what } => (
                format!(
                    "Working in the cloud{} (supervisor last heard {}). This shows what it has synced so far; \
                     the reply in flight is sent every few seconds.",
                    if what.is_empty() { String::new() } else { format!(" while {what}") },
                    at(*heard_at)
                ),
                false,
            ),
            CloudStatus::Silent { heard_at, .. } => (
                format!(
                    "The cloud supervisor has not reported since {}: it stopped, or its news has not reached \
                     this app. Sync now looks again.",
                    at(*heard_at)
                ),
                true,
            ),
            CloudStatus::Idle => (
                "Nothing is running in the cloud for this node right now (it is waiting, finished, or not \
                 started). This is what it last synced."
                    .to_string(),
                false,
            ),
            CloudStatus::Deleted => ("Its cloud sandbox was deleted; run the node in the cloud to make a new one.".to_string(), true),
            CloudStatus::Lost => ("Its cloud sandbox is gone; the app makes a new one at its next sync.".to_string(), true),
        };
        let el = div().flex_none().px_2().py_1();
        let el = if error { crate::ui::style::text_error(el) } else { crate::ui::style::text_muted(el) };
        Some(
            el.child(crate::ui::selectable_text::selectable_text("transcript-cloud-status", text, window, cx))
                .into_any_element(),
        )
    }
}

impl Render for TranscriptPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.contains_focused(window, cx);
        self.panel.update(cx, |panel, cx| panel.set_active(focused, cx));
        let runs = self.render_runs(cx);
        let transcript = div()
            .flex_auto()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .children(self.render_cloud_status(window, cx))
            .child(match self.shown_activity.and_then(|id| self.render_entry(id, window, cx)) {
                Some(entry) => div().flex_auto().min_h_0().child(entry),
                None => div().flex_auto().min_h_0().child(self.panel.clone()),
            });
        forward_panel_keys(div(), &self.panel, true)
            .size_full()
            .key_context(TRANSCRIPT_PANEL_CONTEXT)
            .track_focus(&self.focus_handle)
            .flex()
            .children(runs)
            .child(transcript)
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
    use tod_store::conversation::{Focus, ProtocolKind, TurnRole};
    use tod_store::interview::{ACTOR_USER, InterviewCommand};

    fn add_conversation(fixture: &Fixture) -> Uuid {
        add_conversation_under(fixture, ProtocolKind::Outline)
    }

    fn add_conversation_under(fixture: &Fixture, protocol: ProtocolKind) -> Uuid {
        let id = Uuid::new_v4();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::CreateConversation {
                    id,
                    protocol,
                    focus: Focus::Node(fixture.node_id),
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AppendConversationTurn {
                    conversation_id: id,
                    role: TurnRole::User,
                    body: "Add offline support".into(),
                    parts: Vec::new(),
                    sent_context: None,
                    attachments: Vec::new(),
                },
            )
            .unwrap();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::AppendConversationTurn {
                    conversation_id: id,
                    role: TurnRole::Agent,
                    body: "Done".into(),
                    parts: Vec::new(),
                    sent_context: None,
                    attachments: Vec::new(),
                },
            )
            .unwrap();
        id
    }

    fn open_view<'a>(
        fixture: &Fixture,
        conversation_id: Uuid,
        cx: &'a mut TestAppContext,
    ) -> (Entity<TranscriptPanel>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let slot = Rc::new(RefCell::new(None));
        let store = fixture.store.clone();
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| TranscriptPanel::new(conversation_id, store, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        // The turns are read off the UI thread.
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (view, cx)
    }

    /// Let the throttle and the background reads finish.
    fn settle(cx: &mut VisualTestContext) {
        for _ in 0..3 {
            cx.executor().advance_clock(WATCH_THROTTLE * 2);
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn opens_a_conversation_and_shows_its_turns(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let conversation_id = add_conversation(&fixture);
        let (view, cx) = open_view(&fixture, conversation_id, cx);

        view.read_with(cx, |view, cx| {
            assert_eq!(view.conversation_id(), conversation_id);
            assert_eq!(view.panel.read(cx).entries().len(), 2);
        });
    }

    /// Nothing here sends to a run's conversation, so the panel offers no
    /// input or Send that would do nothing.
    #[gpui::test]
    fn the_transcript_offers_nothing_to_send(cx: &mut TestAppContext) {
        use crate::ui::agent_conversation::PanelStop;

        let fixture = Fixture::new();
        let conversation_id = add_conversation(&fixture);
        let (view, cx) = open_view(&fixture, conversation_id, cx);

        cx.update(|window, cx| {
            let panel = view.read(cx).panel.clone();
            panel.update(cx, |panel, cx| {
                let stops = panel.stops();
                assert!(!stops.contains(&PanelStop::Input), "{stops:?}");
                panel.start_editing(window, cx);
                assert!(!panel.is_editing());
            });
        });
    }

    /// A cloud node's conversations are made by its supervisor and arrive
    /// through the store, with nothing running here: the watch must move to
    /// each newer one, not stay on the first it found.
    #[gpui::test]
    fn watching_a_node_with_nothing_running_here_follows_its_newest_conversation(cx: &mut TestAppContext) {
        use crate::interview::agent::SharedAgent;
        use std::sync::{Arc, Mutex};

        let fixture = Fixture::new();
        let first = add_conversation_under(&fixture, ProtocolKind::Phase);
        cx.update(gpui_component::init);
        let agent: SharedAgent = Arc::new(Mutex::new(Box::new(
            crate::interview::agent::MockAgentProvider::new(),
        )));
        let runs = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));
        let (store, node, runs_in) = (fixture.store.clone(), fixture.node_id, runs.clone());
        let slot = std::rc::Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| TranscriptPanel::watching(node, store, runs_in, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        settle(cx);
        view.read_with(cx, |view, _| assert_eq!(view.conversation_id(), first));

        // The supervisor's next conversation syncs in.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = add_conversation_under(&fixture, ProtocolKind::Implementation);
        for _ in 0..6 {
            // The store announces a commit from a thread of its own.
            std::thread::sleep(std::time::Duration::from_millis(60));
            cx.executor().advance_clock(std::time::Duration::from_millis(250));
            cx.run_until_parked();
        }
        view.read_with(cx, |view, _| assert_eq!(view.conversation_id(), second));
    }

    /// A node working in the cloud streams its reply into the synced
    /// `conversation_live` row: the panel shows it after the stored turns,
    /// and drops it once the supervisor says the turn is over.
    #[gpui::test]
    fn watching_a_cloud_node_shows_the_reply_its_supervisor_has_in_flight(cx: &mut TestAppContext) {
        use crate::interview::agent::SharedAgent;
        use std::sync::{Arc, Mutex};

        let fixture = Fixture::new();
        let conversation = add_conversation_under(&fixture, ProtocolKind::Implementation);
        let now = tod_store::outline::uuid_blob::now_ms();
        let db = fixture.store.paths().db().to_path_buf();
        let write = |f: &dyn Fn(&rusqlite::Connection)| {
            let conn = rusqlite::Connection::open(&db).unwrap();
            f(&conn);
            fixture.store.reload_if_stale().unwrap();
        };
        write(&|conn| {
            tod_store::cloud_nodes::upsert(conn, fixture.node_id, "sandbox", "alice", now).unwrap();
            tod_store::cloud_activity::beat(conn, fixture.node_id, "implementing", now).unwrap();
            tod_store::conversation_live::set(
                conn,
                fixture.node_id,
                conversation,
                &[tod_agent::ReplyPart::Text { text: "reading the plan".into() }],
                now,
            )
            .unwrap();
        });
        cx.update(gpui_component::init);
        let agent: SharedAgent = Arc::new(Mutex::new(Box::new(
            crate::interview::agent::MockAgentProvider::new(),
        )));
        let runs = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));
        let (store, node, runs_in) = (fixture.store.clone(), fixture.node_id, runs.clone());
        let slot = std::rc::Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| TranscriptPanel::watching(node, store, runs_in, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        settle(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.conversation_id(), conversation);
            // Two stored turns and the reply in flight.
            assert_eq!(view.panel.read(cx).entries().len(), 3);
        });

        write(&|conn| tod_store::conversation_live::clear(conn, fixture.node_id).unwrap());
        for _ in 0..6 {
            std::thread::sleep(std::time::Duration::from_millis(60));
            cx.executor().advance_clock(std::time::Duration::from_millis(250));
            cx.run_until_parked();
        }
        view.read_with(cx, |view, cx| assert_eq!(view.panel.read(cx).entries().len(), 2));
    }

    /// Every conversation of the node is listed oldest first, one can be
    /// chosen and then stays while another runs, and "Follow latest" returns
    /// to the running one.
    #[gpui::test]
    fn a_chosen_conversation_stays_while_another_runs(cx: &mut TestAppContext) {
        use crate::interview::agent::SharedAgent;
        use std::sync::{Arc, Mutex};
        use tod_core::conversation::ConversationStatus;

        let fixture = Fixture::new();
        let first = add_conversation_under(&fixture, ProtocolKind::Implementation);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = add_conversation_under(&fixture, ProtocolKind::Verification);
        cx.update(gpui_component::init);
        let agent: SharedAgent = Arc::new(Mutex::new(Box::new(
            crate::interview::agent::MockAgentProvider::new(),
        )));
        let runs = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));
        let (store, node, runs_in) = (fixture.store.clone(), fixture.node_id, runs.clone());
        let slot = std::rc::Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| TranscriptPanel::watching(node, store, runs_in, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        settle(cx);
        view.read_with(cx, |view, _| {
            let items = &view.watching.as_ref().unwrap().items;
            assert_eq!(items.iter().map(|i| (i.id, i.label.as_str())).collect::<Vec<_>>(), vec![(first, "Implement"), (second, "Verify")]);
        });

        let live = ConversationStatus {
            running: true,
            parts: vec![tod_agent::ReplyPart::Text { text: "working".into() }],
            ..Default::default()
        };
        runs.update(cx, |runs, cx| {
            cx.notify();
            runs.host_elsewhere(Focus::Node(node), ProtocolKind::Verification, second, live).unwrap();
        });
        settle(cx);
        view.read_with(cx, |view, _| assert_eq!(view.conversation_id(), second));

        // Choosing the first stays on it, without the second's reply.
        view.update(cx, |view, cx| view.choose(Some(first), cx));
        settle(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.conversation_id(), first);
            assert_eq!(view.panel.read(cx).entries().len(), 2);
        });

        view.update(cx, |view, cx| view.choose(None, cx));
        settle(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.conversation_id(), second);
            assert_eq!(view.panel.read(cx).entries().len(), 3);
        });
    }

    /// Gate checks and command rounds are listed with the sessions, in groups
    /// that start collapsed; following the latest shows the newest of them,
    /// and a user's phase change is marked in its group's title.
    #[gpui::test]
    fn the_activity_lists_gates_and_commands_by_phase_and_follows_the_latest(cx: &mut TestAppContext) {
        use crate::interview::agent::SharedAgent;
        use std::sync::{Arc, Mutex};
        use tod_store::node_timeline::{CommandResult, GateLine, KIND_CHECKS, KIND_GATE, TimelineEntry};

        let fixture = Fixture::new();
        let session = add_conversation_under(&fixture, ProtocolKind::Implementation);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let gate = TimelineEntry::new(
            fixture.node_id,
            KIND_GATE,
            "active",
            "Gate check: active \u{2192} verifying",
            true,
            serde_json::to_string(&[GateLine { criterion: "Tests pass".into(), outcome: "fail".into(), detail: "2 failing".into() }]).unwrap(),
        );
        fixture.store.enqueue(tod_store::fleet::FleetMutation::RecordGateTimeline(Box::new(gate.clone()))).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let commands = TimelineEntry::new(
            fixture.node_id,
            KIND_CHECKS,
            "active",
            "Ran cargo test",
            true,
            serde_json::to_string(&[CommandResult {
                name: "cargo test".into(),
                command: "cargo test".into(),
                verdict: "failed".into(),
                output: "boom".into(),
            }])
            .unwrap(),
        );
        fixture.store.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(commands.clone()))).unwrap();
        fixture.store.writer().flush().unwrap();

        cx.update(gpui_component::init);
        let agent: SharedAgent = Arc::new(Mutex::new(Box::new(crate::interview::agent::MockAgentProvider::new())));
        let runs = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));
        let (store, node, runs_in) = (fixture.store.clone(), fixture.node_id, runs.clone());
        let slot = std::rc::Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| TranscriptPanel::watching(node, store, runs_in, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        settle(cx);
        view.read_with(cx, |view, cx| {
            let watching = view.watching.as_ref().unwrap();
            assert_eq!(
                watching.items.iter().map(|i| i.id).collect::<Vec<_>>(),
                vec![session, gate.id, commands.id]
            );
            assert!(watching.items[1].failed && watching.items[2].failed);
            // One stay, collapsed.
            assert_eq!(watching.groups.len(), 1);
            assert!(watching.expanded_groups.is_empty());
            // Following the latest: the commands, not the session.
            assert_eq!(view.shown_activity, Some(commands.id));
            assert!(view.title(cx) == "Activity");
        });

        // Choosing the session shows its transcript; Follow latest returns.
        view.update(cx, |view, cx| view.choose(Some(session), cx));
        settle(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.shown_activity, None);
            assert_eq!(view.conversation_id(), session);
        });
        view.update(cx, |view, cx| view.choose(Some(gate.id), cx));
        view.read_with(cx, |view, _| assert_eq!(view.shown_activity, Some(gate.id)));
        view.update(cx, |view, cx| view.choose(None, cx));
        settle(cx);
        view.read_with(cx, |view, _| assert_eq!(view.shown_activity, Some(commands.id)));
    }

    #[gpui::test]
    fn watching_shows_the_live_reply_and_moves_to_the_next_session(cx: &mut TestAppContext) {
        use crate::interview::agent::SharedAgent;
        use std::sync::{Arc, Mutex};
        use tod_core::conversation::ConversationStatus;

        let fixture = Fixture::new();
        let first = add_conversation(&fixture);
        let second = add_conversation(&fixture);
        cx.update(gpui_component::init);
        let agent: SharedAgent = Arc::new(Mutex::new(Box::new(
            crate::interview::agent::MockAgentProvider::new(),
        )));
        let runs = cx.new(|_| AgentRuns::new(fixture.store.clone(), agent));
        let (store, node, runs_in) = (fixture.store.clone(), fixture.node_id, runs.clone());
        let slot = std::rc::Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| TranscriptPanel::watching(node, store, runs_in, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();

        let live = |text: &str| ConversationStatus {
            running: true,
            parts: vec![tod_agent::ReplyPart::Text { text: text.into() }],
            ..Default::default()
        };
        // The first session is running and streaming.
        let slot_id = runs.update(cx, |runs, cx| {
            cx.notify();
            runs.host_elsewhere(Focus::Node(node), ProtocolKind::Phase, first, live("working")).unwrap()
        });
        settle(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.conversation_id(), first);
            // Two stored turns and the reply in flight.
            assert_eq!(view.panel.read(cx).entries().len(), 3);
        });

        // The next session takes over: the panel moves to it.
        runs.update(cx, |runs, cx| {
            cx.notify();
            runs.release_elsewhere(slot_id);
            runs.host_elsewhere(Focus::Node(node), ProtocolKind::Evaluate, second, live("evaluating")).unwrap();
        });
        settle(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.conversation_id(), second);
            assert_eq!(view.panel.read(cx).entries().len(), 3);
        });
    }
}
