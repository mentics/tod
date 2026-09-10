//! Interactive fleet-agent chat window — prompt in, replies out.

use crate::app::InteractiveAgentWindowControl;
use crate::interview::agent::{AgentRunState, RunId, SharedAgent};
use crate::ui::key_context;
use crate::ui::selectable_text::selectable_markdown;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Render, Styled, Timer, Window, actions, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, Selectable, StyledExt, h_flex, v_flex};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tod_agent::{SessionOpening, SessionTurn};
use tod_store::fleet::repos::transcript::TranscriptTurn;
use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::{AgentLaunchOptions, AgentPlatform, parse_platform, platform_storage};

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
    prompt_id: String,
    response_id: String,
    user_text: String,
}

pub struct InteractiveAgentView {
    config_id: String,
    session_run_id: String,
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    workspace_cwd: PathBuf,
    platform: String,
    model: String,
    effort: String,
    window_control: InteractiveAgentWindowControl,
    prompt_input: Entity<InputState>,
    conversation: Vec<(String, String)>,
    pending: Option<PendingRun>,
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
    _poll_task: gpui::Task<()>,
}

impl InteractiveAgentView {
    pub fn new(
        config_id: String,
        session_run_id: String,
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
        workspace_cwd: PathBuf,
        window_control: InteractiveAgentWindowControl,
        // Assembled app context, sent once ahead of the session's first message.
        initial_context: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let conversation = fleet
            .list_transcript_for_agent(&session_run_id)
            .ok()
            .map(|turns| conversation_from_transcript(&turns))
            .unwrap_or_default();

        let launch = fleet
            .get_agent(&config_id)
            .ok()
            .flatten()
            .map(|row| row.launch_options())
            .unwrap_or_else(|| AgentLaunchOptions::for_platform(AgentPlatform::Claude));

        let run = fleet.get_run(&session_run_id).ok().flatten();
        let session_name = run
            .as_ref()
            .and_then(|run| run.session_name.clone())
            .unwrap_or_else(|| format!("Session {}", run.as_ref().map_or(0, |run| run.run_number)));
        let agent_session_id = run.and_then(|run| run.agent_session_id);

        let prompt_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(4)
                .placeholder("Enter to edit · Ctrl+Enter to submit")
        });

        let poll_entity = cx.weak_entity();
        let _poll_task = cx.spawn(async move |_, cx| {
            loop {
                Timer::after(POLL_INTERVAL).await;
                let _ = poll_entity.update(cx, |this, cx| {
                    if this.pending.is_some() {
                        this.poll_agent(cx);
                    }
                });
            }
        });

        // A session that already has turns was opened with its context, and its
        // agent session still holds it.
        let context_prefix = if conversation.is_empty() {
            initial_context
        } else {
            None
        };
        let view = Self {
            config_id,
            session_run_id,
            fleet,
            agent,
            workspace_cwd,
            platform: platform_storage(launch.platform).to_string(),
            model: launch.model,
            effort: launch.effort,
            window_control,
            prompt_input,
            conversation,
            pending: None,
            status_line: "Ready".into(),
            error_banner: None,
            poll_lock_misses: 0,
            focus_handle: cx.focus_handle(),
            focus_stop: InteractiveAgentStop::Prompt,
            prompt_editing: false,
            session_name,
            agent_session_id,
            context_prefix,
            context_expanded: false,
            _poll_task,
        };
        view
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
        self.focus_handle.focus(window);
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
        self.focus_handle.focus(window);
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

    /// What opens the agent session — its name and the app context. Only the
    /// session's first message carries it; the agent keeps it from then on.
    fn opening(&self) -> Option<SessionOpening> {
        let first_message = self.conversation.is_empty() && self.agent_session_id.is_none();
        first_message.then(|| SessionOpening {
            title: self.session_name.clone(),
            context: self.context_prefix.clone(),
        })
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

        let prompt_id = uuid::Uuid::new_v4().to_string();
        let response_id = uuid::Uuid::new_v4().to_string();
        let session_run_id = self.session_run_id.clone();

        self.error_banner = None;
        self.status_line = "Sending…".into();
        self.pending = Some(PendingRun {
            run_id: None,
            prompt_id: prompt_id.clone(),
            response_id: response_id.clone(),
            user_text: text.clone(),
        });

        if let Err(err) = self.fleet.enqueue(FleetMutation::SendPrompt {
            id: prompt_id.clone(),
            agent_id: self.config_id.clone(),
            content: text.clone(),
            run_id: Some(session_run_id.clone()),
        }) {
            self.fail_submit(format!("Fleet: {err}"), cx);
            return;
        }
        if let Err(err) = self.fleet.writer().flush() {
            self.fail_submit(format!("Fleet: {err}"), cx);
            return;
        }

        let provider_run = match self.agent.lock() {
            Ok(mut provider) => {
                let options = self
                    .fleet
                    .get_agent(&self.config_id)
                    .ok()
                    .flatten()
                    .map(|row| row.launch_options())
                    .unwrap_or_else(|| {
                        AgentLaunchOptions::from_settings(
                            parse_platform(&self.platform).unwrap_or(AgentPlatform::Claude),
                            self.model.clone(),
                            self.effort.clone(),
                        )
                    });
                provider.send_session_turn(SessionTurn {
                    key: session_run_id,
                    agent_config_id: self.config_id.clone(),
                    cwd: self.workspace_cwd.clone(),
                    options,
                    resume_session_id: self.agent_session_id.clone(),
                    opening: self.opening(),
                    message: text,
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

        let PendingRun {
            prompt_id,
            response_id,
            user_text,
            ..
        } = match self.pending.take() {
            Some(p) => p,
            None => return,
        };

        match state {
            AgentRunState::InFlight => {
                self.pending = Some(PendingRun {
                    run_id: Some(run_id),
                    prompt_id,
                    response_id,
                    user_text,
                });
            }
            AgentRunState::Success(response) => {
                let assistant = response.unwrap_or_default();
                self.conversation
                    .push((user_text.clone(), assistant.clone()));
                if let Err(err) = self.fleet.enqueue(FleetMutation::CompleteResponse {
                    response_id,
                    agent_id: self.config_id.clone(),
                    content: assistant,
                    prompt_id,
                    run_id: Some(self.session_run_id.clone()),
                }) {
                    self.error_banner = Some(format!("Fleet: {err}"));
                } else {
                    let _ = self.fleet.writer().flush();
                }
                self.status_line = "Agent replied".into();
                self.error_banner = None;
                self.record_agent_session_id(agent_session_id);
            }
            AgentRunState::Failure(message) => {
                self.error_banner = Some(message);
                self.status_line = "Agent run failed".into();
            }
        }
        cx.notify();
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
        window.remove_window();
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
    border: gpui::Hsla,
    panel_bg: gpui::Hsla,
    label_color: gpui::Hsla,
    muted: gpui::Hsla,
) -> impl IntoElement {
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
                .text_sm()
                .text_color(muted)
                .italic()
                .child("Thinking…"),
        )
}

fn conversation_from_transcript(turns: &[TranscriptTurn]) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut i = 0;
    while i < turns.len() {
        if turns[i].kind == "prompt" {
            let user = turns[i].content.clone();
            let assistant = turns
                .get(i + 1)
                .filter(|turn| turn.kind == "response")
                .map(|turn| turn.content.clone())
                .unwrap_or_default();
            if !assistant.is_empty() {
                pairs.push((user, assistant));
                i += 2;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    pairs
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
                                "interactive-agent-header-agent",
                                "Agent",
                                self.config_id.clone(),
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
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .v_flex()
                            .gap_3()
                            .children(context_panel)
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
                                Input::new(&self.prompt_input)
                                    .disabled(in_flight || !self.prompt_editing)
                                    .focus_bordered(self.prompt_editing)
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
