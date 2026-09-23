//! **Slated for deletion.** Every chat that used to open here now runs in the
//! conversation view under a protocol; the one caller left is the visual
//! design panel's embedded chat. Delete this file with
//! `views/visual_design_panel.rs`. See `doc/conversation/protocols.md` §6.
//!
//! Interactive fleet-agent chat window — prompt in, replies out.

use crate::app::InteractiveAgentWindowControl;
use crate::interview::agent::{AgentRunState, RunId, SharedAgent};
use crate::ui::key_context;
use crate::ui::selectable_text::selectable_markdown;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Render, StatefulInteractiveElement, Styled, Window, actions, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, Selectable, StyledExt, h_flex, v_flex};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tod_agent::{EngagementState, SessionOpening, SessionTurn, SharedEngagementRegistry};
use tod_core::run_transcript;
use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::{AgentRole, TodSettings, parse_platform, platform_storage};

const INTERACTIVE_AGENT_CONTEXT: &str = "InteractiveAgent";
const POLL_INTERVAL: Duration = Duration::from_millis(300);

actions!(
    interactive_agent,
    [
        SubmitInteractivePrompt,
        InteractiveAgentClose,
        InteractiveAgentStopUp,
        InteractiveAgentStopDown,
        InteractiveAgentActivate,
        InteractiveAgentEscape,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InteractiveAgentStop {
    /// The collapsed app-context panel. Only a stop while a context is pending.
    Context,
    Prompt,
    Submit,
}

struct PendingRun {
    run_id: Option<RunId>,
    user_text: String,
}

/// Prior-session history shown above the live conversation. Populated from
/// the run's stored transcript, or fetched in the background (the session is
/// loaded read-only) when a resumable session has none stored yet.
enum TranscriptHistory {
    /// No prior agent-side session — nothing to load.
    NotNeeded,
    Loading,
    Loaded(String),
    FetchFailed(String),
}

pub struct InteractiveAgentView {
    node_id: String,
    node_title: String,
    session_run_id: String,
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    workspace_cwd: PathBuf,
    settings: TodSettings,
    platform: String,
    model: String,
    effort: String,
    window_control: InteractiveAgentWindowControl,
    prompt_input: Entity<TextareaState>,
    conversation: Vec<(String, String)>,
    /// Prior-session history, distinct from `conversation` (this window's own
    /// live turns) — see [`TranscriptHistory`].
    history: TranscriptHistory,
    pending: Option<PendingRun>,
    /// Latest human-readable activity reported by the agent for the pending
    /// run (a tool call, a permission request, …), shown in place of the
    /// generic "Thinking…" label while there is something more specific to say.
    activity: Option<String>,
    status_line: String,
    error_banner: Option<String>,
    poll_lock_misses: u32,
    focus_handle: FocusHandle,
    focus_stop: InteractiveAgentStop,
    prompt_editing: bool,
    /// Human-readable session name, shown in the header and given to the
    /// agent-side session when the first message goes out.
    session_name: String,
    /// Agent-side session id, recorded after the first reply so a later process
    /// can resume the conversation.
    agent_session_id: Option<String>,
    /// Assembled app context for a new session. Nothing is sent until the user
    /// submits their first message; it goes out once, ahead of that message.
    context_prefix: Option<String>,
    /// The context panel is collapsed by default — it is rarely worth reading.
    context_expanded: bool,
    /// Scroll position of the transcript, so new turns can be scrolled into view.
    scroll_handle: gpui::ScrollHandle,
    /// When set, the transcript scrolls to the bottom whenever a new turn appears.
    auto_scroll: bool,
    /// Turn count as of the last render, so a new turn can be detected.
    last_turn_count: usize,
    /// Where this session's live `EngagementState` is published while a turn
    /// is pending — see `poll_agent`. Shared across every chat window and the
    /// Action panel's auto-runs, so the status bar can read one source.
    engagement: SharedEngagementRegistry,
    /// True when this view is embedded inside another panel (e.g. the visual
    /// design panel) rather than owning its own OS window — `close()` must
    /// then leave the window alone and let the embedding panel handle it.
    embedded: bool,
    _poll_task: gpui::Task<()>,
}

impl InteractiveAgentView {
    pub fn new(
        node_id: String,
        session_run_id: String,
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
        workspace_cwd: PathBuf,
        window_control: InteractiveAgentWindowControl,
        // Assembled app context, sent once ahead of the session's first message.
        initial_context: Option<String>,
        // When set, submitted as the first turn's message automatically
        // instead of waiting on the user to type one (e.g. the implementation
        // session's "Go implement this.").
        auto_submit_message: Option<String>,
        settings: TodSettings,
        engagement: SharedEngagementRegistry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // This window's own live turns start empty every time it opens —
        // prior-session history is loaded separately into `history` below.
        let conversation = Vec::new();

        // The run records the platform/model/effort it was started with; a run
        // without that record follows the "Chat with agent" settings.
        let node_title = fleet
            .get_node(&node_id)
            .ok()
            .flatten()
            .map(|node| node.title)
            .unwrap_or_default();
        let run = fleet.get_run(&session_run_id).ok().flatten();
        let stored_transcript = run.as_ref().and_then(run_transcript::stored);
        let launch = run
            .as_ref()
            .and_then(|run| run.launch_options())
            .unwrap_or_else(|| settings.launch_options_for(AgentRole::Chat));
        let session_name = run
            .as_ref()
            .and_then(|run| run.session_name.clone())
            .unwrap_or_else(|| format!("Session {}", run.as_ref().map_or(0, |run| run.run_number)));
        let agent_session_id = run.and_then(|run| run.agent_session_id);

        let history = match (&stored_transcript, &agent_session_id) {
            (Some(transcript), _) => TranscriptHistory::Loaded(transcript.to_text()),
            (None, Some(_)) => TranscriptHistory::Loading,
            (None, None) => TranscriptHistory::NotNeeded,
        };

        let prompt_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .placeholder("Enter to edit · Ctrl+Enter to submit")
        });

        let poll_entity = cx.weak_entity();
        let _poll_task = cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let _ = poll_entity.update(cx, |this, cx| {
                    if this.pending.is_some() {
                        this.poll_agent(cx);
                    }
                });
            }
        });

        // A resumed session was opened with its context before, and its
        // agent-side session still holds it.
        let context_prefix = if agent_session_id.is_none() {
            initial_context
        } else {
            None
        };

        if matches!(history, TranscriptHistory::Loading) {
            spawn_history_fetch(fleet.clone(), session_run_id.clone(), cx);
        }

        let view = Self {
            node_id,
            node_title,
            session_run_id,
            fleet,
            agent,
            workspace_cwd,
            settings,
            platform: platform_storage(launch.platform).to_string(),
            model: launch.model,
            effort: launch.effort,
            window_control,
            prompt_input,
            conversation,
            history,
            pending: None,
            activity: None,
            status_line: "Ready".into(),
            error_banner: None,
            poll_lock_misses: 0,
            focus_handle: cx.focus_handle(),
            focus_stop: InteractiveAgentStop::Prompt,
            // Start in edit mode so the window opens with the caret already
            // in the prompt box, ready for immediate typing.
            prompt_editing: true,
            session_name,
            agent_session_id,
            context_prefix,
            context_expanded: false,
            scroll_handle: gpui::ScrollHandle::new(),
            auto_scroll: true,
            last_turn_count: 0,
            embedded: false,
            engagement,
            _poll_task,
        };
        let is_first_message = view.conversation.is_empty() && view.agent_session_id.is_none();
        if let Some(message) = auto_submit_message.filter(|_| is_first_message) {
            // Auto-submit on the next frame, same timing as the focus-into-prompt
            // path below, since submit needs a live window.
            cx.defer_in(window, move |this, window, cx| {
                this.prompt_input.update(cx, |input, cx| {
                    input.set_value(&message, window, cx);
                });
                this.submit_prompt(window, cx);
            });
        } else {
            // Focus straight into the prompt box so the window opens ready for
            // typing, without waiting on a click or Enter to enter edit mode.
            cx.defer_in(window, |this, window, cx| {
                this.prompt_input.update(cx, |input, cx| {
                    input.focus(window, cx);
                });
            });
        }
        view
    }

    /// Mark this view as embedded inside another panel rather than owning its
    /// own OS window (see the `embedded` field doc).
    pub fn with_embedded(mut self, embedded: bool) -> Self {
        self.embedded = embedded;
        self
    }

    fn text_editing(&self) -> bool {
        self.prompt_editing
    }

    /// Keyboard stops, top to bottom. The context panel only takes part while
    /// it is on screen.
    fn stops(&self) -> Vec<InteractiveAgentStop> {
        let mut stops = Vec::with_capacity(3);
        if self.context_prefix.is_some() {
            stops.push(InteractiveAgentStop::Context);
        }
        stops.push(InteractiveAgentStop::Prompt);
        stops.push(InteractiveAgentStop::Submit);
        stops
    }

    fn stop_focused(&self, stop: InteractiveAgentStop) -> bool {
        self.focus_stop == stop && !self.text_editing()
            || (stop == InteractiveAgentStop::Prompt && self.prompt_editing)
    }

    fn move_stop(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() || self.in_flight() {
            return;
        }
        let stops = self.stops();
        let idx = stops
            .iter()
            .position(|stop| *stop == self.focus_stop)
            .unwrap_or(0) as i32;
        let next = ((idx + delta).rem_euclid(stops.len() as i32)) as usize;
        self.focus_stop = stops[next];
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn enter_prompt_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.in_flight() {
            return;
        }
        self.focus_stop = InteractiveAgentStop::Prompt;
        self.prompt_editing = true;
        cx.notify();
        let input = self.prompt_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    fn exit_prompt_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.prompt_editing {
            return;
        }
        self.prompt_editing = false;
        self.focus_stop = InteractiveAgentStop::Prompt;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn activate_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() {
            return;
        }
        match self.focus_stop {
            InteractiveAgentStop::Context => self.toggle_context(cx),
            InteractiveAgentStop::Prompt if !self.in_flight() => self.enter_prompt_edit(window, cx),
            InteractiveAgentStop::Submit if !self.in_flight() => self.submit_prompt(window, cx),
            _ => {}
        }
    }

    fn toggle_context(&mut self, cx: &mut Context<Self>) {
        self.context_expanded = !self.context_expanded;
        cx.notify();
    }

    fn handle_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompt_editing {
            self.exit_prompt_edit(window, cx);
            return;
        }
        self.close(window, cx);
    }

    fn in_flight(&self) -> bool {
        self.pending.is_some()
    }

    /// What opens the agent session — the app context. Only the session's
    /// first message carries it; the agent keeps it from then on.
    fn opening(&self) -> Option<SessionOpening> {
        let first_message = self.conversation.is_empty() && self.agent_session_id.is_none();
        first_message.then(|| SessionOpening {
            context: Some(tod_core::codebase_rules::with_codebase_rules(
                self.context_prefix.clone().unwrap_or_default(),
                &self.workspace_cwd,
            ))
            .filter(|context| !context.is_empty()),
        })
    }

    /// The platform/model/effort this session was started with.
    fn launch_options(&self) -> tod_store::AgentLaunchOptions {
        tod_store::AgentLaunchOptions {
            platform: parse_platform(&self.platform).unwrap_or(self.settings.agent_platform),
            model: self.model.clone(),
            effort: self.effort.clone(),
        }
    }

    fn fail_submit(&mut self, message: String, cx: &mut Context<Self>) {
        self.pending = None;
        self.error_banner = Some(message);
        self.status_line = "Submit failed".into();
        cx.notify();
    }

    fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.in_flight() {
            return;
        }
        let text = self
            .prompt_input
            .read(cx)
            .text()
            .to_string()
            .trim()
            .to_string();
        if text.is_empty() {
            return;
        }

        self.prompt_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });

        let session_run_id = self.session_run_id.clone();

        self.error_banner = None;
        self.status_line = "Sending…".into();
        self.activity = None;
        self.pending = Some(PendingRun {
            run_id: None,
            user_text: text.clone(),
        });

        let provider_run = match self.agent.lock() {
            Ok(mut provider) => {
                let options = self.launch_options();
                provider.send_session_turn(SessionTurn {
                    key: session_run_id,
                    owner_id: self.node_id.clone(),
                    title: self.session_name.clone(),
                    cwd: self.workspace_cwd.clone(),
                    options,
                    resume_session_id: self.agent_session_id.clone(),
                    opening: self.opening(),
                    message: text,
                    purpose: tod_agent::SessionPurpose::Chat,
                    env: Vec::new(),
                    environment: Default::default(),
                })
            }
            Err(_) => Err(anyhow::anyhow!("Agent busy — try again shortly")),
        };

        match provider_run {
            Ok(handle) => {
                if let Some(pending) = self.pending.as_mut() {
                    pending.run_id = Some(handle.id);
                }
                self.status_line = "Agent thinking…".into();
            }
            Err(err) => {
                self.fail_submit(format!("Launch agent failed: {err:#}"), cx);
            }
        }
        cx.notify();
    }

    fn poll_agent(&mut self, cx: &mut Context<Self>) {
        let Some(run_id) = self.pending.as_ref().and_then(|p| p.run_id) else {
            return;
        };
        let Ok(mut agent) = self.agent.try_lock() else {
            self.poll_lock_misses = self.poll_lock_misses.saturating_add(1);
            if self.poll_lock_misses == 20 {
                self.status_line = "Waiting for agent lock (another view may be using it)…".into();
                cx.notify();
            }
            return;
        };
        self.poll_lock_misses = 0;
        let Some(state) = agent.poll_run(run_id) else {
            return;
        };
        let agent_session_id = agent.session_id(&self.session_run_id);
        drop(agent);

        let PendingRun { user_text, .. } = match self.pending.take() {
            Some(p) => p,
            None => return,
        };

        match state {
            AgentRunState::InFlight(activity) => {
                self.status_line = activity.clone().unwrap_or_else(|| "Agent thinking…".into());
                self.activity = activity;
                self.pending = Some(PendingRun {
                    run_id: Some(run_id),
                    user_text,
                });
                self.set_engagement(EngagementState::WaitingOnAgent);
            }
            AgentRunState::NeedsPermission(request) => {
                self.status_line = "Waiting for permission…".into();
                crate::ui::agent_permission::queue_permission_request(self.agent.clone(), request);
                self.pending = Some(PendingRun {
                    run_id: Some(run_id),
                    user_text,
                });
                self.set_engagement(EngagementState::WaitingOnUser);
            }
            AgentRunState::Success(response) => {
                let assistant = response.unwrap_or_default();
                self.conversation
                    .push((user_text.clone(), assistant.clone()));
                self.status_line = "Agent replied".into();
                self.error_banner = None;
                self.activity = None;
                self.record_agent_session_id(agent_session_id);
                self.clear_engagement();
            }
            AgentRunState::Failure(message) => {
                self.error_banner = Some(message);
                self.status_line = "Agent run failed".into();
                self.activity = None;
                self.clear_engagement();
            }
        }
        cx.notify();
    }

    /// Publish this session's live engagement state — see the `engagement`
    /// field doc. Keyed by the fleet run id, not the provider's `RunId`, so
    /// the status bar can key on the same id `ActionPanelView` uses.
    fn set_engagement(&self, state: EngagementState) {
        if let Ok(mut registry) = self.engagement.lock() {
            registry.insert(self.session_run_id.clone(), state);
        }
    }

    fn clear_engagement(&self) {
        if let Ok(mut registry) = self.engagement.lock() {
            registry.remove(&self.session_run_id);
        }
    }

    /// Persist the agent-side session id when the agent first reports it (or
    /// when a resume landed on a different one).
    fn record_agent_session_id(&mut self, agent_session_id: Option<String>) {
        let Some(agent_session_id) = agent_session_id else {
            return;
        };
        if self.agent_session_id.as_deref() == Some(agent_session_id.as_str()) {
            return;
        }
        if let Err(err) = self.fleet.enqueue(FleetMutation::SetAgentRunSessionId {
            run_id: self.session_run_id.clone(),
            agent_session_id: agent_session_id.clone(),
        }) {
            self.error_banner = Some(format!("Fleet: {err}"));
            return;
        }
        let _ = self.fleet.writer().flush();
        self.agent_session_id = Some(agent_session_id);
    }

    fn close(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        self.window_control.release_session(&self.session_run_id);
        self.clear_engagement();
        if !self.embedded {
            window.remove_window();
        }
    }
}

/// A "Label value" pair in the chat header, so what the session runs with is
/// visible at a glance.
fn render_header_field(
    id: &'static str,
    label: &'static str,
    value: impl Into<gpui::SharedString>,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    h_flex()
        .gap_1()
        .items_center()
        .child(div().text_xs().text_color(muted).child(label))
        .child(
            crate::ui::selectable_text::selectable_text(id, value, window, cx)
                .text_sm()
                .font_semibold()
                .text_color(foreground),
        )
}

fn render_user_panel(
    turn_ix: usize,
    text: impl Into<gpui::SharedString>,
    border: gpui::Hsla,
    panel_bg: gpui::Hsla,
    label_color: gpui::Hsla,
    foreground: gpui::Hsla,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let text = text.into();
    v_flex()
        .id(("interactive-agent-user", turn_ix))
        .gap_1()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(panel_bg)
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(label_color)
                .child("You"),
        )
        .child(
            // Messages often carry markdown (lists, pasted notes, code); it
            // reads better rendered than as source.
            selectable_markdown(("interactive-agent-user-text", turn_ix), text, window, cx)
                .text_sm()
                .text_color(foreground),
        )
}

fn render_agent_panel(
    turn_ix: usize,
    text: impl Into<gpui::SharedString>,
    border: gpui::Hsla,
    panel_bg: gpui::Hsla,
    label_color: gpui::Hsla,
    foreground: gpui::Hsla,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let text = text.into();
    v_flex()
        .id(("interactive-agent-agent", turn_ix))
        .gap_1()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(panel_bg)
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(label_color)
                .child("Agent"),
        )
        .child(
            selectable_markdown(("interactive-agent-agent-text", turn_ix), text, window, cx)
                .text_sm()
                .text_color(foreground),
        )
}

fn render_agent_thinking_panel(
    turn_ix: usize,
    activity: Option<&str>,
    border: gpui::Hsla,
    panel_bg: gpui::Hsla,
    label_color: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
    // One overwriting line rather than an accumulating log: only the latest
    // activity matters to someone watching a run in progress.
    let label = activity.unwrap_or("Thinking…").to_string();
    v_flex()
        .id(("interactive-agent-agent", turn_ix))
        .gap_1()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(panel_bg)
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(label_color)
                .child("Agent"),
        )
        .child(
            div()
                .id(("interactive-agent-activity", turn_ix))
                .text_sm()
                .text_color(muted)
                .italic()
                .child(label),
        )
}

/// Read a resumed session's full transcript in the background, so it never
/// blocks the window, and store it once it lands.
fn spawn_history_fetch(
    fleet: Arc<FleetStore>,
    run_id: String,
    cx: &mut Context<InteractiveAgentView>,
) {
    let entity = cx.weak_entity();
    cx.spawn(async move |_, cx| {
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = fleet.get_run(&run_id).and_then(|run| match run {
                Some(run) => run_transcript::capture(&fleet, &run),
                None => Ok(None),
            });
            let _ = tx.send_blocking(
                result
                    .map(|read| {
                        read.map(|read| read.transcript.to_text())
                            .unwrap_or_default()
                    })
                    .map_err(|err| err.to_string()),
            );
        });
        let result = rx
            .recv()
            .await
            .unwrap_or_else(|_| Err("transcript fetch thread panicked".into()));
        let _ = entity.update(cx, |this, cx| {
            this.history = match result {
                Ok(text) => TranscriptHistory::Loaded(text),
                Err(err) => TranscriptHistory::FetchFailed(err),
            };
            cx.notify();
        });
    })
    .detach();
}

impl Focusable for InteractiveAgentView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for InteractiveAgentView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.poll_agent(cx);
        key_context::set_input_tab_stop(&self.prompt_input, self.prompt_editing, cx);
        if !self.prompt_editing
            && self
                .prompt_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            self.enter_prompt_edit(window, cx);
        }

        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let panel_bg = theme.muted;
        let user_label = theme.primary;
        let agent_label = theme.accent;
        let in_flight = self.in_flight();
        let conversation = self.conversation.clone();
        let pending_user = self.pending.as_ref().map(|p| p.user_text.clone());
        let show_empty = conversation.is_empty() && pending_user.is_none();
        let pending_turn_ix = conversation.len();
        let turn_count = conversation.len() + pending_user.is_some() as usize;
        if turn_count != self.last_turn_count {
            self.last_turn_count = turn_count;
            if self.auto_scroll {
                self.scroll_handle.scroll_to_bottom();
            }
        }
        const PROMPT_ROWS: f32 = 4.;
        let prompt_height = window.line_height() * PROMPT_ROWS;
        let list_active_border = theme.list_active_border;
        let background = theme.background;
        let prompt_focused = self.stop_focused(InteractiveAgentStop::Prompt);
        let submit_focused = self.stop_focused(InteractiveAgentStop::Submit);
        let platform_label = parse_platform(&self.platform)
            .map_or_else(|| self.platform.clone(), |p| p.label().to_string());

        // The context leads the prompt body, so it leads the transcript too —
        // collapsed, because it is rarely what the reader came for.
        let context_panel = self.context_prefix.clone().map(|context| {
            let expanded = self.context_expanded;
            let focused = self.stop_focused(InteractiveAgentStop::Context);
            let summary = if expanded {
                "click to collapse".to_string()
            } else {
                let lines = context.lines().count();
                let unit = if lines == 1 { "line" } else { "lines" };
                let when = if show_empty {
                    "sent ahead of your first prompt"
                } else {
                    "leads every session"
                };
                format!("{lines} {unit} · {when}")
            };
            let body = expanded.then(|| {
                crate::ui::selectable_text::selectable_text(
                    "interactive-agent-context-text",
                    context,
                    window,
                    cx,
                )
                .text_sm()
                .text_color(foreground)
            });
            v_flex()
                .id("interactive-agent-context")
                .gap_2()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(if focused { list_active_border } else { border })
                .bg(panel_bg)
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .cursor_pointer()
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.focus_stop = InteractiveAgentStop::Context;
                                this.toggle_context(cx);
                            }),
                        )
                        .child(div().text_xs().text_color(muted).child(if expanded {
                            "▾"
                        } else {
                            "▸"
                        }))
                        .child(
                            div()
                                .text_xs()
                                .font_semibold()
                                .text_color(user_label)
                                .child("App context"),
                        )
                        .child(div().text_xs().text_color(muted).child(summary)),
                )
                .children(body)
                .into_any_element()
        });

        let history_panel = match &self.history {
            TranscriptHistory::NotNeeded => None,
            TranscriptHistory::Loading => Some(
                h_flex()
                    .id("interactive-agent-history-loading")
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(panel_bg)
                    .child(gpui_component::spinner::Spinner::new())
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child("Loading prior transcript…"),
                    )
                    .into_any_element(),
            ),
            TranscriptHistory::Loaded(text) => Some(
                v_flex()
                    .id("interactive-agent-history")
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(panel_bg)
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(muted)
                            .child("Prior session"),
                    )
                    .child(crate::ui::selectable_text::selectable_text(
                        "interactive-agent-history-text",
                        text.clone(),
                        window,
                        cx,
                    ))
                    .into_any_element(),
            ),
            TranscriptHistory::FetchFailed(err) => Some(
                div()
                    .id("interactive-agent-history-error")
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(panel_bg)
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(format!("Couldn't load prior transcript: {err}")),
                    )
                    .into_any_element(),
            ),
        };

        div()
            .key_context(INTERACTIVE_AGENT_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(background)
            .v_flex()
            .on_action(
                cx.listener(|this, _: &SubmitInteractivePrompt, window, cx| {
                    this.submit_prompt(window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &InteractiveAgentStopUp, window, cx| {
                this.move_stop(-1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &InteractiveAgentStopDown, window, cx| {
                this.move_stop(1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &InteractiveAgentActivate, window, cx| {
                this.activate_stop(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &InteractiveAgentEscape, window, cx| {
                this.handle_escape(window, cx);
                cx.stop_propagation();
            }))
            .child(
                v_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .flex_wrap()
                            .items_center()
                            .gap_x_4()
                            .gap_y_1()
                            .child(render_header_field(
                                "interactive-agent-header-task",
                                "Task",
                                self.node_title.clone(),
                                foreground,
                                muted,
                                window,
                                cx,
                            ))
                            .child(render_header_field(
                                "interactive-agent-header-platform",
                                "Platform",
                                platform_label,
                                foreground,
                                muted,
                                window,
                                cx,
                            ))
                            .child(render_header_field(
                                "interactive-agent-header-model",
                                "Model",
                                self.model.clone(),
                                foreground,
                                muted,
                                window,
                                cx,
                            ))
                            .child(render_header_field(
                                "interactive-agent-header-effort",
                                "Effort",
                                self.effort.clone(),
                                foreground,
                                muted,
                                window,
                                cx,
                            )),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .child(
                                crate::ui::selectable_text::selectable_text(
                                    "interactive-agent-info-line",
                                    format!(
                                        "{} · {}",
                                        self.session_name,
                                        self.workspace_cwd.display()
                                    ),
                                    window,
                                    cx,
                                )
                                .text_xs()
                                .text_color(muted),
                            )
                            .child(
                                Checkbox::new("interactive-agent-auto-scroll")
                                    .label("Auto Scroll")
                                    .checked(self.auto_scroll)
                                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                        this.auto_scroll = *checked;
                                        if this.auto_scroll {
                                            this.scroll_handle.scroll_to_bottom();
                                        }
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .when_some(self.error_banner.clone(), |el, msg| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .px_4()
                        .py_2()
                        .bg(gpui::red())
                        .border_b_1()
                        .border_color(border)
                        .child(
                            crate::ui::selectable_text::selectable_text(
                                "interactive-agent-error-banner",
                                msg,
                                window,
                                cx,
                            )
                            .text_color(gpui::white()),
                        ),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_1()
                    .p_4()
                    .pb_0()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(muted)
                            .child("Replies"),
                    )
                    .child(
                        div()
                            .id("interactive-agent-transcript")
                            .flex_1()
                            .min_h_0()
                            .track_scroll(&self.scroll_handle)
                            .vertical_scrollbar(&self.scroll_handle)
                            .overflow_y_scroll()
                            .v_flex()
                            .gap_3()
                            .children(context_panel)
                            .children(history_panel)
                            .when(show_empty, |el| {
                                el.child(
                                    div()
                                        .text_sm()
                                        .text_color(muted)
                                        .child("Agent replies will appear here…"),
                                )
                            })
                            .children(conversation.iter().enumerate().map(
                                |(turn_ix, (user, assistant))| {
                                    v_flex()
                                        .id(("interactive-agent-turn", turn_ix))
                                        .gap_2()
                                        .child(render_user_panel(
                                            turn_ix,
                                            user.clone(),
                                            border,
                                            panel_bg,
                                            user_label,
                                            foreground,
                                            window,
                                            cx,
                                        ))
                                        .child(render_agent_panel(
                                            turn_ix,
                                            assistant.clone(),
                                            border,
                                            panel_bg,
                                            agent_label,
                                            foreground,
                                            window,
                                            cx,
                                        ))
                                },
                            ))
                            .when_some(pending_user, |el, user_text| {
                                el.child(
                                    v_flex()
                                        .id(("interactive-agent-turn", pending_turn_ix))
                                        .gap_2()
                                        .child(render_user_panel(
                                            pending_turn_ix,
                                            user_text,
                                            border,
                                            panel_bg,
                                            user_label,
                                            foreground,
                                            window,
                                            cx,
                                        ))
                                        .child(render_agent_thinking_panel(
                                            pending_turn_ix,
                                            self.activity.as_deref(),
                                            border,
                                            panel_bg,
                                            agent_label,
                                            muted,
                                        )),
                                )
                            }),
                    ),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .p_4()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(muted)
                            .child("Prompt"),
                    )
                    .child(
                        div()
                            .w_full()
                            .rounded_md()
                            .cursor_text()
                            .when(prompt_focused, |el| {
                                el.border_1().border_color(list_active_border)
                            })
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    if !this.in_flight() && !this.prompt_editing {
                                        this.enter_prompt_edit(window, cx);
                                    }
                                }),
                            )
                            .child(
                                Textarea::new(&self.prompt_input)
                                    .disabled(in_flight || !self.prompt_editing)
                                    .w_full()
                                    .h(prompt_height),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new("interactive-agent-submit")
                                    .label("Submit prompt")
                                    .primary()
                                    .compact()
                                    .selected(submit_focused)
                                    .disabled(in_flight)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit_prompt(window, cx);
                                    })),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("↑↓ control · Enter activate · Esc exit edit/close · Ctrl+Enter submit"),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        crate::ui::selectable_text::selectable_text(
                            "interactive-agent-status-line",
                            self.status_line.clone(),
                            window,
                            cx,
                        )
                        .text_xs()
                        .text_color(muted),
                    ),
            )
    }
}

pub fn register_interactive_agent_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(INTERACTIVE_AGENT_CONTEXT));
    let input_context = Some(key_context::including_input(INTERACTIVE_AGENT_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("up", InteractiveAgentStopUp, context),
        KeyBinding::new("down", InteractiveAgentStopDown, context),
        KeyBinding::new("enter", InteractiveAgentActivate, context),
        KeyBinding::new("space", InteractiveAgentActivate, context),
        KeyBinding::new("escape", InteractiveAgentEscape, context),
        KeyBinding::new("escape", InteractiveAgentEscape, input_context),
    ]);
    let submit_bindings = [
        "ctrl-enter",
        #[cfg(target_os = "macos")]
        "cmd-enter",
    ];
    for keystroke in submit_bindings {
        cx.bind_keys([
            gpui::KeyBinding::new(
                keystroke,
                SubmitInteractivePrompt,
                Some(key_context::including_input(INTERACTIVE_AGENT_CONTEXT)),
            ),
            gpui::KeyBinding::new(
                keystroke,
                SubmitInteractivePrompt,
                Some(INTERACTIVE_AGENT_CONTEXT),
            ),
        ]);
    }
}
