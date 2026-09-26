//! The chat drawer: a collapsible strip under columns 2+ (never under the
//! node tree) that shows the freeform conversation about the unified view's
//! current focus — the focused column's target node, or the tree selection
//! when nothing is focused. See `doc/ui/unified-view.md` ("The chat
//! drawer").
//!
//! Expanded, its top edge is a thick line that drags to make it taller or
//! shorter (`resize::ChatDrawerEdge`, handled by `UnifiedView`, which saves
//! the height), and its one header line — "Chat — {about}" and a collapse
//! chevron — takes the focused-column tint while the drawer has focus.
//!
//! Freeform means the plain/default protocol (`ProtocolKind::Outline`), the
//! same one Ctrl+J opens everywhere else. A structured lifecycle
//! conversation (implement, verify, review, fix, gate check, on-entry) never
//! shows here.
//!
//! Sending goes through the shared `ui::agent_runs::AgentRuns` registry
//! exactly as `conversation::ConversationView` does: this view only starts
//! and hands off turns, it never ticks a driver itself. The turn is
//! collected by whichever poll loop is already ticking `AgentRuns` — the app
//! keeps `ConversationView`'s alive for the app's whole lifetime — and this
//! view finds out through `cx.observe(&agent_runs, ..)`, not by polling on
//! its own.

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, MouseDownEvent, ParentElement, Pixels, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, actions, div,
    prelude::FluentBuilder, px, relative,
};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};
use tod_core::conversation::context::focus_selection;
use tod_core::conversation::{ConversationConfig, ConversationDriver, ConversationStatus, SharedAgentAccess};
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind, Turn, TurnRole};
use tod_store::fleet::FleetStore;

use crate::interview::agent::SharedAgent;
use crate::interview::{TodPaths, TodSettings};
use crate::ui::agent_conversation::{AgentConversationEvent, AgentConversationPanel, Entry, EntryKind};
use crate::ui::agent_runs::AgentRuns;
use crate::ui::key_context;
use crate::ui::style;
use crate::ui::terminal_handoff::{self, CONTINUE_IN_TERMINAL, OPEN_SHELL};
use crate::unified::resize::{CHAT_START_HEIGHT, ChatDrawerEdge, DIVIDER_WIDTH, DividerDrag};
use uuid::Uuid;

pub const CHAT_DRAWER_CONTEXT: &str = "ChatDrawer";

/// How thick the expanded drawer's top edge is drawn, inside its
/// `DIVIDER_WIDTH` grab area.
const EDGE_LINE: f32 = 3.;

actions!(chat_drawer, [ChatDrawerNewConversation]);

/// Registers Ctrl+N ("new conversation") inside the drawer, from either the
/// navigation or the writing context — mirrors `conversation::keyboard`'s
/// `ConversationNew`.
pub fn register_chat_drawer_keyboard_bindings(cx: &mut App) {
    let nav = Some(key_context::excluding_input(CHAT_DRAWER_CONTEXT));
    let input = Some(key_context::including_input(CHAT_DRAWER_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("ctrl-n", ChatDrawerNewConversation, nav),
        KeyBinding::new("ctrl-n", ChatDrawerNewConversation, input),
    ]);
}

fn entry_of(turn: &Turn) -> Entry {
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
    }
}

pub enum ChatDrawerEvent {
    /// The drawer collapsed; focus should go back to the columns.
    Collapsed,
}

pub struct ChatDrawer {
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    agent_runs: Entity<AgentRuns>,
    focus: Focus,
    /// What the header says this is about (a node's title, or "Project").
    about: SharedString,
    expanded: bool,
    /// The expanded height the user dragged it to; `None` is
    /// `CHAT_START_HEIGHT`.
    height: Option<Pixels>,
    conversation_id: Option<Uuid>,
    /// Whether the conversation has an agent session to continue elsewhere.
    has_session: bool,
    status: ConversationStatus,
    error: Option<SharedString>,
    transcript: Entity<AgentConversationPanel>,
    _transcript_events: Subscription,
    _agent_runs_sub: Subscription,
    focus_handle: FocusHandle,
}

impl ChatDrawer {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
        agent_runs: Entity<AgentRuns>,
        height: Option<Pixels>,
    ) -> Self {
        let transcript = cx.new(|cx| {
            let mut panel = AgentConversationPanel::new(
                "Chat",
                "Give direction — Enter to write, Ctrl+Enter to send",
                window,
                cx,
            );
            panel.set_extra_hint("Ctrl+N new conversation");
            // The drawer's own header line shows the title, beside its
            // collapse chevron.
            panel.set_header_visible(false, cx);
            panel
        });
        let transcript_events = cx.subscribe_in(&transcript, window, Self::on_transcript_event);
        let agent_runs_sub = cx.observe(&agent_runs, |this, _, cx| this.on_agent_runs_changed(cx));
        let mut drawer = Self {
            fleet,
            agent,
            agent_runs,
            focus: Focus::Project,
            about: "Project".into(),
            expanded: false,
            height,
            conversation_id: None,
            has_session: false,
            status: ConversationStatus::default(),
            error: None,
            transcript,
            _transcript_events: transcript_events,
            _agent_runs_sub: agent_runs_sub,
            focus_handle: cx.focus_handle(),
        };
        drawer.reload(cx);
        drawer
    }

    #[cfg(test)]
    pub fn expanded(&self) -> bool {
        self.expanded
    }

    #[cfg(test)]
    pub fn conversation_id(&self) -> Option<Uuid> {
        self.conversation_id
    }

    #[cfg(test)]
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// The expanded height, as dragged.
    pub fn height(&self) -> Option<Pixels> {
        self.height
    }

    pub fn set_height(&mut self, height: Pixels, cx: &mut Context<Self>) {
        if self.height != Some(height) {
            self.height = Some(height);
            cx.notify();
        }
    }

    /// Point the drawer at a new focus: swaps to that focus's latest
    /// freeform conversation (or an unsaved empty one).
    pub fn set_focus(&mut self, focus: Focus, cx: &mut Context<Self>) {
        if focus == self.focus {
            return;
        }
        self.focus = focus;
        self.conversation_id = self
            .fleet
            .read(|conn| ConversationRepo::new(conn).latest_for_focus(focus))
            .ok()
            .flatten()
            .map(|c| c.id);
        self.about = self.about_text();
        self.error = None;
        self.transcript.update(cx, |panel, cx| panel.reset(cx));
        self.reload(cx);
    }

    fn about_text(&self) -> SharedString {
        self.fleet
            .read(|conn| focus_selection(conn, self.focus))
            // An obligation's or plan step's `title` is its kind and short
            // id; what it says is in `text`, and that is what names it.
            .map(|s| {
                s.text
                    .as_deref()
                    .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
                    .map(|line| line.trim().to_string())
                    .unwrap_or(s.title)
            })
            .unwrap_or_else(|_| "Project".to_string())
            .into()
    }

    /// Expand the drawer, if it is not already.
    pub fn expand(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.expanded {
            self.toggle(window, cx);
        }
    }

    /// Expand or collapse the drawer.
    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.expanded = !self.expanded;
        if self.expanded {
            self.focus_handle.focus(window, cx);
            self.transcript.update(cx, |panel, cx| panel.set_active(true, cx));
        } else {
            cx.emit(ChatDrawerEvent::Collapsed);
        }
        cx.notify();
    }

    /// Start a fresh conversation about the current focus.
    pub fn new_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.conversation_id = None;
        self.error = None;
        self.expanded = true;
        self.transcript.update(cx, |panel, cx| panel.reset(cx));
        self.reload(cx);
        self.transcript.update(cx, |panel, cx| panel.start_editing(window, cx));
        cx.notify();
    }

    fn on_transcript_event(
        &mut self,
        _: &Entity<AgentConversationPanel>,
        event: &AgentConversationEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AgentConversationEvent::Send(text) => self.send(text, window, cx),
            AgentConversationEvent::Stop => self.stop_turn(cx),
            AgentConversationEvent::Activated | AgentConversationEvent::EditingChanged(_) => {
                cx.notify();
            }
            AgentConversationEvent::Action(id, _) if id.as_ref() == CONTINUE_IN_TERMINAL => {
                terminal_handoff::continue_in_terminal(
                    self.fleet.clone(),
                    self.agent.clone(),
                    self.driver_config(),
                    self.conversation_id,
                    self.status.running,
                    window,
                    cx,
                )
            }
            AgentConversationEvent::Action(id, _) if id.as_ref() == OPEN_SHELL => {
                terminal_handoff::open_shell(self.fleet.clone(), self.focus.node_id(), window, cx)
            }
            AgentConversationEvent::Action(..) => {}
        }
    }

    fn send(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let ix = match self.ensure_current_driver(cx) {
            Ok(ix) => ix,
            Err(err) => {
                self.error = Some(err.into());
                cx.notify();
                return;
            }
        };
        let Some((slot, mut driver)) = self.agent_runs.update(cx, |runs, cx| {
            let taken = runs.take_to_send(ix);
            cx.notify();
            taken
        }) else {
            self.error = Some("the agent is still working on the previous message".into());
            cx.notify();
            return;
        };
        self.error = None;
        self.refresh_status(cx);
        cx.notify();
        self.transcript.update(cx, |panel, cx| panel.clear_input(window, cx));
        let fleet = self.fleet.clone();
        let agent = self.agent.clone();
        cx.spawn(async move |this, cx| {
            let (driver, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = driver
                        .send(&fleet, &mut SharedAgentAccess(&agent), &text)
                        .map_err(|e| format!("{e:#}"));
                    (driver, result)
                })
                .await;
            let _ = this.update(cx, |this, cx| this.sent(slot, driver, result, cx));
        })
        .detach();
    }

    fn sent(
        &mut self,
        slot: u64,
        driver: ConversationDriver,
        result: Result<i64, String>,
        cx: &mut Context<Self>,
    ) {
        let conversation = driver.conversation_id();
        self.agent_runs.update(cx, |runs, cx| {
            runs.put_back(slot, driver);
            cx.notify();
        });
        match result {
            Ok(_) => {
                self.conversation_id = conversation;
                self.error = None;
            }
            Err(err) => self.error = Some(err.into()),
        }
        self.reload(cx);
        self.refresh_status(cx);
        cx.notify();
    }

    fn stop_turn(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.current_index(cx) else {
            return;
        };
        self.agent_runs.update(cx, |runs, cx| {
            let Some(slot) = runs.slot_by_index(ix).map(|s| s.id) else {
                return;
            };
            match runs.driver_mut(slot) {
                Some(driver) => {
                    let _ = driver.cancel(&self.fleet, &mut SharedAgentAccess(&self.agent));
                    let status = driver.status();
                    runs.set_status(slot, status);
                }
                None => runs.set_cancel(slot),
            }
            cx.notify();
        });
        self.refresh_status(cx);
        cx.notify();
    }

    fn current_index(&self, cx: &App) -> Option<usize> {
        self.agent_runs
            .read(cx)
            .find_index(self.focus, ProtocolKind::Outline, self.conversation_id)
    }

    fn ensure_current_driver(&mut self, cx: &mut Context<Self>) -> Result<usize, String> {
        let config = self.driver_config();
        let focus = self.focus;
        let conversation_id = self.conversation_id;
        let fleet = self.fleet.clone();
        self.agent_runs.update(cx, |runs, cx| {
            let result = runs.ensure(focus, ProtocolKind::Outline, conversation_id, move || {
                let config = config?;
                match conversation_id {
                    Some(id) => {
                        ConversationDriver::open(config, &fleet, id).map_err(|e| format!("{e:#}"))
                    }
                    None => Ok(ConversationDriver::new(config, focus, ProtocolKind::Outline)),
                }
            });
            cx.notify();
            result
        })
    }

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

    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let status = self
            .current_index(cx)
            .and_then(|ix| self.agent_runs.read(cx).status_at(ix))
            .unwrap_or_default();
        if status != self.status {
            self.status = status;
        }
    }

    fn on_agent_runs_changed(&mut self, cx: &mut Context<Self>) {
        self.refresh_status(cx);
        self.reload(cx);
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let turns = match self.conversation_id {
            Some(id) => self
                .fleet
                .read(|conn| ConversationRepo::new(conn).turns(id))
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let entries: Vec<Entry> = turns.iter().map(entry_of).collect();
        self.has_session = self.conversation_id.is_some_and(|id| {
            self.fleet
                .read(|conn| ConversationRepo::new(conn).get(id))
                .ok()
                .flatten()
                .is_some_and(|c| c.agent_session_id.is_some())
        });
        let tools =
            terminal_handoff::tools(self.focus.node_id(), self.has_session, self.status.running);
        let running = self.status.running;
        let activity = self.status.activity.clone();
        let about = self.about.clone();
        let empty = format!("No conversation about {about} yet. Give direction below.");
        self.transcript.update(cx, |panel, cx| {
            panel.set_entries(entries, cx);
            panel.set_tools(tools, cx);
            panel.set_status(running, activity, cx);
            panel.set_empty_message(empty, cx);
        });
        cx.notify();
    }
}

impl EventEmitter<ChatDrawerEvent> for ChatDrawer {}

impl Focusable for ChatDrawer {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChatDrawer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted, accent, drag_border) =
            (theme.border, theme.muted_foreground, theme.accent, theme.drag_border);
        // Its header is in the `column-focused` state while it has focus, as
        // a focused column's is; only one of them shows it.
        let focused = self.focus_handle.contains_focused(window, cx);

        if !self.expanded {
            return div()
                .id("chat-drawer-collapsed")
                .key_context(CHAT_DRAWER_CONTEXT)
                .track_focus(&self.focus_handle)
                .flex_shrink_0()
                .w_full()
                .border_t_1()
                .border_color(border)
                .map(|el| style::header_focusable(el, focused))
                .px_2()
                .py_1()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| this.toggle(window, cx)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_sm().child("Chat"))
                        .when(self.status.running, |el| {
                            el.child(div().text_xs().text_color(muted).child("Agent working…"))
                        }),
                )
                .into_any_element();
        }

        // The top edge: a thick line that drags to resize, in the accent
        // color while the drawer has focus, like a focused column's divider.
        let line = if focused { accent } else { muted.opacity(0.6) };
        let edge = div()
            .id("chat-drawer-edge")
            .flex_shrink_0()
            .w_full()
            .h(px(DIVIDER_WIDTH))
            .flex()
            .flex_col()
            .justify_center()
            .cursor_row_resize()
            .hover(move |el| el.bg(drag_border))
            .child(div().w_full().h(px(EDGE_LINE)).bg(line))
            .on_drag(ChatDrawerEdge, |_, _, _, cx| cx.new(|_| DividerDrag));

        // One header line: the title, any error, and the collapse chevron.
        // Clicking anywhere on it collapses the drawer.
        let error = self.error.clone();
        let header = div()
            .id("chat-drawer-header")
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(border)
            .map(|el| style::header_focusable(el, focused))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| this.toggle(window, cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_sm()
                    .child(format!("Chat — {}", self.about)),
            )
            .when_some(error, |el, err| {
                el.child(style::text_error(div()).text_xs().child(err))
            })
            .child(Icon::new(IconName::ChevronDown).small().text_color(muted));

        div()
            .id("chat-drawer")
            .key_context(CHAT_DRAWER_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &ChatDrawerNewConversation, window, cx| {
                this.new_conversation(window, cx)
            }))
            .flex_shrink_0()
            .w_full()
            .h(self.height.unwrap_or(px(CHAT_START_HEIGHT)))
            // A window made shorter than the dragged height still shows
            // some of the tree.
            .max_h(relative(0.85))
            .flex()
            .flex_col()
            .child(edge)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.transcript.clone()),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::agent_runs::AgentRuns;
    use crate::views::rows::fixture::Fixture;
    use gpui::{TestAppContext, VisualContext, VisualTestContext};
    use gpui_component::Root;
    use std::sync::Mutex;
    use tod_agent::MockAgentProvider;
    use tod_store::interview::{ACTOR_USER, InterviewCommand};
    use tod_store::outline::{CreatePosition, OutlineMutation};

    fn mock_agent() -> SharedAgent {
        Arc::new(Mutex::new(Box::new(MockAgentProvider::new())))
    }

    fn set_data_root() {
        let config_root = std::env::temp_dir().join(format!("tod-chat-drawer-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&config_root).unwrap();
        crate::interview::set_data_root(config_root);
    }

    fn open_drawer<'a>(
        fixture: &Fixture,
        agent: SharedAgent,
        cx: &'a mut TestAppContext,
    ) -> (Entity<ChatDrawer>, Entity<AgentRuns>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::agent_conversation::register_agent_conversation_bindings(cx);
            register_chat_drawer_keyboard_bindings(cx);
        });
        set_data_root();
        // The mock agent's directives (`think`, `add obligation`, …) are
        // interpreted by the global handler installed here, the same call
        // the real app makes in `app::window::open` for `--agent mock`;
        // without it the mock provider never resolves the run.
        tod_core::interview::mock::install_mock_interview_handler(
            fixture.store.paths().root().to_path_buf(),
        );
        let store = fixture.store.clone();
        let runs_slot: std::rc::Rc<std::cell::RefCell<Option<Entity<AgentRuns>>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let runs_slot_in = runs_slot.clone();
        let drawer_slot: std::rc::Rc<std::cell::RefCell<Option<Entity<ChatDrawer>>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let drawer_slot_in = drawer_slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let agent_runs = cx.new(|_| AgentRuns::new(store.clone(), agent.clone()));
            let drawer = cx.new(|cx| {
                ChatDrawer::new(window, cx, store.clone(), agent.clone(), agent_runs.clone(), None)
            });
            *runs_slot_in.borrow_mut() = Some(agent_runs.clone());
            *drawer_slot_in.borrow_mut() = Some(drawer.clone());
            Root::new(drawer, window, cx)
        });
        let drawer = drawer_slot.borrow_mut().take().unwrap();
        let runs = runs_slot.borrow_mut().take().unwrap();
        draw(cx);
        (drawer, runs, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// Ticks every running slot in `runs` to completion, exactly as the
    /// app-wide poll loop (`ConversationView`'s, kept alive for the app's
    /// life) does for every conversation's driver, including this drawer's.
    fn drain(runs: &Entity<AgentRuns>, fleet: &Arc<FleetStore>, agent: &SharedAgent, cx: &mut VisualTestContext) {
        for _ in 0..50 {
            let away = runs.update(cx, |runs, cx| {
                let taken = runs.take_running();
                cx.notify();
                taken
            });
            if away.is_empty() {
                cx.run_until_parked();
                return;
            }
            let ticked: Vec<_> = away
                .into_iter()
                .map(|(id, mut driver)| {
                    let _ = driver.tick(fleet, &mut SharedAgentAccess(agent));
                    (id, driver)
                })
                .collect();
            runs.update(cx, |runs, cx| {
                for (id, driver) in ticked {
                    runs.put_back(id, driver);
                }
                cx.notify();
            });
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[gpui::test]
    fn toggling_expands_and_collapses(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (drawer, _runs, cx) = open_drawer(&fixture, mock_agent(), cx);
        assert!(!drawer.read_with(cx, |d, _| d.expanded()));

        cx.update_window(cx.window_handle(), |_, window, cx| {
            drawer.update(cx, |d, cx| d.toggle(window, cx));
        })
        .unwrap();
        draw(cx);
        assert!(drawer.read_with(cx, |d, _| d.expanded()));

        cx.update_window(cx.window_handle(), |_, window, cx| {
            drawer.update(cx, |d, cx| d.toggle(window, cx));
        })
        .unwrap();
        draw(cx);
        assert!(!drawer.read_with(cx, |d, _| d.expanded()));
    }

    #[gpui::test]
    fn changing_focus_swaps_to_that_focus_latest_conversation(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let list_id = fixture.store.list_outline_lists().unwrap()[0].id;
        let other_node = Uuid::new_v4();
        fixture
            .store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(other_node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Other".into(),
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();

        let conv_a = Uuid::new_v4();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::CreateConversation {
                    id: conv_a,
                    protocol: ProtocolKind::Outline,
                    focus: Focus::Node(fixture.node_id),
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();
        let conv_b = Uuid::new_v4();
        fixture
            .store
            .interview(
                ACTOR_USER,
                InterviewCommand::CreateConversation {
                    id: conv_b,
                    protocol: ProtocolKind::Outline,
                    focus: Focus::Node(other_node),
                    platform: None,
                    model: None,
                    effort: None,
                },
            )
            .unwrap();

        let (drawer, _runs, cx) = open_drawer(&fixture, mock_agent(), cx);
        drawer.update(cx, |d, cx| d.set_focus(Focus::Node(fixture.node_id), cx));
        assert_eq!(drawer.read_with(cx, |d, _| d.conversation_id()), Some(conv_a));

        drawer.update(cx, |d, cx| d.set_focus(Focus::Node(other_node), cx));
        assert_eq!(drawer.read_with(cx, |d, _| d.conversation_id()), Some(conv_b));
    }

    #[gpui::test]
    fn sending_with_the_mock_agent_produces_a_reply(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let agent = mock_agent();
        let (drawer, runs, cx) = open_drawer(&fixture, agent.clone(), cx);
        drawer.update(cx, |d, cx| d.set_focus(Focus::Node(fixture.node_id), cx));

        cx.update_window(cx.window_handle(), |_, window, cx| {
            drawer.update(cx, |d, cx| d.send("think Hello there", window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        drain(&runs, &fixture.store, &agent, cx);

        let has_reply = drawer.read_with(cx, |d, cx| {
            d.transcript
                .read(cx)
                .entries()
                .iter()
                .any(|e| e.kind == EntryKind::Agent)
        });
        assert!(has_reply, "the mock agent's reply should be in the transcript");
    }
}
