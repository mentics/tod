//! A general-purpose agent conversation panel: a transcript of collapsible
//! chunks above a message input.
//!
//! The panel knows nothing about where the conversation is stored or which
//! agent runs it. Its host hands it [`Entry`]s ([`AgentConversationPanel::set_entries`])
//! and the run status, and hears back through [`AgentConversationEvent`].
//!
//! The transcript itself is [`TranscriptList`], shared with every other
//! surface that shows one; this panel adds the message input and folds the
//! transcript's chunks into one keyboard model with it. Expansion state and
//! the highlight live here, not in the list, because a chunk and the input
//! are stops in the same sequence.
//!
//! Keyboard: the host owns focus and moves the panel's highlight with
//! [`AgentConversationPanel::move_highlight`] and
//! [`AgentConversationPanel::activate`]. While the input is being written in,
//! the panel's own bindings (Ctrl+Enter sends, Escape stops writing) apply,
//! and focus returns to the handle given to
//! [`AgentConversationPanel::set_return_focus`].

use crate::ui::key_context;
use crate::ui::key_context::set_input_tab_stop;
use crate::ui::style;
use crate::ui::transcript_list::{self, TranscriptList, TranscriptListEvent};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::{Selectable, Sizable, h_flex, v_flex};
use std::collections::HashMap;

pub use crate::ui::transcript_list::{ChunkId, Entry, EntryKind};

pub const AGENT_CONVERSATION_CONTEXT: &str = "AgentConversation";

const INPUT_HEIGHT: f32 = 104.;

actions!(
    agent_conversation,
    [
        /// Send the message being written.
        AgentConversationSubmit,
        /// Stop writing; focus returns to the host.
        AgentConversationEscape,
    ]
);

/// Register after the host's bindings, so these are tried first; each
/// propagates when the panel is not being written in.
pub fn register_agent_conversation_bindings(cx: &mut App) {
    let input = Some(key_context::including_input(AGENT_CONVERSATION_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("ctrl-enter", AgentConversationSubmit, input),
        KeyBinding::new("escape", AgentConversationEscape, input),
    ]);
}

/// A keyboard stop in the panel, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelStop {
    Chunk(ChunkId),
    Input,
    /// Stop the turn in flight (only while one runs).
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentConversationEvent {
    /// Send this message. The host clears the input
    /// ([`AgentConversationPanel::clear_input`]) once it is on its way.
    Send(String),
    /// Stop the turn in flight.
    Stop,
    /// The user clicked into the panel.
    Activated,
    /// Writing started or stopped.
    EditingChanged(bool),
}

pub struct AgentConversationPanel {
    entries: Vec<Entry>,
    /// Chunks the user expanded or collapsed; the rest show their default.
    toggled: HashMap<ChunkId, bool>,
    highlight: PanelStop,
    /// Whether the host's keyboard is on the panel, so its highlight shows.
    active: bool,
    running: bool,
    activity: Option<SharedString>,
    title: SharedString,
    empty_message: SharedString,
    /// Shown after the panel's own hint while not writing.
    extra_hint: Option<SharedString>,
    input: Entity<TextareaState>,
    editing: bool,
    return_focus: Option<FocusHandle>,
    /// The transcript above the input.
    list: Entity<TranscriptList>,
    _list_subscription: Subscription,
}

impl EventEmitter<AgentConversationEvent> for AgentConversationPanel {}

impl AgentConversationPanel {
    /// A panel titled `title`, whose input shows `placeholder` while empty.
    pub fn new(
        title: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = SharedString::from(placeholder.to_string());
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .placeholder(placeholder)
        });
        let list = cx.new(|_| TranscriptList::new());
        let subscription = cx.subscribe(&list, |this, _, event, cx| match event {
            // A click on a chunk moves the highlight there and toggles it,
            // exactly as Enter on that stop would.
            TranscriptListEvent::ChunkClicked(id) => {
                this.highlight = PanelStop::Chunk(*id);
                this.toggle(*id, cx);
                cx.emit(AgentConversationEvent::Activated);
            }
        });
        Self {
            entries: Vec::new(),
            toggled: HashMap::new(),
            highlight: PanelStop::Input,
            active: false,
            running: false,
            activity: None,
            title: SharedString::from(title.to_string()),
            empty_message: SharedString::default(),
            extra_hint: None,
            input,
            editing: false,
            return_focus: None,
            list,
            _list_subscription: subscription,
        }
    }

    pub fn set_extra_hint(&mut self, hint: impl Into<SharedString>) {
        self.extra_hint = Some(hint.into());
    }

    /// Where focus goes when the user stops writing.
    pub fn set_return_focus(&mut self, handle: FocusHandle) {
        self.return_focus = Some(handle);
    }

    pub fn set_empty_message(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        let message = message.into();
        if message != self.empty_message {
            self.empty_message = message;
            cx.notify();
        }
    }

    /// Show `entries`. Entries only ever append within one conversation; a
    /// different conversation starts with [`Self::reset`].
    pub fn set_entries(&mut self, entries: Vec<Entry>, cx: &mut Context<Self>) {
        if entries != self.entries {
            self.entries = entries;
            if !self.stops().contains(&self.highlight) {
                self.highlight = PanelStop::Input;
            }
            cx.notify();
        }
    }

    /// Forget what was expanded and where the highlight was: a different
    /// conversation is about to be shown.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.entries.clear();
        self.toggled.clear();
        self.highlight = PanelStop::Input;
        self.list.update(cx, |list, cx| list.reset(cx));
        cx.notify();
    }

    pub fn set_status(
        &mut self,
        running: bool,
        activity: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let activity = activity.map(SharedString::from);
        if running != self.running || activity != self.activity {
            self.running = running;
            self.activity = activity;
            if !running && self.highlight == PanelStop::Stop {
                self.highlight = PanelStop::Input;
            }
            cx.notify();
        }
    }

    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if active != self.active {
            self.active = active;
            cx.notify();
        }
    }

    // Read by tests.
    #[allow(dead_code)]
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn highlight(&self) -> PanelStop {
        self.highlight
    }

    #[allow(dead_code)]
    pub fn input(&self) -> &Entity<TextareaState> {
        &self.input
    }

    #[allow(dead_code)]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn clear_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_input("", window, cx);
    }

    pub fn set_input(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_string();
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    // ----- chunks ------------------------------------------------------------

    pub fn is_expanded(&self, id: ChunkId) -> bool {
        transcript_list::is_expanded(&self.entries, &self.toggled, id)
    }

    pub fn toggle(&mut self, id: ChunkId, cx: &mut Context<Self>) {
        let expanded = !self.is_expanded(id);
        if expanded == transcript_list::expanded_by_default(&self.entries, id) {
            self.toggled.remove(&id);
        } else {
            self.toggled.insert(id, expanded);
        }
        cx.notify();
    }

    /// The panel's stops, top to bottom.
    pub fn stops(&self) -> Vec<PanelStop> {
        let mut stops: Vec<PanelStop> = transcript_list::chunks(&self.entries, &self.toggled)
            .into_iter()
            .map(PanelStop::Chunk)
            .collect();
        stops.push(PanelStop::Input);
        if self.running {
            stops.push(PanelStop::Stop);
        }
        stops
    }

    /// Move the highlight by `delta` stops. Returns false, without moving,
    /// when that would leave the panel.
    pub fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let stops = self.stops();
        let ix = stops
            .iter()
            .position(|s| *s == self.highlight)
            .unwrap_or(stops.len() - 1) as isize;
        let next = ix + delta;
        if next < 0 || next >= stops.len() as isize {
            return false;
        }
        self.set_highlight(stops[next as usize], cx);
        true
    }

    pub fn set_highlight(&mut self, stop: PanelStop, cx: &mut Context<Self>) {
        self.highlight = stop;
        cx.notify();
    }

    /// Enter on the highlight: expand or collapse a chunk, start writing, or
    /// stop the turn.
    pub fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.highlight {
            PanelStop::Chunk(id) => self.toggle(id, cx),
            PanelStop::Input => self.start_editing(window, cx),
            PanelStop::Stop => cx.emit(AgentConversationEvent::Stop),
        }
    }

    // ----- the input -----------------------------------------------------------

    pub fn start_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.highlight = PanelStop::Input;
        if !self.editing {
            self.editing = true;
            cx.emit(AgentConversationEvent::EditingChanged(true));
        }
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.input.update(cx, |input, cx| input.focus(window, cx));
        });
    }

    pub fn stop_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing {
            return;
        }
        self.editing = false;
        if let Some(handle) = &self.return_focus {
            handle.focus(window, cx);
        }
        cx.emit(AgentConversationEvent::EditingChanged(false));
        cx.notify();
    }

    /// Ask the host to send what is written.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if !text.is_empty() {
            cx.emit(AgentConversationEvent::Send(text));
        }
    }
}

impl Render for AgentConversationPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        set_input_tab_stop(&self.input, self.editing, cx);

        // Bring the transcript up to date. The panel owns what is expanded
        // and where the highlight is, so both are pushed down each render.
        let entries = self.entries.clone();
        let toggled = self.toggled.clone();
        let chunk_highlight = match self.highlight {
            PanelStop::Chunk(id) => Some(id),
            _ => None,
        };
        let active = self.active;
        let running = self.running;
        let activity = self.activity.clone().map(|a| a.to_string());
        let empty_message = self.empty_message.clone();
        self.list.update(cx, |list, cx| {
            list.set_entries(entries, cx);
            list.set_toggled(toggled, cx);
            list.set_highlight(chunk_highlight, cx);
            list.set_active(active, cx);
            list.set_status(running, activity, cx);
            list.set_empty_message(empty_message, cx);
        });

        let input_highlighted = self.active && !self.editing && self.highlight == PanelStop::Input;
        let field = div()
            .id("agent-conversation-input")
            .w_full()
            .h(px(INPUT_HEIGHT))
            .overflow_hidden()
            .rounded(style::radius::CONTROL)
            .when(input_highlighted, style::highlighted)
            .on_click(cx.listener(|this, _, window, cx| {
                this.start_editing(window, cx);
                cx.emit(AgentConversationEvent::Activated);
            }))
            .child(
                Textarea::new(&self.input)
                    .disabled(!self.editing)
                    .w_full()
                    .h(px(INPUT_HEIGHT)),
            );

        let hint: SharedString = if self.editing {
            "Ctrl+Enter sends · Esc stops writing".into()
        } else {
            match &self.extra_hint {
                Some(extra) => format!("Enter to write or expand · {extra}").into(),
                None => "Enter to write or expand".into(),
            }
        };
        let stop_highlighted = self.active && self.highlight == PanelStop::Stop;

        v_flex()
            .key_context(AGENT_CONVERSATION_CONTEXT)
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .on_action(cx.listener(|this, _: &AgentConversationSubmit, _, cx| {
                if this.editing {
                    this.submit(cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &AgentConversationEscape, window, cx| {
                if this.editing {
                    this.stop_editing(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .child(
                style::panel_header(h_flex()).items_center().child(
                    if self.active {
                        style::text_title(div())
                    } else {
                        style::text_muted(div())
                    }
                    .child(self.title.clone()),
                ),
            )
            .child(div().flex_1().min_h_0().child(self.list.clone()))
            .child(
                style::panel_footer(v_flex()).child(field).child(
                    h_flex()
                        .items_center()
                        .gap(style::space::RELATED)
                        .child(
                            style::text_dense_muted(div())
                                .flex_1()
                                .min_w_0()
                                .child(hint),
                        )
                        .when(self.running, |el| {
                            el.child(
                                Button::new("agent-conversation-stop")
                                    .label("Stop")
                                    .ghost()
                                    .small()
                                    .selected(stop_highlighted)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(AgentConversationEvent::Stop)
                                    })),
                            )
                        })
                        .child(
                            Button::new("agent-conversation-send")
                                .label("Send")
                                .primary()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                        ),
                ),
            )
    }
}
