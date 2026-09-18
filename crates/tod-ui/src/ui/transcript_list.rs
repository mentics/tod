//! A transcript of collapsible chunks: the scrolling list shared by every
//! surface that shows an agent transcript.
//!
//! The list knows nothing about where the transcript came from or what sits
//! around it. Its host hands it [`Entry`]s ([`TranscriptList::set_entries`])
//! and the run status, and hears back through [`TranscriptListEvent`].
//!
//! Every entry is a chunk that expands and collapses. An agent reply is a
//! chunk whose pieces — narration, thoughts, tool calls, and the answer — are
//! chunks of their own; everything except the answer starts collapsed, so a
//! reply reads as its answer with one line per step of work above it.
//!
//! Expansion state and the highlight belong to the **host**, not the list: a
//! host that gives the transcript keyboard stops (see
//! `crate::ui::agent_conversation::AgentConversationPanel`) needs them in the
//! same model as its other stops. The list resolves what is expanded through
//! [`is_expanded`], which hosts share, and reports clicks rather than acting
//! on them.

use crate::ui::selectable_text::{selectable_markdown, selectable_text};
use crate::ui::style;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Context, ElementId, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::scroll::Scrollbar;
use gpui_component::{Icon, Sizable, h_flex, v_flex};
use gpui_kit_assets::IconName;
use std::collections::HashMap;
use tod_agent::ReplyPart;

/// Who a transcript entry is from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    User,
    Agent,
    Error,
    /// A one-line note between entries (e.g. a fresh agent session).
    Marker,
    /// An uninterpreted payload, shown verbatim under the entry's own
    /// [`Entry::label`] — the raw agent traffic the transcripts window logs.
    /// `outgoing` is what distinguishes a request from a response.
    Raw { outgoing: bool },
}

/// One transcript entry, as the host has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    /// The message. For an agent entry with [`Self::parts`], its answer.
    pub body: String,
    /// An agent reply as it was streamed; empty when there are none.
    pub parts: Vec<ReplyPart>,
    /// Header label, when the kind's own label ("You", "Agent", …) is not
    /// what this entry should say. Required by [`EntryKind::Raw`].
    pub label: Option<SharedString>,
}

impl Entry {
    /// A raw payload shown verbatim under `label`.
    pub fn raw(outgoing: bool, label: impl Into<SharedString>, body: impl Into<String>) -> Self {
        Self {
            kind: EntryKind::Raw { outgoing },
            body: body.into(),
            parts: Vec::new(),
            label: Some(label.into()),
        }
    }
}

/// A chunk: an entry, or one piece of an agent entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkId {
    pub entry: usize,
    pub part: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptListEvent {
    /// The user clicked a chunk's header. The host decides what that means —
    /// typically move its highlight there and toggle the chunk.
    ChunkClicked(ChunkId),
}

/// How a piece of an agent entry is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceKind {
    /// Text before the last thought or tool call: the agent narrating.
    Narration,
    Thought,
    Tool,
    /// Text after the last thought or tool call.
    Answer,
}

impl PieceKind {
    pub fn expanded_by_default(self) -> bool {
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
pub fn pieces(entry: &Entry) -> Vec<(PieceKind, &ReplyPart)> {
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

/// Whether `id` shows expanded by default: entries do, and so does the
/// answer of a reply — the work above it starts collapsed.
pub fn expanded_by_default(entries: &[Entry], id: ChunkId) -> bool {
    let Some(entry) = entries.get(id.entry) else {
        return false;
    };
    match id.part {
        None => true,
        Some(ix) => pieces(entry)
            .get(ix)
            .is_some_and(|(kind, _)| kind.expanded_by_default()),
    }
}

/// Whether `id` is expanded, given the chunks the host's user has toggled
/// away from their default. Shared so a host and its list agree.
pub fn is_expanded(entries: &[Entry], toggled: &HashMap<ChunkId, bool>, id: ChunkId) -> bool {
    toggled
        .get(&id)
        .copied()
        .unwrap_or_else(|| expanded_by_default(entries, id))
}

/// The chunks of `entries` in display order: each entry, then (while it is
/// expanded) its pieces. Marker entries are not chunks.
pub fn chunks(entries: &[Entry], toggled: &HashMap<ChunkId, bool>) -> Vec<ChunkId> {
    let mut chunks = Vec::new();
    for (entry_ix, entry) in entries.iter().enumerate() {
        if entry.kind == EntryKind::Marker {
            continue;
        }
        let id = ChunkId {
            entry: entry_ix,
            part: None,
        };
        chunks.push(id);
        if entry.kind == EntryKind::Agent && is_expanded(entries, toggled, id) {
            chunks.extend((0..pieces(entry).len()).map(|part| ChunkId {
                entry: entry_ix,
                part: Some(part),
            }));
        }
    }
    chunks
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

pub struct TranscriptList {
    entries: Vec<Entry>,
    /// Chunks the host's user expanded or collapsed; the rest show their
    /// default. Owned by the host and pushed down — see the module docs.
    toggled: HashMap<ChunkId, bool>,
    /// The chunk the host's highlight is on, when it is in this list.
    highlight: Option<ChunkId>,
    /// Whether the host's keyboard is on the list, so its highlight shows.
    active: bool,
    running: bool,
    activity: Option<SharedString>,
    empty_message: SharedString,
    scroll: ScrollHandle,
    /// Entries shown last render; more means scroll to the newest.
    rendered_entries: usize,
    /// Scroll the highlighted chunk into view on the next render.
    scroll_to_highlight: bool,
}

impl EventEmitter<TranscriptListEvent> for TranscriptList {}

impl Default for TranscriptList {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptList {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            toggled: HashMap::new(),
            highlight: None,
            active: false,
            running: false,
            activity: None,
            empty_message: SharedString::default(),
            scroll: ScrollHandle::new(),
            rendered_entries: 0,
            scroll_to_highlight: false,
        }
    }

    pub fn set_entries(&mut self, entries: Vec<Entry>, cx: &mut Context<Self>) {
        if entries != self.entries {
            self.entries = entries;
            cx.notify();
        }
    }

    /// Mirror the host's expansion state.
    pub fn set_toggled(&mut self, toggled: HashMap<ChunkId, bool>, cx: &mut Context<Self>) {
        if toggled != self.toggled {
            self.toggled = toggled;
            cx.notify();
        }
    }

    /// Put the highlight on `chunk`, and scroll it into view when it moved.
    pub fn set_highlight(&mut self, chunk: Option<ChunkId>, cx: &mut Context<Self>) {
        if chunk != self.highlight {
            self.highlight = chunk;
            self.scroll_to_highlight = true;
            cx.notify();
        }
    }

    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if active != self.active {
            self.active = active;
            cx.notify();
        }
    }

    pub fn set_status(&mut self, running: bool, activity: Option<String>, cx: &mut Context<Self>) {
        let activity = activity.map(SharedString::from);
        if running != self.running || activity != self.activity {
            self.running = running;
            self.activity = activity;
            cx.notify();
        }
    }

    pub fn set_empty_message(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        let message = message.into();
        if message != self.empty_message {
            self.empty_message = message;
            cx.notify();
        }
    }

    /// Forget the scroll position and what was shown: a different transcript
    /// is about to be.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.entries.clear();
        self.toggled.clear();
        self.highlight = None;
        self.rendered_entries = 0;
        cx.notify();
    }

    #[allow(dead_code)]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn is_expanded(&self, id: ChunkId) -> bool {
        is_expanded(&self.entries, &self.toggled, id)
    }

    fn chunk_header(
        &self,
        id: ChunkId,
        icon: Option<IconName>,
        label: SharedString,
        summary: Option<String>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let expanded = self.is_expanded(id);
        let highlighted = self.active && self.highlight == Some(id);
        style::chunk_header(h_flex())
            .id(ElementId::Name(
                format!("chunk-{}-{}", id.entry, id.part.map_or(-1, |p| p as i64)).into(),
            ))
            .w_full()
            .when(highlighted, style::highlighted)
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(TranscriptListEvent::ChunkClicked(id));
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
                        .child(
                            div()
                                .flex_1()
                                .h(style::size::BORDER)
                                .bg(style::color::divider()),
                        )
                        .child(style::text_dense_muted(div()).child(entry.body.clone()))
                        .child(
                            div()
                                .flex_1()
                                .h(style::size::BORDER)
                                .bg(style::color::divider()),
                        )
                        .into_any_element(),
                );
            }
            EntryKind::User | EntryKind::Error | EntryKind::Raw { .. } => {
                let default_label = match entry.kind {
                    EntryKind::Error => "Error",
                    _ => "You",
                };
                let label = entry
                    .label
                    .clone()
                    .unwrap_or_else(|| SharedString::from(default_label));
                // A request carries what we sent, so it gets the same tint a
                // user message does.
                let tinted = matches!(
                    entry.kind,
                    EntryKind::User | EntryKind::Raw { outgoing: true }
                );
                let mut chunk = style::chunk(v_flex())
                    .when(tinted, |el| el.bg(style::color::badge_fill()))
                    .child(self.chunk_header(id, None, label, Some(first_line(&entry.body)), cx));
                if self.is_expanded(id) {
                    // Shown verbatim: a user message may carry markdown, but
                    // so may a raw payload contain text that must not be
                    // reflowed, and neither is the agent's own prose.
                    let text =
                        selectable_text(key("entry-body"), entry.body.clone(), window, cx).w_full();
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
                let label = entry
                    .label
                    .clone()
                    .unwrap_or_else(|| SharedString::from("Agent"));
                rows.push(
                    style::chunk(v_flex())
                        .child(self.chunk_header(id, None, label, Some(summary), cx))
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
                                    selectable_markdown(
                                        key("entry-body"),
                                        entry.body.clone(),
                                        window,
                                        cx,
                                    )
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
                    let mut chunk = style::chunk(v_flex()).child(self.chunk_header(
                        part_id,
                        icon,
                        SharedString::from(kind.label()),
                        Some(summary),
                        cx,
                    ));
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
                                    selectable_markdown(
                                        body_id,
                                        text.trim().to_string(),
                                        window,
                                        cx,
                                    )
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

impl Render for TranscriptList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            if let Some(id) = self.highlight
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
                        "transcript-list-empty",
                        self.empty_message.clone(),
                        window,
                        cx,
                    ))
                    .into_any_element(),
            );
        }
        if std::mem::take(&mut self.scroll_to_highlight) {
            match (self.highlight, highlight_row) {
                (Some(_), Some(row)) => self.scroll.scroll_to_item(row),
                (Some(_), None) => {}
                _ => self.scroll.scroll_to_bottom(),
            }
        }

        div()
            .size_full()
            .min_h_0()
            .relative()
            .child(
                v_flex()
                    .id("transcript-list")
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
            label: None,
        }
    }

    fn text(text: &str) -> ReplyPart {
        ReplyPart::Text { text: text.into() }
    }

    fn entry_chunk(entry: usize) -> ChunkId {
        ChunkId { entry, part: None }
    }

    fn plain(kind: EntryKind, body: &str) -> Entry {
        Entry {
            kind,
            body: body.into(),
            parts: Vec::new(),
            label: None,
        }
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

    #[test]
    fn a_raw_entry_is_one_chunk_labelled_by_its_host() {
        let entries = vec![Entry::raw(
            true,
            "#1 · request · Fleet",
            "{\"jsonrpc\":\"2.0\"}",
        )];
        let toggled = HashMap::new();
        assert_eq!(chunks(&entries, &toggled), [entry_chunk(0)]);
        // Raw payloads show verbatim by default, as the transcripts window
        // always showed them, and can be collapsed away.
        assert!(is_expanded(&entries, &toggled, entry_chunk(0)));
    }

    #[test]
    fn a_marker_is_not_a_chunk() {
        let entries = vec![
            plain(EntryKind::Marker, "Started a fresh agent session"),
            plain(EntryKind::User, "hello"),
        ];
        assert_eq!(chunks(&entries, &HashMap::new()), [entry_chunk(1)]);
    }

    #[test]
    fn a_reply_contributes_its_pieces_as_chunks_while_expanded() {
        let entries = vec![agent(
            vec![
                ReplyPart::Thought {
                    text: "Hmm".into(),
                },
                text("Done."),
            ],
            "Done.",
        )];
        let mut toggled = HashMap::new();
        assert_eq!(
            chunks(&entries, &toggled),
            [
                entry_chunk(0),
                ChunkId {
                    entry: 0,
                    part: Some(0)
                },
                ChunkId {
                    entry: 0,
                    part: Some(1)
                },
            ]
        );
        // Collapsed, the reply's pieces are no longer stops.
        toggled.insert(entry_chunk(0), false);
        assert_eq!(chunks(&entries, &toggled), [entry_chunk(0)]);
    }
}
