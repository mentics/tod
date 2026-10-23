//! The transcript column panel: one conversation's turns, read-only, reusing
//! `conversation/transcript.rs`'s entry rendering
//! ([`AgentConversationPanel`]).

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, Window, div,
};
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind, Turn, TurnRole};
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::ui::agent_conversation::{
    AgentConversationEvent, AgentConversationPanel, Entry, EntryKind, bind_panel_host_keys,
    forward_panel_keys,
};
use crate::ui::agent_runs::AgentRuns;
use crate::unified::panel::ColumnPanel;

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
    /// Looking for the node's latest conversation.
    searching: bool,
    /// A `sync_watch` is waiting out its throttle.
    sync_scheduled: bool,
    _load: gpui::Task<()>,
    _search: gpui::Task<()>,
    _sync: gpui::Task<()>,
}

/// The longest a streamed update waits to be shown, so a burst of them is
/// shown as one.
const WATCH_THROTTLE: std::time::Duration = std::time::Duration::from_millis(60);

struct Watching {
    node: Uuid,
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
            panel
        });
        // Read-only: a Send or Stop here has nothing to act on.
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
            stored: Arc::new(Vec::new()),
            stored_dirty: false,
            loaded: false,
            loading: false,
            reload_again: false,
            searching: false,
            sync_scheduled: false,
            _load: gpui::Task::ready(()),
            _search: gpui::Task::ready(()),
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
        this.watching = Some(Watching { node, runs, live: Vec::new(), _sub });
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

    /// Find the node's latest conversation off the UI thread, and move to it
    /// unless the panel has found one meanwhile.
    fn find_latest(&mut self, node: Uuid, cx: &mut Context<Self>) {
        if self.searching {
            return;
        }
        self.searching = true;
        let fleet = self.fleet.clone();
        self._search = cx.spawn(async move |this, cx| {
            let latest = cx
                .background_executor()
                .spawn(async move {
                    fleet
                        .read(|conn| ConversationRepo::new(conn).list_for_focus(Focus::Node(node)))
                        .ok()
                        .and_then(|list| {
                            list.into_iter()
                                .map(|s| s.conversation)
                                .filter(|c| !matches!(c.protocol, ProtocolKind::Outline | ProtocolKind::Chat))
                                .max_by_key(|c| c.updated_at)
                                .map(|c| c.id)
                        })
                })
                .await;
            this.update(cx, |this, cx| {
                this.searching = false;
                // Watching a node with nothing running here (a cloud node's
                // work arrives through the store): follow its newest
                // conversation, not the first one found.
                if let Some(id) = latest
                    && id != this.conversation_id
                    && (this.conversation_id.is_nil() || this.watching.is_some())
                {
                    this.retarget(id, cx);
                }
            })
            .ok();
        });
    }

    /// Move to the node's running conversation (else, when none is showing
    /// yet, its latest), and take its streamed reply.
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
        let (target, live) = match running {
            Some((conversation, parts)) => (Some(conversation), parts),
            None => {
                self.find_latest(node, cx);
                (None, Vec::new())
            }
        };
        if let Some(watching) = &mut self.watching {
            watching.live = live;
        }
        self.cloud = tod_core::cloud_sync::cloud_status(&self.fleet, node, tod_store::outline::uuid_blob::now_ms());
        match target {
            Some(id) if id != self.conversation_id => self.retarget(id, cx),
            _ => self.show(cx),
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
        "Transcript".into()
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
                     the agent's output is not streamed live from the cloud.",
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
        forward_panel_keys(div(), &self.panel, true)
            .size_full()
            .key_context(TRANSCRIPT_PANEL_CONTEXT)
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .children(self.render_cloud_status(window, cx))
            .child(div().flex_auto().min_h_0().child(self.panel.clone()))
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
