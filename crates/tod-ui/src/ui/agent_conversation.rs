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
//!
//! A host can put its own buttons beside Send ([`AgentConversationPanel::set_actions`])
//! and status lines above the input, each with an optional button
//! ([`AgentConversationPanel::set_notices`]); the panel reports clicks on them
//! as [`AgentConversationEvent::Action`] and knows nothing of what they do.
//! Icon buttons in the input's own row, left of Send, are
//! [`AgentConversationPanel::set_tools`]; they report the same way.
//!
//! An image pasted while writing is attached to the message rather than
//! sent: it waits above the input, where Enter on it (or its ×) takes it off,
//! and goes out with the text in [`AgentConversationEvent::Send`].

use crate::ui::key_context;
use crate::ui::key_context::set_input_tab_stop;
use crate::ui::pasted_image::{self, ClipboardImage, PendingImage};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::ui::transcript_list::{self, StartState, TranscriptList, TranscriptListEvent};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement, IntoElement,
    KeyBinding, ObjectFit, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, StyledImage, Subscription, Window, actions, div, img, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Paste, Textarea, TextareaState};
use gpui_component::spinner::Spinner;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    Disableable, Icon, IconName, IconNamed, Selectable, Sizable, h_flex, v_flex,
};
use std::collections::HashMap;
use tod_agent::PromptImage;

pub use crate::ui::transcript_list::{ChunkId, Entry, EntryKind};

pub const AGENT_CONVERSATION_CONTEXT: &str = "AgentConversation";

const INPUT_HEIGHT: f32 = 104.;
/// Past this the notices scroll, so the input always stays in view.
const NOTICES_MAX_HEIGHT: f32 = 160.;

actions!(
    agent_conversation,
    [
        /// Send the message being written.
        AgentConversationSubmit,
        /// Stop writing; focus returns to the host.
        AgentConversationEscape,
        /// Keys a host that owns focus forwards to the panel; see
        /// [`bind_panel_host_keys`].
        PanelUp,
        PanelDown,
        PanelPageUp,
        PanelPageDown,
        PanelLeft,
        PanelRight,
        PanelActivate,
    ]
);

/// A navigation key a host forwards to the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKey {
    Up,
    Down,
    PageUp,
    PageDown,
    Left,
    Right,
    Activate,
}

/// Bind the navigation keys in a host's `context`, outside text input. The
/// host attaches them with [`forward_panel_keys`].
pub fn bind_panel_host_keys(cx: &mut App, context: &str) {
    let nav = Some(key_context::excluding_input(context));
    cx.bind_keys([
        KeyBinding::new("up", PanelUp, nav),
        KeyBinding::new("down", PanelDown, nav),
        KeyBinding::new("pageup", PanelPageUp, nav),
        KeyBinding::new("pagedown", PanelPageDown, nav),
        KeyBinding::new("left", PanelLeft, nav),
        KeyBinding::new("right", PanelRight, nav),
        KeyBinding::new("enter", PanelActivate, nav),
    ]);
}

/// Send the keys bound by [`bind_panel_host_keys`] to `panel`. A key the
/// panel has nothing to do with propagates. `chunks_only` is for a read-only
/// transcript: the highlight stays on the transcript's chunks, and Enter only
/// expands or collapses.
pub fn forward_panel_keys<E: InteractiveElement>(
    el: E,
    panel: &Entity<AgentConversationPanel>,
    chunks_only: bool,
) -> E {
    // `on_action` returns `Self`, so the chain keeps the element's type.
    let el = el
        .on_action({
            let panel = panel.clone();
            move |_: &PanelUp, window, cx| {
                if !panel.update(cx, |p, cx| p.handle_key(PanelKey::Up, chunks_only, window, cx)) {
                    cx.propagate();
                }
            }
        })
        .on_action({
            let panel = panel.clone();
            move |_: &PanelDown, window, cx| {
                if !panel.update(cx, |p, cx| p.handle_key(PanelKey::Down, chunks_only, window, cx)) {
                    cx.propagate();
                }
            }
        })
        .on_action({
            let panel = panel.clone();
            move |_: &PanelPageUp, window, cx| {
                if !panel.update(cx, |p, cx| p.handle_key(PanelKey::PageUp, chunks_only, window, cx)) {
                    cx.propagate();
                }
            }
        })
        .on_action({
            let panel = panel.clone();
            move |_: &PanelPageDown, window, cx| {
                if !panel.update(cx, |p, cx| p.handle_key(PanelKey::PageDown, chunks_only, window, cx)) {
                    cx.propagate();
                }
            }
        })
        .on_action({
            let panel = panel.clone();
            move |_: &PanelLeft, window, cx| {
                if !panel.update(cx, |p, cx| p.handle_key(PanelKey::Left, chunks_only, window, cx)) {
                    cx.propagate();
                }
            }
        })
        .on_action({
            let panel = panel.clone();
            move |_: &PanelRight, window, cx| {
                if !panel.update(cx, |p, cx| p.handle_key(PanelKey::Right, chunks_only, window, cx)) {
                    cx.propagate();
                }
            }
        })
        .on_action({
            let panel = panel.clone();
            move |_: &PanelActivate, window, cx| {
                if !panel.update(cx, |p, cx| p.handle_key(PanelKey::Activate, chunks_only, window, cx)) {
                    cx.propagate();
                }
            }
        });
    el
}

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
    /// The host's header button `ix`, beside the title.
    HeaderAction(usize),
    Chunk(ChunkId),
    /// The button on notice `ix` (only notices that have one are stops).
    NoticeAction(usize),
    /// Image `ix` attached to the message being written.
    Image(usize),
    Input,
    /// The host's icon button `ix`, left of Send.
    Tool(usize),
    /// The host's action `ix`, beside Send.
    Action(usize),
    /// Stop the turn in flight (only while one runs).
    Stop,
}

/// A host button: beside Send, on a notice, or in the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelAction {
    /// Reported back in [`AgentConversationEvent::Action`].
    pub id: SharedString,
    pub label: SharedString,
    pub primary: bool,
    pub disabled: bool,
}

impl PanelAction {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            primary: false,
            disabled: false,
        }
    }

}

/// A host icon button in the input's row, left of Send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelTool {
    /// Reported back in [`AgentConversationEvent::Action`].
    pub id: SharedString,
    /// The icon's asset path.
    pub icon: SharedString,
    pub tooltip: SharedString,
    pub disabled: bool,
}

impl PanelTool {
    pub fn new(
        id: impl Into<SharedString>,
        icon: impl IconNamed,
        tooltip: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon: icon.path(),
            tooltip: tooltip.into(),
            disabled: false,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeTone {
    Muted,
    /// Work is under way: shown with a spinner.
    Busy,
    Error,
}

/// A status line above the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelNotice {
    pub text: SharedString,
    pub tone: NoticeTone,
    pub action: Option<PanelAction>,
}

impl PanelNotice {
    pub fn new(tone: NoticeTone, text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            tone,
            action: None,
        }
    }

}

/// A message the user sent: what they wrote and the images they attached.
/// Either may be empty, not both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMessage {
    pub text: String,
    pub images: Vec<PromptImage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentConversationEvent {
    /// Send this message. The host clears the input
    /// ([`AgentConversationPanel::clear_input`]) once it is on its way.
    Send(OutgoingMessage),
    /// Stop the turn in flight.
    Stop,
    /// The user clicked into the panel.
    Activated,
    /// Writing started or stopped.
    EditingChanged(bool),
    /// One of the host's buttons ([`PanelAction::id`]), and whether it was
    /// clicked or activated from the keyboard highlight.
    Action(SharedString, crate::ui::journey::Source),
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
    /// Whether the panel draws its own title bar; a host that has its own
    /// header turns it off.
    header_visible: bool,
    empty_message: SharedString,
    /// Shown after the panel's own hint while not writing.
    extra_hint: Option<SharedString>,
    /// The host's buttons beside Send.
    actions: Vec<PanelAction>,
    /// The host's icon buttons left of Send.
    tools: Vec<PanelTool>,
    /// The host's buttons beside the title.
    header_actions: Vec<PanelAction>,
    /// The host's status lines above the input.
    notices: Vec<PanelNotice>,
    lifecycle_state: Option<String>,
    /// The session's token usage: a line under the title, and every figure
    /// in its tooltip.
    usage: Option<(SharedString, SharedString)>,
    input: Entity<TextareaState>,
    /// Images attached to the message being written.
    images: Vec<PendingImage>,
    /// Pasted images still being prepared; nothing is sent meanwhile.
    preparing: usize,
    /// Why the last pasted image could not be attached.
    image_error: Option<SharedString>,
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
            header_visible: true,
            empty_message: SharedString::default(),
            extra_hint: None,
            actions: Vec::new(),
            tools: Vec::new(),
            header_actions: Vec::new(),
            notices: Vec::new(),
            lifecycle_state: None,
            usage: None,
            input,
            images: Vec::new(),
            preparing: 0,
            image_error: None,
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

    pub fn set_status(&mut self, running: bool, activity: Option<String>, cx: &mut Context<Self>) {
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

    /// The host's icon buttons left of Send, left to right.
    pub fn set_tools(&mut self, tools: Vec<PanelTool>, cx: &mut Context<Self>) {
        if tools != self.tools {
            self.tools = tools;
            self.keep_highlight();
            cx.notify();
        }
    }

    /// Replace the panel's title.
    pub fn set_title(&mut self, title: impl Into<SharedString>, cx: &mut Context<Self>) {
        let title = title.into();
        if title != self.title {
            self.title = title;
            cx.notify();
        }
    }

    /// Hide the panel's title bar (title and header buttons) when the host
    /// draws its own header.
    pub fn set_header_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible != self.header_visible {
            self.header_visible = visible;
            cx.notify();
        }
    }

    /// The session line shown under the title (platform, model, effort,
    /// tokens; see `ui::session_info`): one line, and everything behind it
    /// shown on hover. `None` hides the line.
    pub fn set_usage(&mut self, usage: Option<(String, String)>, cx: &mut Context<Self>) {
        let usage = usage.map(|(line, details)| (line.into(), details.into()));
        if usage != self.usage {
            self.usage = usage;
            cx.notify();
        }
    }

    /// Back to the input when the highlighted stop went away.
    fn keep_highlight(&mut self) {
        if !self.stops().contains(&self.highlight) {
            self.highlight = PanelStop::Input;
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

    #[allow(dead_code)]
    pub fn input(&self) -> &Entity<TextareaState> {
        &self.input
    }

    #[allow(dead_code)]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Empty the message being written: its text and its images.
    pub fn clear_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_input("", window, cx);
        self.images.clear();
        self.image_error = None;
        self.keep_highlight();
        cx.notify();
    }

    // Read by tests.
    #[allow(dead_code)]
    pub fn images(&self) -> Vec<PromptImage> {
        self.images.iter().map(|image| image.prompt.clone()).collect()
    }

    /// Attach the images a paste found, once each is prepared off the UI
    /// thread, in the order they were on the clipboard.
    fn paste_images(&mut self, found: Vec<ClipboardImage>, cx: &mut Context<Self>) {
        self.image_error = None;
        self.preparing += found.len();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let count = found.len();
            let prepared = cx
                .background_executor()
                .spawn(async move {
                    found
                        .into_iter()
                        .map(pasted_image::prepare)
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.preparing = this.preparing.saturating_sub(count);
                for image in prepared {
                    match image {
                        Ok(image) => this.images.push(image),
                        Err(err) => this.image_error = Some(err.into()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_image(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.images.len() {
            self.images.remove(ix);
            self.keep_highlight();
            cx.notify();
        }
    }

    pub fn set_input(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_string();
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    // ----- chunks ------------------------------------------------------------

    pub fn is_expanded(&self, id: ChunkId) -> bool {
        transcript_list::is_expanded(&self.entries, &self.toggled, id, StartState::Reading)
    }

    pub fn toggle(&mut self, id: ChunkId, cx: &mut Context<Self>) {
        let expanded = !self.is_expanded(id);
        if expanded == transcript_list::expanded_by_default(&self.entries, id, StartState::Reading)
        {
            self.toggled.remove(&id);
        } else {
            self.toggled.insert(id, expanded);
        }
        cx.notify();
    }

    /// The panel's stops, top to bottom.
    pub fn stops(&self) -> Vec<PanelStop> {
        let mut stops: Vec<PanelStop> = self
            .header_actions
            .iter()
            .enumerate()
            .filter(|(_, a)| !a.disabled)
            .map(|(ix, _)| PanelStop::HeaderAction(ix))
            .collect();
        stops.extend(
            transcript_list::chunks(&self.entries, &self.toggled, StartState::Reading)
                .into_iter()
                .map(PanelStop::Chunk),
        );
        stops.extend(
            self.notices
                .iter()
                .enumerate()
                .filter(|(_, n)| n.action.as_ref().is_some_and(|a| !a.disabled))
                .map(|(ix, _)| PanelStop::NoticeAction(ix)),
        );
        stops.extend((0..self.images.len()).map(PanelStop::Image));
        stops.push(PanelStop::Input);
        stops.extend(
            self.tools
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.disabled)
                .map(|(ix, _)| PanelStop::Tool(ix)),
        );
        stops.extend(
            self.actions
                .iter()
                .enumerate()
                .filter(|(_, a)| !a.disabled)
                .map(|(ix, _)| PanelStop::Action(ix)),
        );
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

    /// A navigation key forwarded by the host that owns focus. False when the
    /// panel has nothing to do with it, so the host can let it propagate.
    pub fn handle_key(
        &mut self,
        key: PanelKey,
        chunks_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.editing {
            return false;
        }
        let on_chunk = matches!(self.highlight, PanelStop::Chunk(_));
        match key {
            PanelKey::Up | PanelKey::Down => {
                let delta = if key == PanelKey::Up { -1 } else { 1 };
                let stops = self.stops();
                if chunks_only {
                    let chunks: Vec<PanelStop> = stops
                        .into_iter()
                        .filter(|s| matches!(s, PanelStop::Chunk(_)))
                        .collect();
                    let target = match chunks.iter().position(|s| *s == self.highlight) {
                        Some(ix) => ix.checked_add_signed(delta).and_then(|ix| chunks.get(ix)),
                        None if delta < 0 => chunks.last(),
                        None => chunks.first(),
                    };
                    match target.copied() {
                        Some(stop) => {
                            self.set_highlight(stop, cx);
                            true
                        }
                        None => false,
                    }
                } else {
                    self.move_highlight(delta, cx)
                }
            }
            PanelKey::PageUp | PanelKey::PageDown => {
                self.page(key == PanelKey::PageDown, cx);
                true
            }
            PanelKey::Left => self.collapse_highlight(cx),
            PanelKey::Right => self.expand_highlight(cx),
            PanelKey::Activate => {
                if chunks_only && !on_chunk {
                    return false;
                }
                self.activate(window, cx);
                true
            }
        }
    }

    /// Page Up / Page Down: scroll the transcript one screen, and carry a
    /// highlighted chunk along so the selection stays on screen.
    pub fn page(&mut self, down: bool, cx: &mut Context<Self>) {
        let from = match self.highlight {
            PanelStop::Chunk(id) => Some(id),
            _ => None,
        };
        let moved = self.list.update(cx, |list, cx| list.page(down, from, cx));
        if let Some(id) = moved {
            self.set_highlight(PanelStop::Chunk(id), cx);
        }
    }

    /// Left on a chunk: collapse it, or when it is already collapsed (or has
    /// nothing to collapse) go to the reply it is a piece of. False, having
    /// done nothing, when the highlight is not on a chunk that can.
    pub fn collapse_highlight(&mut self, cx: &mut Context<Self>) -> bool {
        let PanelStop::Chunk(id) = self.highlight else {
            return false;
        };
        if self.is_expanded(id) {
            self.toggle(id, cx);
            return true;
        }
        match id.part {
            Some(_) => {
                self.set_highlight(PanelStop::Chunk(ChunkId { entry: id.entry, part: None }), cx);
                true
            }
            None => false,
        }
    }

    /// Right on a chunk: expand it. False when it is not a collapsed chunk.
    pub fn expand_highlight(&mut self, cx: &mut Context<Self>) -> bool {
        match self.highlight {
            PanelStop::Chunk(id) if !self.is_expanded(id) => {
                self.toggle(id, cx);
                true
            }
            _ => false,
        }
    }

    pub fn set_highlight(&mut self, stop: PanelStop, cx: &mut Context<Self>) {
        self.highlight = stop;
        cx.notify();
    }

    /// Enter on the highlight: expand or collapse a chunk, start writing,
    /// press a host button, or stop the turn.
    pub fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.highlight {
            PanelStop::Chunk(id) => self.toggle(id, cx),
            PanelStop::Input => self.start_editing(window, cx),
            PanelStop::Image(ix) => self.remove_image(ix, cx),
            PanelStop::Action(ix) => {
                if let Some(action) = self.actions.get(ix) {
                    cx.emit(AgentConversationEvent::Action(
                        action.id.clone(),
                        crate::ui::journey::Source::Keyboard,
                    ));
                }
            }
            PanelStop::Tool(ix) => {
                if let Some(tool) = self.tools.get(ix) {
                    cx.emit(AgentConversationEvent::Action(
                        tool.id.clone(),
                        crate::ui::journey::Source::Keyboard,
                    ));
                }
            }
            PanelStop::NoticeAction(ix) => {
                if let Some(action) = self.notices.get(ix).and_then(|n| n.action.as_ref()) {
                    cx.emit(AgentConversationEvent::Action(
                        action.id.clone(),
                        crate::ui::journey::Source::Keyboard,
                    ));
                }
            }
            PanelStop::HeaderAction(ix) => {
                if let Some(action) = self.header_actions.get(ix) {
                    cx.emit(AgentConversationEvent::Action(
                        action.id.clone(),
                        crate::ui::journey::Source::Keyboard,
                    ));
                }
            }
            PanelStop::Stop => cx.emit(AgentConversationEvent::Stop),
        }
    }

    /// A host button, highlighted when the keyboard is on `stop`.
    fn action_button(
        &self,
        element_id: impl Into<gpui::ElementId>,
        action: &PanelAction,
        stop: PanelStop,
        cx: &mut Context<Self>,
    ) -> Button {
        let id = action.id.clone();
        let button = Button::new(element_id)
            .label(action.label.clone())
            .small()
            .disabled(action.disabled)
            .selected(self.active && self.highlight == stop)
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(AgentConversationEvent::Activated);
                cx.emit(AgentConversationEvent::Action(
                    id.clone(),
                    crate::ui::journey::Source::Click,
                ));
            }));
        if action.primary {
            button.primary()
        } else {
            button.ghost()
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

    /// Ask the host to send what is written, with the images attached to
    /// it. Waits for a pasted image still being prepared.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if self.preparing > 0 {
            return;
        }
        let text = self.input.read(cx).value().trim().to_string();
        if !text.is_empty() || !self.images.is_empty() {
            cx.emit(AgentConversationEvent::Send(OutgoingMessage {
                text,
                images: self.images(),
            }));
        }
    }

    /// The images attached to the message being written, above the input,
    /// and a line while one is being prepared or could not be.
    fn render_images(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.images.is_empty() && self.preparing == 0 && self.image_error.is_none() {
            return None;
        }
        let thumbnails = self.images.iter().enumerate().map(|(ix, image)| {
            let highlighted = self.active && self.highlight == PanelStop::Image(ix);
            style::image_thumbnail(div())
                .id(("agent-conversation-image", ix))
                .relative()
                .when(highlighted, style::highlighted)
                .tooltip(|window, cx| {
                    Tooltip::new("Attached image · Enter or × takes it off").build(window, cx)
                })
                .child(
                    img(image.preview.clone())
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                )
                .child(
                    div().absolute().top_0().right_0().child(
                        Button::new(("agent-conversation-image-remove", ix))
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_image(ix, cx);
                                cx.emit(AgentConversationEvent::Activated);
                            })),
                    ),
                )
        });
        let status = if self.preparing > 0 {
            Some(
                h_flex()
                    .gap(style::space::RELATED)
                    .child(Spinner::new().small())
                    .child(style::text_dense_muted(div()).child("Attaching the image…"))
                    .into_any_element(),
            )
        } else {
            self.image_error.clone().map(|err| {
                style::text_error(div())
                    .child(err)
                    .into_any_element()
            })
        };
        Some(
            v_flex()
                .gap(style::space::INLINE)
                .pb(style::space::INLINE)
                .when(!self.images.is_empty(), |el| {
                    el.child(
                        h_flex()
                            .flex_wrap()
                            .gap(style::space::RELATED)
                            .children(thumbnails),
                    )
                })
                .children(status)
                .into_any_element(),
        )
    }
}

impl Render for AgentConversationPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            "Ctrl+Enter sends · Ctrl+V attaches an image · Esc stops writing".into()
        } else {
            match &self.extra_hint {
                Some(extra) => format!("Enter to write or expand · {extra}").into(),
                None => "Enter to write or expand".into(),
            }
        };
        let stop_highlighted = self.active && self.highlight == PanelStop::Stop;

        let mut notices = v_flex()
            .id("agent-conversation-notices")
            .max_h(px(NOTICES_MAX_HEIGHT))
            .overflow_y_scroll()
            .gap(style::space::INLINE);
        for (ix, notice) in self.notices.clone().into_iter().enumerate() {
            let text = selectable_text(
                SharedString::from(format!("agent-conversation-notice-{ix}")),
                notice.text.clone(),
                window,
                cx,
            );
            let text = match notice.tone {
                NoticeTone::Error => style::text_error(div()),
                NoticeTone::Muted | NoticeTone::Busy => style::text_dense_muted(div()),
            }
            .flex_1()
            .min_w_0()
            .child(text);
            let button = notice.action.as_ref().map(|action| {
                self.action_button(
                    ("agent-conversation-notice-action", ix),
                    action,
                    PanelStop::NoticeAction(ix),
                    cx,
                )
            });
            notices = notices.child(
                h_flex()
                    .items_center()
                    .gap(style::space::RELATED)
                    .when(notice.tone == NoticeTone::Busy, |el| {
                        el.child(Spinner::new().small())
                    })
                    .child(text)
                    .children(button),
            );
        }
        let header_actions: Vec<Button> = self
            .header_actions
            .clone()
            .iter()
            .enumerate()
            .map(|(ix, action)| {
                self.action_button(
                    ("agent-conversation-header-action", ix),
                    action,
                    PanelStop::HeaderAction(ix),
                    cx,
                )
            })
            .collect();
        let actions: Vec<Button> = self
            .actions
            .clone()
            .iter()
            .enumerate()
            .map(|(ix, action)| {
                self.action_button(
                    ("agent-conversation-action", ix),
                    action,
                    PanelStop::Action(ix),
                    cx,
                )
            })
            .collect();

        let tools: Vec<Button> = self
            .tools
            .clone()
            .into_iter()
            .enumerate()
            .map(|(ix, tool)| {
                let id = tool.id.clone();
                Button::new(("agent-conversation-tool", ix))
                    .icon(Icon::default().path(tool.icon))
                    .ghost()
                    .small()
                    .disabled(tool.disabled)
                    .selected(self.active && self.highlight == PanelStop::Tool(ix))
                    .tooltip(tool.tooltip.clone())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(AgentConversationEvent::Activated);
                        cx.emit(AgentConversationEvent::Action(
                            id.clone(),
                            crate::ui::journey::Source::Click,
                        ));
                    }))
            })
            .collect();

        let has_lifecycle = !self.notices.is_empty() || !self.actions.is_empty();

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
            // Ahead of the input's own paste: an image on the clipboard is
            // attached to the message, and anything else pastes as text.
            .capture_action(cx.listener(|this, _: &Paste, _, cx| {
                if !this.editing {
                    return;
                }
                let found = cx
                    .read_from_clipboard()
                    .map(|item| pasted_image::clipboard_images(&item))
                    .unwrap_or_default();
                if !found.is_empty() {
                    cx.stop_propagation();
                    this.paste_images(found, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &AgentConversationEscape, window, cx| {
                    if this.editing {
                        this.stop_editing(window, cx);
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .when(self.header_visible, |el| {
                el.child(
                    style::panel_header(h_flex())
                        .items_center()
                        .child(
                            if self.active {
                                style::text_title(div())
                            } else {
                                style::text_muted(div())
                            }
                            .flex_1()
                            .min_w_0()
                            .child(self.title.clone()),
                        )
                        .children(header_actions),
                )
            })
            .children(self.usage.clone().map(|(line, details)| {
                div()
                    .id("agent-conversation-usage")
                    .px(style::space::INSET)
                    .py(style::space::HAIRLINE)
                    .border_b(style::size::BORDER)
                    .border_color(style::color::divider())
                    .tooltip(move |window, cx| Tooltip::new(details.clone()).build(window, cx))
                    .child(style::text_dense_muted(div()).child(selectable_text(
                        "agent-conversation-usage-text",
                        line,
                        window,
                        cx,
                    )))
            }))
            .child(div().flex_1().min_h_0().child(self.list.clone()))
            .child(
                style::panel_footer(v_flex())
                    .children(self.render_images(cx))
                    .child(field)
                    .child(
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
                        .children(tools)
                        .child(
                            Button::new("agent-conversation-send")
                                .label("Send")
                                .primary()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                        ),
                ),
            )
            .when(has_lifecycle, |el| {
                el.child(
                    style::panel_footer(v_flex())
                        .child(
                            style::text_dense_muted(div()).child(match &self.lifecycle_state {
                                Some(state) => format!("Lifecycle: {state}"),
                                None => "Lifecycle".to_string(),
                            }),
                        )
                        .when(!self.notices.is_empty(), |el| el.child(notices))
                        .when(!actions.is_empty(), |el| {
                            el.child(
                                h_flex()
                                    .items_center()
                                    .justify_end()
                                    .gap(style::space::RELATED)
                                    .children(actions),
                            )
                        }),
                )
            })
    }
}
