//! The agent transcripts window: a list of recorded agent sessions, and the
//! transcript of the one under the cursor.
//!
//! The session list — the cursor, the keys, the day groups, the scrolling and
//! the column header — is [`crate::ui::item_list`]. Only what a session *is*
//! lives here: when it was last active, what to call it, and what it was.
//! The transcript beside it is [`crate::ui::transcript_list`], a chat log
//! rather than a list of items, and is not this component's business.

use crate::app::transcript_window::TranscriptWindowControl;
use crate::ui::actionable::{render_label_badge, render_shortcut_pill};
use crate::ui::item_list::keyboard::{
    ItemListCollapse, ItemListDown, ItemListEnd, ItemListExpand, ItemListHome, ItemListPageDown,
    ItemListPageUp, ItemListUp,
};
use crate::ui::item_list::{
    CollapseStep, ColumnSpec, GroupSpec, ItemList, ItemListEvent, ItemListKeys, ItemListRow,
    ItemRowState, bind_item_list_keys,
};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::ui::token_usage;
use crate::ui::transcript_list::{
    self, ChunkId, Entry, EntryKind, StartState, TranscriptList, TranscriptListEvent,
};
use crate::views::rows::RowHost;
use chrono::{DateTime, Local, NaiveDate, TimeZone};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, ParentElement, Pixels, Render, SharedString, Styled,
    Subscription, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::{ActiveTheme, Disableable, Sizable, StyledExt, TitleBar, h_flex, v_flex};
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

/// The day a row's last activity fell on, as a group key and a heading.
///
/// Sessions are listed newest first, so a day is a contiguous run and grouping
/// by it does not reorder anything. It also makes the time column readable: a
/// row under a day heading only has to say the time of day.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ActivityDay {
    key: String,
    label: SharedString,
}

fn activity_day(ms: Option<i64>, today: NaiveDate) -> ActivityDay {
    match ms.and_then(local_time) {
        Some(when) => {
            let date = when.date_naive();
            let label = match (today - date).num_days() {
                0 => "Today".to_string(),
                1 => "Yesterday".to_string(),
                _ => date.format("%Y-%m-%d").to_string(),
            };
            ActivityDay {
                key: format!("day-{date}"),
                label: label.into(),
            }
        }
        // A row whose activity we have no time for still belongs somewhere,
        // and the sort already puts it last.
        None => ActivityDay {
            key: "day-unknown".to_string(),
            label: "No recorded activity".into(),
        },
    }
}

/// The sessions grouped by the day each was last active, in list order. The
/// sessions arrive newest first, so a day is one contiguous run and grouping
/// never reorders anything.
fn day_runs(agents: &[AgentRow], today: NaiveDate) -> Vec<(ActivityDay, Vec<&AgentRow>)> {
    let mut runs: Vec<(ActivityDay, Vec<&AgentRow>)> = Vec::new();
    for agent in agents {
        let day = activity_day(agent.last_activity_ms, today);
        match runs.last_mut() {
            Some((open, run)) if open.key == day.key => run.push(agent),
            _ => runs.push((day, vec![agent])),
        }
    }
    runs
}

fn local_time(ms: i64) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(ms).single()
}

/// The time of day a row was last active. The day itself is the group heading
/// above it.
fn format_time_of_day(ms: Option<i64>) -> String {
    match ms.and_then(local_time) {
        Some(when) => when.format("%H:%M").to_string(),
        None => "—".to_string(),
    }
}

const COLUMN_TIME: &str = "time";
const COLUMN_AGENT: &str = "agent";

/// The list is a table of the two values every row has: when it was last
/// active, and what it is. What it *ran on* — the platform, and how much
/// traffic was logged — is only sometimes known (traffic under a key no
/// session was recorded for has no platform), so it stays inside the content
/// column as trailing context.
fn agent_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::fixed(COLUMN_TIME, COLUMN_TIME, style::size::TIMESTAMP_COLUMN),
        ColumnSpec::content(COLUMN_AGENT, COLUMN_AGENT),
    ]
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
    // An agent session is a record of what already happened: it cannot be
    // edited, created, reordered or marked, so the window takes navigation and
    // nothing else from the one key set.
    bind_item_list_keys(cx, AGENT_TRANSCRIPTS_CONTEXT, ItemListKeys::default());
    let context = Some(key_context::excluding_input(AGENT_TRANSCRIPTS_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("r", AgentTranscriptsRefresh, context),
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

/// What the user did in the session list, queued for the view to apply.
#[derive(Debug, Clone)]
enum AgentListAction {
    Select {
        row_ix: usize,
    },
    ToggleGroup {
        key: String,
    },
    /// Something the list can report but a read-only list never does.
    Ignored,
}

impl From<ItemListEvent> for AgentListAction {
    fn from(event: ItemListEvent) -> Self {
        match event {
            ItemListEvent::Select { row_ix } => Self::Select { row_ix },
            ItemListEvent::ToggleGroup { key } => Self::ToggleGroup { key },
            // A session list is ordered by when each one last spoke, so
            // there is nothing for a drag to rearrange.
            ItemListEvent::ToggleMark { .. } | ItemListEvent::Drop(_) => Self::Ignored,
        }
    }
}

pub struct AgentTranscriptsView {
    fleet: Arc<FleetStore>,
    traffic_log: SharedAgentTrafficLog,
    window_control: TranscriptWindowControl,
    focus_handle: FocusHandle,
    /// Every session, newest first: what the list's rows are built from, and
    /// rebuilt from when a day group collapses.
    agents: Vec<AgentRow>,
    /// The rows, the cursor, the collapsed days and the scrolling: everything
    /// every list in the app shares.
    list: ItemList<AgentRow>,
    host: RowHost<AgentListAction>,
    /// The session the transcript beside the list is showing. It follows the
    /// cursor, and stays put while the cursor is on a day heading.
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
    /// Every usage figure is shown, not just the one-line summary.
    usage_expanded: bool,
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
            list: ItemList::new().with_columns(agent_columns()),
            host: RowHost::for_entity(cx.weak_entity()),
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
            usage_expanded: false,
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

    /// The rows the list shows: a heading per day of last activity, and the
    /// sessions under it. A collapsed day contributes its heading alone.
    fn rebuild_rows(&mut self) {
        let today = Local::now().date_naive();
        let mut rows: Vec<ItemListRow<AgentRow>> = Vec::new();
        for (day, run) in day_runs(&self.agents, today) {
            let collapsed = self.list.is_collapsed(&day.key);
            rows.push(ItemListRow::heading(
                GroupSpec::new(day.key, 0, day.label)
                    .count(run.len())
                    .collapsed(collapsed),
            ));
            if !collapsed {
                rows.extend(
                    run.into_iter()
                        .map(|agent| ItemListRow::item(agent.id.clone(), agent.clone())),
                );
            }
        }
        self.list.set_rows(rows);
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
        self.rebuild_rows();
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
            summary: None,
        });
        let read_error = self.read_error.iter().map(|err| Entry {
            kind: EntryKind::Error,
            body: format!("Couldn't read the transcript: {err}"),
            parts: Vec::new(),
            label: None,
            summary: None,
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
        // The cursor and the shown transcript are one selection: whichever
        // way a session was picked, the list ends up highlighting it.
        self.list.set_cursor_key(Some(agent_id.clone()));
        if let Some(ix) = self
            .list
            .rows()
            .iter()
            .position(|row| row.key() == agent_id.as_str())
        {
            self.list.set_cursor(ix);
        }
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

    /// The sessions the list is showing, in order. A collapsed day's sessions
    /// are not among them, so a number badge always names a row on screen.
    fn visible_agent_ids(&self) -> Vec<String> {
        self.list.items().map(|agent| agent.id.clone()).collect()
    }

    fn pick_by_index(&mut self, index: usize, cx: &mut Context<Self>) {
        let ids = self.visible_agent_ids();
        if let Some(id) = ids.get(index) {
            self.select_agent(id.clone(), cx);
        }
    }

    fn agent_pick_badges(&self) -> BTreeMap<String, String> {
        let mut badges = BTreeMap::new();
        for (index, id) in self.visible_agent_ids().into_iter().take(9).enumerate() {
            badges.insert(id, (index + 1).to_string());
        }
        badges
    }

    /// Apply what the rows and the list reported, in order.
    fn drain_row_actions(&mut self, cx: &mut Context<Self>) {
        for action in self.host.drain() {
            match action {
                AgentListAction::Select { row_ix } => {
                    if self.list.set_cursor(row_ix) {
                        self.follow_cursor(cx);
                    }
                }
                AgentListAction::ToggleGroup { key } => {
                    self.list.toggle_collapsed(&key);
                    self.rebuild_rows();
                    cx.notify();
                }
                AgentListAction::Ignored => {}
            }
        }
    }

    /// Show the transcript of whatever the cursor is now on. A day heading is
    /// not a session, so the cursor resting on one leaves the transcript as it
    /// was.
    fn follow_cursor(&mut self, cx: &mut Context<Self>) {
        match self.list.cursor_item().map(|agent| agent.id.clone()) {
            Some(id) if self.selected_agent_id.as_deref() != Some(id.as_str()) => {
                self.select_agent(id, cx)
            }
            _ => cx.notify(),
        }
    }

    fn move_cursor(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.list.move_cursor(delta) {
            self.follow_cursor(cx);
        }
    }

    fn on_arrow_up(&mut self, _: &ItemListUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(-1, cx);
    }

    fn on_arrow_down(&mut self, _: &ItemListDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(1, cx);
    }

    fn on_page_up(&mut self, _: &ItemListPageUp, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<AgentRow>::page_rows(window.viewport_size().height) as i32;
        self.move_cursor(-page, cx);
    }

    fn on_page_down(&mut self, _: &ItemListPageDown, window: &mut Window, cx: &mut Context<Self>) {
        let page = ItemList::<AgentRow>::page_rows(window.viewport_size().height) as i32;
        self.move_cursor(page, cx);
    }

    fn on_home(&mut self, _: &ItemListHome, _: &mut Window, cx: &mut Context<Self>) {
        if self.list.cursor_home() {
            self.follow_cursor(cx);
        }
    }

    fn on_end(&mut self, _: &ItemListEnd, _: &mut Window, cx: &mut Context<Self>) {
        if self.list.cursor_end() {
            self.follow_cursor(cx);
        }
    }

    fn on_collapse(&mut self, _: &ItemListCollapse, _: &mut Window, cx: &mut Context<Self>) {
        match self.list.collapse_step() {
            CollapseStep::Collapsed => {
                self.rebuild_rows();
                cx.notify();
            }
            CollapseStep::MovedToParent => cx.notify(),
            CollapseStep::Nothing => {}
        }
    }

    fn on_expand(&mut self, _: &ItemListExpand, _: &mut Window, cx: &mut Context<Self>) {
        if self.list.expand_step() {
            self.rebuild_rows();
            cx.notify();
        }
    }

    /// One session: when it was last active, what to call it, and what it was.
    fn render_agent(
        agent: &AgentRow,
        state: ItemRowState<'_>,
        badge: Option<String>,
        host: &RowHost<AgentListAction>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let row_ix = state.row_ix;
        let select_host = host.clone();
        style::row(h_flex())
            .w_full()
            .items_start()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                select_host.push(AgentListAction::Select { row_ix }, cx);
            })
            .when(state.highlighted, style::highlighted)
            .child(
                state
                    .column(COLUMN_TIME, style::text_dense_muted(div()))
                    .child(selectable_text(
                        SharedString::from(format!("agent-pick-time-{}", agent.id)),
                        format_time_of_day(agent.last_activity_ms),
                        window,
                        cx,
                    )),
            )
            .child(
                state
                    .column(COLUMN_AGENT, v_flex())
                    .gap(style::space::HAIRLINE)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(style::space::INLINE)
                            .child(div().flex_1().min_w_0().child(selectable_text(
                                SharedString::from(format!("agent-pick-label-{}", agent.id)),
                                agent.label.clone(),
                                window,
                                cx,
                            )))
                            .children(badge.map(|label| render_label_badge(label, cx))),
                    )
                    .child(style::text_dense_muted(div()).child(selectable_text(
                        SharedString::from(format!("agent-pick-detail-{}", agent.id)),
                        agent.detail.clone(),
                        window,
                        cx,
                    ))),
            )
            .into_any_element()
    }
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
        summary: None,
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

impl AgentTranscriptsView {
    /// The selected session's token usage under the header: a line, and
    /// every figure when expanded. Nothing for traffic with no session.
    fn render_usage(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let history = self.history.as_ref()?;
        let border = cx.theme().border;
        let line = match (&history.usage, history.usage_read) {
            (Some(usage), _) => token_usage::summary(usage),
            (None, false) => "Tokens: not read yet — kept from before usage was".to_string(),
            (None, true) => "Tokens: the platform's record of this session has none".to_string(),
        };
        let details = history
            .usage
            .as_ref()
            .filter(|_| self.usage_expanded)
            .map(token_usage::details);
        Some(
            v_flex()
                .w_full()
                .px_4()
                .py_1()
                .gap(style::space::HAIRLINE)
                .border_b_1()
                .border_color(border)
                .child(
                    h_flex()
                        .items_center()
                        .gap(style::space::RELATED)
                        .child(
                            style::text_dense_muted(div()).flex_1().min_w_0().child(
                                selectable_text("agent-transcript-usage", line, window, cx),
                            ),
                        )
                        .when(history.usage.is_some(), |row| {
                            row.child(
                                Button::new("agent-transcript-usage-details")
                                    .label(if self.usage_expanded { "Less" } else { "Details" })
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.usage_expanded = !this.usage_expanded;
                                        cx.notify();
                                    })),
                            )
                        }),
                )
                .children(details.map(|details| {
                    style::text_dense_muted(div()).child(selectable_text(
                        "agent-transcript-usage-details-text",
                        details,
                        window,
                        cx,
                    ))
                }))
                .into_any_element(),
        )
    }

    /// The window is opened with `TitleBar::title_bar_options()`, which leaves
    /// it without a system caption — the view has to draw one, or the window
    /// cannot be dragged, minimized, or closed by its own chrome.
    fn render_title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .gap(style::space::RELATED)
                .child("Agent transcripts")
                .child(div().flex_1())
                // The pill sits beside the button, not under it as
                // `chrome_control_with_shortcut` puts it: a title bar has no
                // room below, and the pill lands on top of the label.
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap(style::space::INLINE)
                        .child(
                            Button::new("close-transcripts")
                                .label("Close")
                                .ghost()
                                .compact()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close(window, cx);
                                })),
                        )
                        .when_some(
                            render_shortcut_pill(
                                window,
                                &AgentTranscriptsClose,
                                AGENT_TRANSCRIPTS_CONTEXT,
                                cx,
                            ),
                            |el, pill| el.child(pill),
                        ),
                ),
        )
    }
}

impl Render for AgentTranscriptsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_row_actions(cx);
        let border = cx.theme().border;
        let foreground = cx.theme().foreground;
        let muted_bg = cx.theme().muted;
        let pick_badges = self.agent_pick_badges();

        v_flex()
            .key_context(AGENT_TRANSCRIPTS_CONTEXT)
            .size_full()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &AgentTranscriptsClose, window, cx| {
                this.close(window, cx);
            }))
            .on_action(cx.listener(|this, _: &AgentTranscriptsRefresh, _, cx| {
                this.refresh(cx);
            }))
            .on_action(cx.listener(Self::on_arrow_up))
            .on_action(cx.listener(Self::on_arrow_down))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .on_action(cx.listener(Self::on_collapse))
            .on_action(cx.listener(Self::on_expand))
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
            .child(self.render_title_bar(window, cx))
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
                                                            &ItemListUp,
                                                            AGENT_TRANSCRIPTS_CONTEXT,
                                                            cx,
                                                        ),
                                                        |row, pill| row.child(pill),
                                                    )
                                                    .when_some(
                                                        render_shortcut_pill(
                                                            window,
                                                            &ItemListDown,
                                                            AGENT_TRANSCRIPTS_CONTEXT,
                                                            cx,
                                                        ),
                                                        |row, pill| row.child(pill),
                                                    ),
                                            )
                                            .child(
                                                // Pills beside their buttons,
                                                // not under them: this header
                                                // is one row tall, and a
                                                // stacked pill lands on top of
                                                // the label. Close lives in the
                                                // title bar, with the window's
                                                // own controls.
                                                h_flex()
                                                    .flex_shrink_0()
                                                    .items_center()
                                                    .gap(style::space::INLINE)
                                                    .child(
                                                        Button::new("refresh-transcripts")
                                                            .label("Refresh")
                                                            .outline()
                                                            .compact()
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| {
                                                                    this.refresh(cx);
                                                                },
                                                            )),
                                                    )
                                                    .when_some(
                                                        render_shortcut_pill(
                                                            window,
                                                            &AgentTranscriptsRefresh,
                                                            AGENT_TRANSCRIPTS_CONTEXT,
                                                            cx,
                                                        ),
                                                        |el, pill| el.child(pill),
                                                    ),
                                            ),
                                    )
                                    .child(if self.list.is_empty() {
                                        style::empty_message(div())
                                            .p(style::space::INSET)
                                            .child("No agent sessions recorded yet.")
                                            .into_any_element()
                                    } else {
                                        let host = self.host.clone();
                                        self.list.render(
                                            "agent-transcripts-list",
                                            &self.host,
                                            move |agent, state, window, cx| {
                                                let badge = pick_badges.get(&agent.id).cloned();
                                                Self::render_agent(
                                                    agent, state, badge, &host, window, cx,
                                                )
                                            },
                                            window,
                                            cx,
                                        )
                                    }),
                            ),
                    )
                    .child(
                        resizable_panel()
                            .size_range(px(TRANSCRIPT_PANEL_MIN)..Pixels::MAX)
                            .child(
                                v_flex()
                                    // `size_full`, not `h_full`: this panel has
                                    // no fixed width, so without it the column
                                    // shrinks to its content and the header
                                    // divider stops short of the panel edge.
                                    .size_full()
                                    .min_w_0()
                                    .child(
                                        h_flex()
                                            // Without this the header shrinks
                                            // to its content and its divider
                                            // stops short of the panel edge.
                                            .w_full()
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
                                    .children(self.render_usage(window, cx))
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Days;

    fn agent(id: &str, ms: Option<i64>) -> AgentRow {
        AgentRow {
            id: id.to_string(),
            label: id.to_string(),
            detail: "mock".to_string(),
            last_activity_ms: ms,
            session: true,
            traffic_key: None,
        }
    }

    /// Noon local on `date`, so a day's rows land on that day whatever the
    /// zone offset is.
    fn noon(date: NaiveDate) -> i64 {
        Local
            .from_local_datetime(&date.and_hms_opt(12, 0, 0).expect("noon"))
            .single()
            .expect("an unambiguous local noon")
            .timestamp_millis()
    }

    #[test]
    fn the_list_is_a_table_of_the_time_and_the_session() {
        let columns = agent_columns();
        let labels: Vec<&str> = columns.iter().map(|c| c.label.as_ref()).collect();
        assert_eq!(labels, vec![COLUMN_TIME, COLUMN_AGENT]);
        assert_eq!(columns[0].width, Some(style::size::TIMESTAMP_COLUMN));
        // The session is the content column: exactly one, as the component
        // requires.
        assert_eq!(columns[1].width, None);
    }

    #[test]
    fn a_day_is_named_by_how_recent_it_is() {
        let today = Local::now().date_naive();
        let yesterday = today.checked_sub_days(Days::new(1)).expect("yesterday");
        let older = today.checked_sub_days(Days::new(5)).expect("last week");
        assert_eq!(activity_day(Some(noon(today)), today).label, "Today");
        assert_eq!(
            activity_day(Some(noon(yesterday)), today).label,
            "Yesterday"
        );
        assert_eq!(
            activity_day(Some(noon(older)), today).label,
            older.format("%Y-%m-%d").to_string()
        );
    }

    #[test]
    fn a_session_with_no_recorded_time_still_has_a_group_and_a_cell() {
        let today = Local::now().date_naive();
        let day = activity_day(None, today);
        assert_eq!(day.key, "day-unknown");
        assert_eq!(day.label, "No recorded activity");
        assert_eq!(format_time_of_day(None), "—");
        assert_eq!(format_time_of_day(Some(i64::MAX)), "—");
    }

    #[test]
    fn sessions_from_the_same_day_share_one_heading() {
        let today = Local::now().date_naive();
        let yesterday = today.checked_sub_days(Days::new(1)).expect("yesterday");
        let agents = vec![
            agent("a", Some(noon(today))),
            agent("b", Some(noon(today) - 60_000)),
            agent("c", Some(noon(yesterday))),
            agent("d", None),
        ];
        let runs = day_runs(&agents, today);
        let shape: Vec<(String, usize)> = runs
            .iter()
            .map(|(day, run)| (day.label.to_string(), run.len()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("Today".to_string(), 2),
                ("Yesterday".to_string(), 1),
                ("No recorded activity".to_string(), 1),
            ]
        );
    }
}
