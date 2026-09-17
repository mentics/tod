//! A general-purpose agent conversation panel: a transcript of collapsible
//! chunks above a message input.
//!
//! The panel knows nothing about where the conversation is stored or which
//! agent runs it. Its host hands it [`Entry`]s ([`AgentConversationPanel::set_entries`])
//! and the run status, and hears back through [`AgentConversationEvent`].
//!
//! Every message is a chunk that expands and collapses. An agent reply is a
//! chunk whose pieces — narration, thoughts, tool calls, and the answer — are
//! chunks of their own; everything except the answer starts collapsed, so a
//! reply reads as its answer with one line per step of work above it.
//!
//! Keyboard: the host owns focus and moves the panel's highlight with
//! [`AgentConversationPanel::move_highlight`] and
//! [`AgentConversationPanel::activate`]. While the input is being written in,
//! the panel's own bindings (Ctrl+Enter sends, Escape stops writing) apply,
//! and focus returns to the handle given to
//! [`AgentConversationPanel::set_return_focus`].

use crate::ui::key_context;
use crate::ui::key_context::set_input_tab_stop;
use crate::ui::selectable_text::{selectable_markdown, selectable_text};
use crate::ui::style;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EventEmitter, FocusHandle,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement, Styled, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::scroll::Scrollbar;
use gpui_component::{Icon, Selectable, Sizable, h_flex, v_flex};
use gpui_kit_assets::IconName;
use std::collections::HashMap;
use tod_agent::ReplyPart;

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

/// Who a transcript entry is from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    User,
    Agent,
    Error,
    /// A one-line note between entries (e.g. a fresh agent session).
    Marker,
}

/// One transcript entry, as the host has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    /// The message. For an agent entry with [`Self::parts`], its answer.
    pub body: String,
    /// An agent reply as it was streamed; empty when there are none.
    pub parts: Vec<ReplyPart>,
}

/// A chunk: an entry, or one piece of an agent entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkId {
    pub entry: usize,
    pub part: Option<usize>,
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

/// How a piece of an agent entry is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PieceKind {
    /// Text before the last thought or tool call: the agent narrating.
    Narration,
    Thought,
    Tool,
    /// Text after the last thought or tool call.
    Answer,
}

impl PieceKind {
    fn expanded_by_default(self) -> bool {
        self == PieceKind::Answer
    }

    fn label(self) -> &'static str {
        match self {
            PieceKind::Narration => "Note",
            PieceKind::Thought => "Thinking",
            PieceKind::Tool => "Tool",
            PieceKind::Answer => "Reply",
        }
    }
}

/// The pieces of an agent entry: its parts, or its body as one answer.
fn pieces(entry: &Entry) -> Vec<(PieceKind, &ReplyPart)> {
    let answer_from = entry
        .parts
        .iter()
        .rposition(|p| !matches!(p, ReplyPart::Text { .. }))
        .map_or(0, |ix| ix + 1);
    entry
        .parts
        .iter()
        .enumerate()
        .filter(|(_, part)| !matches!(part, ReplyPart::Text { text } if text.trim().is_empty()))
        .map(|(ix, part)| {
            let kind = match part {
                ReplyPart::Text { .. } if ix >= answer_from => PieceKind::Answer,
                ReplyPart::Text { .. } => PieceKind::Narration,
                ReplyPart::Thought { .. } => PieceKind::Thought,
                ReplyPart::Tool { .. } => PieceKind::Tool,
            };
            (kind, part)
        })
        .collect()
}

/// `text` as one line, for a collapsed chunk.
fn first_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn tool_line(title: &str, status: &str) -> String {
    let title = if title.is_empty() { "Tool call" } else { title };
    if status.is_empty() {
        title.to_string()
    } else {
        format!("{title} · {status}")
    }
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
    scroll: ScrollHandle,
    /// Entries shown last render; more means scroll to the newest.
    rendered_entries: usize,
    /// Scroll the highlighted chunk into view on the next render.
    scroll_to_highlight: bool,
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
            scroll: ScrollHandle::new(),
            rendered_entries: 0,
            scroll_to_highlight: false,
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
        self.rendered_entries = 0;
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
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    // ----- chunks ------------------------------------------------------------

    fn default_expanded(&self, id: ChunkId) -> bool {
        let Some(entry) = self.entries.get(id.entry) else {
            return false;
        };
        match id.part {
            None => true,
            Some(ix) => pieces(entry)
                .get(ix)
                .is_some_and(|(kind, _)| kind.expanded_by_default()),
        }
    }

    pub fn is_expanded(&self, id: ChunkId) -> bool {
        self.toggled
            .get(&id)
            .copied()
            .unwrap_or_else(|| self.default_expanded(id))
    }

    pub fn toggle(&mut self, id: ChunkId, cx: &mut Context<Self>) {
        let expanded = !self.is_expanded(id);
        if expanded == self.default_expanded(id) {
            self.toggled.remove(&id);
        } else {
            self.toggled.insert(id, expanded);
        }
        cx.notify();
    }

    /// The chunks in display order: each entry, then (while it is expanded)
    /// its pieces. Marker entries are not chunks.
    fn chunks(&self) -> Vec<ChunkId> {
        let mut chunks = Vec::new();
        for (entry_ix, entry) in self.entries.iter().enumerate() {
            if entry.kind == EntryKind::Marker {
                continue;
            }
            let id = ChunkId {
                entry: entry_ix,
                part: None,
            };
            chunks.push(id);
            if entry.kind == EntryKind::Agent && self.is_expanded(id) {
                chunks.extend((0..pieces(entry).len()).map(|part| ChunkId {
                    entry: entry_ix,
                    part: Some(part),
                }));
            }
        }
        chunks
    }

    /// The panel's stops, top to bottom.
    pub fn stops(&self) -> Vec<PanelStop> {
        let mut stops: Vec<PanelStop> = self.chunks().into_iter().map(PanelStop::Chunk).collect();
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
        self.scroll_to_highlight = true;
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

    // ----- rendering -------------------------------------------------------------

    fn chunk_header(
        &self,
        id: ChunkId,
        icon: Option<IconName>,
        label: &'static str,
        summary: Option<String>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let expanded = self.is_expanded(id);
        let highlighted = self.active && self.highlight == PanelStop::Chunk(id);
        style::chunk_header(h_flex())
            .id(ElementId::Name(
                format!("chunk-{}-{}", id.entry, id.part.map_or(-1, |p| p as i64)).into(),
            ))
            .w_full()
            .when(highlighted, style::highlighted)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.highlight = PanelStop::Chunk(id);
                this.toggle(id, cx);
                cx.emit(AgentConversationEvent::Activated);
            }))
            .child(
                style::text_muted(div())
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .child(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall(),
                    ),
            )
            .when_some(icon, |el, icon| {
                el.child(
                    style::text_muted(div())
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .child(Icon::new(icon).xsmall()),
                )
            })
            .child(style::chunk_label(div()).flex_shrink_0().child(label))
            .when_some(summary.filter(|_| !expanded), |el, summary| {
                el.child(
                    style::text_dense_muted(div())
                        .flex_1()
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .overflow_hidden()
                        .child(summary),
                )
            })
    }

    fn render_entry(
        &self,
        entry_ix: usize,
        entry: &Entry,
        rows: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = ChunkId {
            entry: entry_ix,
            part: None,
        };
        let key = |name: &str| ElementId::Name(format!("{name}-{entry_ix}").into());
        match entry.kind {
            EntryKind::Marker => {
                rows.push(
                    h_flex()
                        .items_center()
                        .gap(style::space::RELATED)
                        .child(div().flex_1().h(style::size::BORDER).bg(style::color::divider()))
                        .child(style::text_dense_muted(div()).child(entry.body.clone()))
                        .child(div().flex_1().h(style::size::BORDER).bg(style::color::divider()))
                        .into_any_element(),
                );
            }
            EntryKind::User | EntryKind::Error => {
                let (label, summary) = if entry.kind == EntryKind::User {
                    ("You", first_line(&entry.body))
                } else {
                    ("Error", first_line(&entry.body))
                };
                let mut chunk = style::chunk(v_flex())
                    .when(entry.kind == EntryKind::User, |el| {
                        el.bg(style::color::badge_fill())
                    })
                    .child(self.chunk_header(id, None, label, Some(summary), cx));
                if self.is_expanded(id) {
                    let text = selectable_text(key("entry-body"), entry.body.clone(), window, cx)
                        .w_full();
                    chunk = chunk.child(style::chunk_body(div()).child(
                        if entry.kind == EntryKind::Error {
                            style::text_error(text).into_any_element()
                        } else {
                            style::text(text).into_any_element()
                        },
                    ));
                }
                rows.push(chunk.into_any_element());
            }
            EntryKind::Agent => {
                let pieces = pieces(entry);
                let answer = if entry.body.trim().is_empty() {
                    "Done, no notes".to_string()
                } else {
                    first_line(&entry.body)
                };
                let work = pieces
                    .iter()
                    .filter(|(kind, _)| *kind != PieceKind::Answer)
                    .count();
                let summary = match work {
                    0 => answer,
                    1 => format!("1 step · {answer}"),
                    n => format!("{n} steps · {answer}"),
                };
                let expanded = self.is_expanded(id);
                rows.push(
                    style::chunk(v_flex())
                        .child(self.chunk_header(id, None, "Agent", Some(summary), cx))
                        .into_any_element(),
                );
                if !expanded {
                    return;
                }
                if pieces.is_empty() {
                    rows.push(
                        style::chunk_children(div())
                            .child(if entry.body.trim().is_empty() {
                                style::text_muted(div())
                                    .child("Done, no notes")
                                    .into_any_element()
                            } else {
                                style::text(
                                    selectable_markdown(key("entry-body"), entry.body.clone(), window, cx)
                                        .w_full(),
                                )
                                .into_any_element()
                            })
                            .into_any_element(),
                    );
                    return;
                }
                for (part_ix, (kind, part)) in pieces.iter().enumerate() {
                    let part_id = ChunkId {
                        entry: entry_ix,
                        part: Some(part_ix),
                    };
                    let (icon, summary) = match part {
                        ReplyPart::Text { text } | ReplyPart::Thought { text } => (
                            (*kind == PieceKind::Thought).then_some(IconName::Brain),
                            first_line(text),
                        ),
                        ReplyPart::Tool { title, status, .. } => {
                            (Some(IconName::Wrench), tool_line(title, status))
                        }
                    };
                    let mut chunk = style::chunk(v_flex())
                        .child(self.chunk_header(part_id, icon, kind.label(), Some(summary), cx));
                    if self.is_expanded(part_id) {
                        let body_id = ElementId::Name(format!("part-{entry_ix}-{part_ix}").into());
                        let body = match part {
                            ReplyPart::Text { text } if *kind == PieceKind::Answer => style::text(
                                selectable_markdown(body_id, text.trim().to_string(), window, cx)
                                    .w_full(),
                            )
                            .into_any_element(),
                            ReplyPart::Text { text } | ReplyPart::Thought { text } => {
                                style::text_muted(
                                    selectable_markdown(body_id, text.trim().to_string(), window, cx)
                                        .w_full(),
                                )
                                .into_any_element()
                            }
                            ReplyPart::Tool { title, status, .. } => style::text_muted(
                                selectable_text(body_id, tool_line(title, status), window, cx)
                                    .w_full(),
                            )
                            .into_any_element(),
                        };
                        chunk = chunk.child(style::chunk_body(div()).child(body));
                    }
                    rows.push(
                        style::chunk_children(div())
                            .child(chunk)
                            .into_any_element(),
                    );
                }
                if entry.body.trim().is_empty()
                    && !pieces.iter().any(|(kind, _)| *kind == PieceKind::Answer)
                {
                    rows.push(
                        style::chunk_children(div())
                            .child(style::text_muted(div()).child("Done, no notes"))
                            .into_any_element(),
                    );
                }
            }
        }
    }
}

impl Render for AgentConversationPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        set_input_tab_stop(&self.input, self.editing, cx);
        if self.entries.len() != self.rendered_entries {
            self.rendered_entries = self.entries.len();
            self.scroll.scroll_to_bottom();
        }

        // Every chunk is a direct child of the list, so the highlighted one
        // can be scrolled to by index.
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut highlight_row = None;
        for (entry_ix, entry) in self.entries.iter().enumerate() {
            let start = rows.len();
            self.render_entry(entry_ix, entry, &mut rows, window, cx);
            if let PanelStop::Chunk(id) = self.highlight
                && id.entry == entry_ix
            {
                let offset = match id.part {
                    None => 0,
                    Some(part) => part + 1,
                };
                highlight_row = Some((start + offset).min(rows.len().saturating_sub(1)));
            }
        }
        if self.running {
            let label = match &self.activity {
                Some(activity) => format!("Working… {activity}"),
                None => "Working…".to_string(),
            };
            rows.push(
                style::text_dense_muted(div())
                    .px(style::space::RELATED)
                    .child(label)
                    .into_any_element(),
            );
        }
        if rows.is_empty() && !self.empty_message.is_empty() {
            rows.push(
                style::empty_message(div())
                    .p(style::space::INSET)
                    .child(selectable_text(
                        "agent-conversation-empty",
                        self.empty_message.clone(),
                        window,
                        cx,
                    ))
                    .into_any_element(),
            );
        }
        if std::mem::take(&mut self.scroll_to_highlight) {
            match (self.highlight, highlight_row) {
                (PanelStop::Chunk(_), Some(row)) => self.scroll.scroll_to_item(row),
                (PanelStop::Chunk(_), None) => {}
                _ => self.scroll.scroll_to_bottom(),
            }
        }

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
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        v_flex()
                            .id("agent-conversation-list")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .p(style::space::INSET)
                            .gap(style::space::INLINE)
                            // Chunks keep their natural height; the list scrolls instead.
                            .children(
                                rows.into_iter()
                                    .map(|row| div().w_full().flex_shrink_0().child(row)),
                            ),
                    )
                    .child(
                        div()
                            .occlude()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.))
                            .child(Scrollbar::vertical(&self.scroll)),
                    ),
            )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(parts: Vec<ReplyPart>, body: &str) -> Entry {
        Entry {
            kind: EntryKind::Agent,
            body: body.into(),
            parts,
        }
    }

    fn text(text: &str) -> ReplyPart {
        ReplyPart::Text { text: text.into() }
    }

    #[test]
    fn narration_before_the_work_is_not_the_answer() {
        let entry = agent(
            vec![
                text("Let me look."),
                ReplyPart::Thought {
                    text: "Hmm".into(),
                },
                ReplyPart::Tool {
                    id: "1".into(),
                    title: "Read".into(),
                    status: "completed".into(),
                },
                text("  "),
                text("Done."),
            ],
            "Done.",
        );
        let kinds: Vec<PieceKind> = pieces(&entry).into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            kinds,
            [
                PieceKind::Narration,
                PieceKind::Thought,
                PieceKind::Tool,
                PieceKind::Answer
            ]
        );
        assert!(
            kinds
                .iter()
                .all(|k| k.expanded_by_default() == (*k == PieceKind::Answer))
        );
    }

    #[test]
    fn a_reply_without_work_is_all_answer() {
        let entry = agent(vec![text("One."), text("Two.")], "One.\n\nTwo.");
        assert!(
            pieces(&entry)
                .iter()
                .all(|(kind, _)| *kind == PieceKind::Answer)
        );
    }
}
