use crate::app::transcript_window::TranscriptWindowControl;
use crate::ui::actionable::{
    chrome_control_with_shortcut, render_label_badge, render_shortcut_pill,
};
use crate::ui::selectable_text::selectable_text;
use crate::ui::transcript_list::{
    self, ChunkId, Entry, EntryKind, StartState, TranscriptList, TranscriptListEvent,
};
use chrono::{Local, TimeZone};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, MouseButton, ParentElement, Pixels, Render, SharedString, Styled, Subscription,
    Window, actions, div, px,
};
use gpui_component::button::Button;
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, StyledExt, h_flex, v_flex};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use tod_agent::{FormatProblem, Transcript, TranscriptTurn};
use tod_core::run_transcript;
use tod_store::agent_traffic::{
    AgentSummary, SharedAgentTrafficLog, TrafficDirection, TrafficEntry,
};
use tod_store::fleet::{AgentSession, FleetStore};

/// A row in the agent picker: a recorded agent session, whatever started it,
/// or traffic this process logged under a key no session was recorded for.
#[derive(Debug, Clone)]
struct AgentRow {
    /// The agent session id, or the traffic key for traffic alone.
    id: String,
    label: String,
    detail: String,
    last_activity_ms: Option<i64>,
    /// A recorded session, whose transcript is read from its platform's
    /// record.
    session: bool,
    /// The key this process logged the row's raw traffic under.
    traffic_key: Option<String>,
}

fn format_timestamp_ms(ms: i64) -> String {
    match Local.timestamp_millis_opt(ms).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

const AGENT_TRANSCRIPTS_CONTEXT: &str = "AgentTranscripts";
const AGENTS_LIST_WIDTH: f32 = 320.0;
const AGENTS_LIST_MIN: f32 = 200.0;
const TRANSCRIPT_PANEL_MIN: f32 = 280.0;

actions!(
    agent_transcripts,
    [
        AgentTranscriptsClose,
        AgentTranscriptsRefresh,
        AgentTranscriptsSelectUp,
        AgentTranscriptsSelectDown,
        AgentTranscriptsPick1,
        AgentTranscriptsPick2,
        AgentTranscriptsPick3,
        AgentTranscriptsPick4,
        AgentTranscriptsPick5,
        AgentTranscriptsPick6,
        AgentTranscriptsPick7,
        AgentTranscriptsPick8,
        AgentTranscriptsPick9,
    ]
);

pub fn register_agent_transcripts_keyboard_bindings(cx: &mut App) {
    use crate::ui::key_context;
    let context = Some(key_context::excluding_input(AGENT_TRANSCRIPTS_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("r", AgentTranscriptsRefresh, context),
        KeyBinding::new("up", AgentTranscriptsSelectUp, context),
        KeyBinding::new("down", AgentTranscriptsSelectDown, context),
        KeyBinding::new("1", AgentTranscriptsPick1, context),
        KeyBinding::new("2", AgentTranscriptsPick2, context),
        KeyBinding::new("3", AgentTranscriptsPick3, context),
        KeyBinding::new("4", AgentTranscriptsPick4, context),
        KeyBinding::new("5", AgentTranscriptsPick5, context),
        KeyBinding::new("6", AgentTranscriptsPick6, context),
        KeyBinding::new("7", AgentTranscriptsPick7, context),
        KeyBinding::new("8", AgentTranscriptsPick8, context),
        KeyBinding::new("9", AgentTranscriptsPick9, context),
    ]);
    key_context::bind_panel_escape(cx, AgentTranscriptsClose, AGENT_TRANSCRIPTS_CONTEXT);
}

pub struct AgentTranscriptsView {
    fleet: Arc<FleetStore>,
    traffic_log: SharedAgentTrafficLog,
    window_control: TranscriptWindowControl,
    focus_handle: FocusHandle,
    agents: Vec<AgentRow>,
    selected_agent_id: Option<String>,
    /// The selected run's transcript as stored, read after the fact from
    /// the platform's own record of the session.
    history: Option<Transcript>,
    /// Why the selected run's transcript could not be read.
    read_error: Option<String>,
    /// What in the platform's record of the selected run the reader did not
    /// know, and so left out.
    format_problems: Vec<FormatProblem>,
    /// Runs whose transcript is being read.
    reading: HashSet<String>,
    /// Raw traffic this process logged for the selected agent.
    turns: Vec<TurnRow>,
    header: SharedString,
    /// The selected agent's turns, rendered by the same component the
    /// conversation view uses.
    transcript: Entity<TranscriptList>,
    /// What the transcript shows, rebuilt when the selected agent's turns
    /// change rather than every frame: a long transcript is megabytes.
    entries: Vec<Entry>,
    /// Chunks the user toggled away from how they start.
    toggled: HashMap<ChunkId, bool>,
    _transcript_subscription: Subscription,
}

#[derive(Debug, Clone)]
struct TurnRow {
    sequence: u64,
    direction: TrafficDirection,
    label: SharedString,
    content: SharedString,
}

impl AgentTranscriptsView {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        fleet: Arc<FleetStore>,
        traffic_log: SharedAgentTrafficLog,
        window_control: TranscriptWindowControl,
    ) -> Self {
        // Everything starts collapsed: a transcript is read by opening the
        // turns that matter, not by scrolling past all of them.
        let transcript = cx.new(|_| TranscriptList::starting(StartState::Collapsed));
        let subscription = cx.subscribe(&transcript, |this, _, event, cx| match event {
            TranscriptListEvent::ChunkClicked(id) => this.toggle(*id, cx),
        });
        let mut this = Self {
            fleet,
            traffic_log,
            window_control,
            focus_handle: cx.focus_handle(),
            agents: Vec::new(),
            selected_agent_id: None,
            history: None,
            read_error: None,
            format_problems: Vec::new(),
            reading: HashSet::new(),
            turns: Vec::new(),
            header: "Agent transcripts".into(),
            transcript,
            entries: Vec::new(),
            toggled: HashMap::new(),
            _transcript_subscription: subscription,
        };
        this.reload_agents();
        if let Some(first) = this.first_agent_id() {
            this.select_agent(first, cx);
        }
        let focus = this.focus_handle.clone();
        cx.defer_in(window, move |_, window, cx| {
            focus.focus(window, cx);
        });
        this
    }

    fn focus(&mut self, window: &mut Window, cx: &mut gpui::App) {
        self.focus_handle.focus(window, cx);
    }

    fn close(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        self.window_control.clear();
        window.remove_window();
    }

    fn first_agent_id(&self) -> Option<String> {
        self.agents.first().map(|a| a.id.clone())
    }

    /// Every recorded agent session, newest first. A session's live
    /// traffic, logged under its key, goes with the newest session that has
    /// the key; traffic under a key no session has gets a row of its own.
    fn reload_agents(&mut self) {
        let sessions = self
            .fleet
            .list_agent_sessions_without_transcripts()
            .unwrap_or_else(|err| {
                tracing::error!("listing agent sessions failed: {err:#}");
                Vec::new()
            });
        let mut traffic: HashMap<String, AgentSummary> = self
            .traffic_log
            .lock()
            .map(|log| log.agent_summaries())
            .unwrap_or_default()
            .into_iter()
            .map(|summary| (summary.id.clone(), summary))
            .collect();

        let mut rows: Vec<AgentRow> = sessions
            .into_iter()
            .map(|session| {
                let summary = session
                    .session_key
                    .as_ref()
                    .and_then(|key| traffic.remove(key));
                agent_row_from_session(session, summary)
            })
            .collect();
        rows.extend(traffic.into_values().map(agent_row_from_summary));
        rows.sort_by(|a, b| {
            b.last_activity_ms
                .cmp(&a.last_activity_ms)
                .then_with(|| a.label.cmp(&b.label))
        });
        self.agents = rows;
    }

    /// Expand or collapse one chunk.
    fn toggle(&mut self, id: ChunkId, cx: &mut Context<Self>) {
        let start = StartState::Collapsed;
        let expanded = !transcript_list::is_expanded(&self.entries, &self.toggled, id, start);
        if expanded == transcript_list::expanded_by_default(&self.entries, id, start) {
            self.toggled.remove(&id);
        } else {
            self.toggled.insert(id, expanded);
        }
        self.sync_transcript(cx);
        cx.notify();
    }

    /// Rebuild the entries from the selected agent's turns and bring the
    /// transcript up to date. This window has no chunk highlight of its own
    /// — the agent list owns the keyboard — so the list is fed entries and
    /// what the user toggled, nothing more.
    fn sync_transcript(&mut self, cx: &mut Context<Self>) {
        self.entries = self.build_entries();
        let entries = self.entries.clone();
        let toggled = self.toggled.clone();
        let reading = self
            .selected_agent_id
            .as_ref()
            .is_some_and(|id| self.reading.contains(id));
        self.transcript.update(cx, |list, cx| {
            list.set_entries(entries, cx);
            list.set_toggled(toggled, cx);
            list.set_status(
                reading,
                reading.then(|| "reading the transcript".to_string()),
                cx,
            );
            list.set_empty_message("No transcript turns for this agent yet.", cx);
        });
    }

    /// The selected agent's transcript entries: the session's stored transcript,
    /// read as the conversation view reads a conversation, then any raw
    /// traffic this process logged for it. A request carries what we sent,
    /// so it reads as the outgoing side.
    fn build_entries(&self) -> Vec<Entry> {
        let history = self.history.iter().flat_map(|transcript| &transcript.turns);
        let format_problems = (!self.format_problems.is_empty()).then(|| Entry {
            kind: EntryKind::Error,
            body: format!(
                "The transcript format has changed, so parts of this transcript are left \
                 out until the reader is updated: {}.",
                self.format_problems
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            parts: Vec::new(),
            label: None,
        });
        let read_error = self.read_error.iter().map(|err| Entry {
            kind: EntryKind::Error,
            body: format!("Couldn't read the transcript: {err}"),
            parts: Vec::new(),
            label: None,
        });
        format_problems
            .into_iter()
            .chain(history.map(entry_of_turn))
            .chain(read_error)
            .chain(self.turns.iter().map(|turn| {
                Entry::raw(
                    turn.direction == TrafficDirection::Request,
                    turn.label.clone(),
                    turn.content.to_string(),
                )
            }))
            .collect()
    }

    /// How many turns the selected agent shows.
    fn shown_turns(&self) -> usize {
        self.history.as_ref().map_or(0, |t| t.turns.len()) + self.turns.len()
    }

    fn set_header(&mut self, agent_id: &str) {
        let label = self
            .agents
            .iter()
            .find(|agent| agent.id == agent_id)
            .map_or(agent_id, |agent| agent.label.as_str());
        self.header = format!("{label} · {}", turn_count(self.shown_turns())).into();
    }

    /// Read the session's transcript when nothing usable is stored or the
    /// session has moved on since it was. Both the check and the read touch
    /// the platform's files, so they run on a background thread.
    fn refresh_history(&mut self, session: AgentSession, cx: &mut Context<Self>) {
        let id = session.agent_session_id.clone();
        if !self.reading.insert(id.clone()) {
            return;
        }
        let fleet = self.fleet.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = if run_transcript::session_needs_capture(&session) {
                run_transcript::session_capture(&fleet, &session)
                    .map(|read| read.map(|(_, read)| read))
                    .map_err(|err| format!("{err:#}"))
            } else {
                Ok(None)
            };
            let _ = tx.send_blocking(result);
        });
        cx.spawn(async move |this, cx| {
            let result = rx
                .recv()
                .await
                .unwrap_or_else(|_| Err("the transcript read panicked".into()));
            let _ = this.update(cx, |this, cx| {
                this.reading.remove(&id);
                this.reload_agents();
                if this.selected_agent_id.as_deref() == Some(id.as_str()) {
                    match result {
                        Ok(Some(read)) => {
                            this.history = Some(read.transcript);
                            this.format_problems = read.problems;
                            this.read_error = None;
                        }
                        Ok(None) => {}
                        Err(err) => this.read_error = Some(err),
                    }
                    this.set_header(&id);
                }
                this.sync_transcript(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn select_agent(&mut self, agent_id: String, cx: &mut Context<Self>) {
        if self.selected_agent_id.as_deref() != Some(agent_id.as_str()) {
            self.toggled.clear();
            self.transcript.update(cx, |list, cx| list.reset(cx));
        }
        self.selected_agent_id = Some(agent_id.clone());
        let row = self
            .agents
            .iter()
            .find(|agent| agent.id == agent_id)
            .cloned();
        self.turns = row
            .as_ref()
            .and_then(|row| row.traffic_key.as_deref())
            .map(|key| self.load_turns(key))
            .unwrap_or_default();
        let session = row
            .filter(|row| row.session)
            .and_then(|_| self.fleet.get_agent_session(&agent_id).ok().flatten());
        self.history = session
            .as_ref()
            .and_then(|session| session.cached_transcript.as_deref())
            .and_then(Transcript::from_stored);
        self.read_error = None;
        self.format_problems.clear();
        self.set_header(&agent_id);
        if let Some(session) = session {
            self.refresh_history(session, cx);
        }
        self.sync_transcript(cx);
        cx.notify();
    }

    fn load_turns(&self, agent_id: &str) -> Vec<TurnRow> {
        let mut rows: Vec<TurnRow> = Vec::new();

        if let Ok(log) = self.traffic_log.lock() {
            for entry in log.entries_for_agent(agent_id) {
                rows.push(entry_to_row(entry));
            }
        }

        rows.sort_by_key(|row| row.sequence);
        rows
    }

    fn copy_transcript(&mut self, cx: &mut Context<Self>) {
        let mut text = String::new();
        if let Some(history) = &self.history {
            text.push_str(&history.to_text());
            text.push_str("\n\n");
        }
        for turn in &self.turns {
            text.push_str(&turn.label);
            text.push('\n');
            text.push_str(&turn.content);
            text.push_str("\n\n");
        }
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected_agent_id.clone();
        self.reload_agents();
        if let Some(id) = selected {
            self.select_agent(id, cx);
        } else if let Some(first) = self.first_agent_id() {
            self.select_agent(first, cx);
        } else {
            self.turns.clear();
            self.history = None;
            self.read_error = None;
            self.format_problems.clear();
            self.header = "Agent transcripts · no agents yet".into();
            self.sync_transcript(cx);
            cx.notify();
        }
    }

    fn flat_agent_ids(&self) -> Vec<String> {
        self.agents.iter().map(|agent| agent.id.clone()).collect()
    }

    fn select_adjacent(&mut self, delta: i32, cx: &mut Context<Self>) {
        let ids = self.flat_agent_ids();
        if ids.is_empty() {
            return;
        }
        let current = self
            .selected_agent_id
            .as_ref()
            .and_then(|id| ids.iter().position(|candidate| candidate == id))
            .unwrap_or(0);
        let next = (current as i32 + delta).clamp(0, ids.len() as i32 - 1) as usize;
        if next != current {
            self.select_agent(ids[next].clone(), cx);
        }
    }

    fn pick_by_index(&mut self, index: usize, cx: &mut Context<Self>) {
        let ids = self.flat_agent_ids();
        if let Some(id) = ids.get(index) {
            self.select_agent(id.clone(), cx);
        }
    }

    fn agent_pick_badges(&self) -> BTreeMap<String, String> {
        let mut badges = BTreeMap::new();
        for (index, id) in self.flat_agent_ids().into_iter().take(9).enumerate() {
            badges.insert(id, (index + 1).to_string());
        }
        badges
    }
}

#[allow(clippy::too_many_arguments)]
fn render_agent_section(
    title: &'static str,
    rows: &[AgentRow],
    index_offset: usize,
    selected_agent_id: &Option<String>,
    pick_badges: &BTreeMap<String, String>,
    window: &mut Window,
    cx: &mut Context<AgentTranscriptsView>,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    muted_bg: gpui::Hsla,
    foreground: gpui::Hsla,
    accent: gpui::Hsla,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .px_2()
                .text_xs()
                .font_semibold()
                .text_color(muted)
                .child(title),
        )
        .children(rows.iter().enumerate().map(|(ix, agent)| {
            let selected = selected_agent_id.as_deref() == Some(agent.id.as_str());
            let badge = pick_badges.get(&agent.id).cloned();
            let subtitle = match agent.last_activity_ms {
                Some(ms) => format!("{} · {}", agent.detail, format_timestamp_ms(ms)),
                None => agent.detail.clone(),
            };
            div()
                .id(("agent-pick", index_offset + ix))
                .relative()
                .px_2()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .border_1()
                .border_color(if selected { accent } else { border })
                .bg(if selected {
                    accent.opacity(0.12)
                } else {
                    muted_bg
                })
                .hover(|s| s.bg(border.opacity(0.35)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener({
                        let id = agent.id.clone();
                        move |this, _, _, cx| {
                            this.select_agent(id.clone(), cx);
                        }
                    }),
                )
                .child(
                    v_flex()
                        .gap_0p5()
                        .pr(if badge.is_some() { px(18.) } else { px(0.) })
                        .child(
                            selectable_text(
                                gpui::SharedString::from(format!("agent-pick-label-{}", agent.id)),
                                agent.label.clone(),
                                window,
                                cx,
                            )
                            .text_sm()
                            .text_color(foreground),
                        )
                        .child(
                            selectable_text(
                                gpui::SharedString::from(format!("agent-pick-turns-{}", agent.id)),
                                subtitle,
                                window,
                                cx,
                            )
                            .text_xs()
                            .text_color(muted),
                        ),
                )
                .when_some(badge, |row, label| {
                    row.child(
                        div()
                            .absolute()
                            .bottom_0()
                            .right_0()
                            .child(render_label_badge(label, cx)),
                    )
                })
        }))
}

fn agent_row_from_session(session: AgentSession, traffic: Option<AgentSummary>) -> AgentRow {
    let label = [session.title.as_deref(), session.session_key.as_deref()]
        .into_iter()
        .flatten()
        .find(|label| !label.is_empty())
        .unwrap_or(&session.agent_session_id)
        .to_string();
    let platform = session
        .platform
        .as_deref()
        .and_then(tod_store::parse_platform)
        .map_or("agent session", |platform| platform.label());
    let detail = match &traffic {
        Some(summary) => format!("{platform} · {} logged", summary.entry_count),
        None => platform.to_string(),
    };
    let last_activity_ms = traffic.as_ref().map_or(session.started_at, |summary| {
        summary.last_timestamp_ms.max(session.started_at)
    });
    AgentRow {
        id: session.agent_session_id,
        label,
        detail,
        last_activity_ms: Some(last_activity_ms),
        session: true,
        traffic_key: traffic.map(|summary| summary.id),
    }
}

fn agent_row_from_summary(summary: AgentSummary) -> AgentRow {
    AgentRow {
        detail: format!("{} logged", summary.entry_count),
        label: summary.label,
        last_activity_ms: Some(summary.last_timestamp_ms),
        session: false,
        traffic_key: Some(summary.id.clone()),
        id: summary.id,
    }
}

/// A stored transcript turn as a transcript entry, the same shape the
/// conversation view gives its own turns.
fn entry_of_turn(turn: &TranscriptTurn) -> Entry {
    let (kind, parts) = match turn {
        TranscriptTurn::User { .. } => (EntryKind::User, Vec::new()),
        TranscriptTurn::Agent { parts } => (EntryKind::Agent, parts.clone()),
    };
    Entry {
        kind,
        body: turn.text(),
        parts,
        label: None,
    }
}

fn turn_count(count: usize) -> String {
    match count {
        1 => "1 turn".to_string(),
        n => format!("{n} turns"),
    }
}

fn entry_to_row(entry: TrafficEntry) -> TurnRow {
    TurnRow {
        sequence: entry.sequence,
        direction: entry.direction,
        label: format!(
            "#{} · {} · {}",
            entry.sequence,
            entry.direction.label(),
            entry.category.label()
        )
        .into(),
        content: entry.content.into(),
    }
}

impl Focusable for AgentTranscriptsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AgentTranscriptsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let muted_bg = cx.theme().muted;
        let accent = cx.theme().primary;
        let pick_badges = self.agent_pick_badges();

        h_flex()
            .key_context(AGENT_TRANSCRIPTS_CONTEXT)
            .size_full()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &AgentTranscriptsClose, window, cx| {
                this.close(window, cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsRefresh, _, cx| {
                this.refresh(cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsSelectUp, _, cx| {
                this.select_adjacent(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsSelectDown, _, cx| {
                this.select_adjacent(1, cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsPick1, _, cx| {
                this.pick_by_index(0, cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsPick2, _, cx| {
                this.pick_by_index(1, cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsPick3, _, cx| {
                this.pick_by_index(2, cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsPick4, _, cx| {
                this.pick_by_index(3, cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsPick5, _, cx| {
                this.pick_by_index(4, cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsPick6, _, cx| {
                this.pick_by_index(5, cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsPick7, _, cx| {
                this.pick_by_index(6, cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsPick8, _, cx| {
                this.pick_by_index(7, cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsPick9, _, cx| {
                this.pick_by_index(8, cx);
            }))
            .child(
                h_resizable("agent-transcripts-columns")
                    .child(
                        resizable_panel()
                            .size(px(AGENTS_LIST_WIDTH))
                            .size_range(px(AGENTS_LIST_MIN)..Pixels::MAX)
                            .child(
                                v_flex()
                                    .h_full()
                                    .min_w_0()
                                    .bg(muted_bg)
                                    .child(
                                        h_flex()
                                            .px_3()
                                            .py_2()
                                            .border_b_1()
                                            .border_color(border)
                                            .justify_between()
                                            .items_center()
                                            .child(
                                                h_flex()
                                                    .gap_1()
                                                    .items_center()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .font_semibold()
                                                            .text_color(foreground)
                                                            .child("Agents"),
                                                    )
                                                    .when_some(
                                                        render_shortcut_pill(
                                                            window,
                                                            &AgentTranscriptsSelectUp,
                                                            AGENT_TRANSCRIPTS_CONTEXT,
                                                            cx,
                                                        ),
                                                        |row, pill| row.child(pill),
                                                    )
                                                    .when_some(
                                                        render_shortcut_pill(
                                                            window,
                                                            &AgentTranscriptsSelectDown,
                                                            AGENT_TRANSCRIPTS_CONTEXT,
                                                            cx,
                                                        ),
                                                        |row, pill| row.child(pill),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_1()
                                                    .child(chrome_control_with_shortcut(
                                                        Button::new("refresh-transcripts")
                                                            .label("Refresh")
                                                            .outline()
                                                            .compact()
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| {
                                                                    this.refresh(cx);
                                                                },
                                                            )),
                                                        window,
                                                        &AgentTranscriptsRefresh,
                                                        AGENT_TRANSCRIPTS_CONTEXT,
                                                        cx,
                                                    ))
                                                    .child(chrome_control_with_shortcut(
                                                        Button::new("close-transcripts")
                                                            .label("Close")
                                                            .outline()
                                                            .compact()
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    this.close(window, cx);
                                                                },
                                                            )),
                                                        window,
                                                        &AgentTranscriptsClose,
                                                        AGENT_TRANSCRIPTS_CONTEXT,
                                                        cx,
                                                    )),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h_0()
                                            .overflow_y_scrollbar()
                                            .p_2()
                                            .v_flex()
                                            .gap_2()
                                            .when(self.agents.is_empty(), |el| {
                                                el.child(
                                                    div()
                                                        .px_2()
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .child("No agent sessions recorded yet."),
                                                )
                                            })
                                            .when(!self.agents.is_empty(), |el| {
                                                el.child(render_agent_section(
                                                    "Agent sessions",
                                                    &self.agents,
                                                    0,
                                                    &self.selected_agent_id,
                                                    &pick_badges,
                                                    window,
                                                    cx,
                                                    border,
                                                    muted,
                                                    muted_bg,
                                                    foreground,
                                                    accent,
                                                ))
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        resizable_panel()
                            .size_range(px(TRANSCRIPT_PANEL_MIN)..Pixels::MAX)
                            .child(
                                v_flex()
                                    .h_full()
                                    .min_w_0()
                                    .child(
                                        h_flex()
                                            .px_4()
                                            .py_2()
                                            .border_b_1()
                                            .border_color(border)
                                            .justify_between()
                                            .items_center()
                                            .child(
                                                selectable_text(
                                                    "agent-transcript-header",
                                                    self.header.clone(),
                                                    window,
                                                    cx,
                                                )
                                                .text_sm()
                                                .font_semibold()
                                                .text_color(foreground),
                                            )
                                            .child(
                                                Button::new("copy-transcript")
                                                    .label("Copy transcript")
                                                    .outline()
                                                    .compact()
                                                    .disabled(self.shown_turns() == 0)
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.copy_transcript(cx);
                                                    })),
                                            ),
                                    )
                                    .child(div().flex_1().min_h_0().child(self.transcript.clone())),
                            ),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus(window, cx);
                }),
            )
    }
}
