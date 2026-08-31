//! Interactive fleet-agent chat window — prompt in, replies out.

use crate::app::InteractiveAgentWindowControl;
use crate::fleet::repos::transcript::TranscriptTurn;
use crate::fleet::{FleetMutation, FleetStore};
use crate::interview::agent::{AgentRunState, RunId, SharedAgent};
use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::key_context;
use crate::ui::selectable_text::{selectable_markdown, selectable_text};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, Styled, Timer, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, StyledExt, h_flex, v_flex};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const INTERACTIVE_AGENT_CONTEXT: &str = "InteractiveAgent";
const POLL_INTERVAL: Duration = Duration::from_millis(300);

actions!(
    interactive_agent,
    [SubmitInteractivePrompt, InteractiveAgentClose]
);

struct PendingRun {
    run_id: Option<RunId>,
    prompt_id: String,
    response_id: String,
    user_text: String,
}

pub struct InteractiveAgentView {
    task_id: String,
    config_id: String,
    session_run_id: String,
    session_number: i64,
    fleet: Arc<FleetStore>,
    agent: SharedAgent,
    workspace_cwd: PathBuf,
    window_control: InteractiveAgentWindowControl,
    prompt_input: Entity<InputState>,
    conversation: Vec<(String, String)>,
    pending: Option<PendingRun>,
    status_line: String,
    error_banner: Option<String>,
    focus_handle: FocusHandle,
    _poll_task: gpui::Task<()>,
}

impl InteractiveAgentView {
    pub fn new(
        task_id: String,
        config_id: String,
        session_run_id: String,
        fleet: Arc<FleetStore>,
        agent: SharedAgent,
        workspace_cwd: PathBuf,
        window_control: InteractiveAgentWindowControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session_number = fleet
            .list_runs_for_config(&config_id)
            .ok()
            .and_then(|runs| {
                runs.iter()
                    .find(|run| run.id == session_run_id)
                    .map(|run| run.run_number)
            })
            .unwrap_or(0);
        let conversation = fleet
            .list_transcript_for_agent(&session_run_id)
            .ok()
            .map(|turns| conversation_from_transcript(&turns))
            .unwrap_or_default();

        let prompt_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(4)
                .placeholder("Enter a prompt… (Ctrl+Enter to submit)")
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

        let mut view = Self {
            task_id,
            config_id,
            session_run_id,
            session_number,
            fleet,
            agent,
            workspace_cwd,
            window_control,
            prompt_input,
            conversation,
            pending: None,
            status_line: "Ready".into(),
            error_banner: None,
            focus_handle: cx.focus_handle(),
            _poll_task,
        };
        view
    }

    fn in_flight(&self) -> bool {
        self.pending.is_some()
    }

    fn build_prompt(&self, new_user_text: &str) -> String {
        if self.conversation.is_empty() {
            return new_user_text.to_string();
        }
        let mut transcript = String::new();
        for (user, assistant) in &self.conversation {
            transcript.push_str(&format!("User:\n{user}\n\nAssistant:\n{assistant}\n\n"));
        }
        transcript.push_str(&format!("User:\n{new_user_text}"));
        transcript
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

        let prompt_body = self.build_prompt(&text);
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
            content: prompt_body.clone(),
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
                provider.start_fleet_agent(&self.config_id, self.workspace_cwd.clone(), prompt_body)
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
        let Some(state) = self
            .agent
            .try_lock()
            .ok()
            .and_then(|mut agent| agent.poll_run(run_id))
        else {
            return;
        };

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
            }
            AgentRunState::Failure(message) => {
                self.error_banner = Some(message);
                self.status_line = "Agent run failed".into();
            }
        }
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        self.window_control.remove_handle(&self.session_run_id);
        window.remove_window();
    }
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
            selectable_text(("interactive-agent-user-text", turn_ix), text, window, cx)
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

        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let panel_bg = theme.muted;
        let user_label = theme.primary;
        let agent_label = theme.accent;
        let in_flight = self.in_flight();
        let title = format!("Session {} · {}", self.session_number, self.config_id);
        let conversation = self.conversation.clone();
        let pending_user = self.pending.as_ref().map(|p| p.user_text.clone());
        let show_empty = conversation.is_empty() && pending_user.is_none();
        let pending_turn_ix = conversation.len();

        div()
            .key_context(INTERACTIVE_AGENT_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .v_flex()
            .on_action(
                cx.listener(|this, _: &SubmitInteractivePrompt, window, cx| {
                    this.submit_prompt(window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &InteractiveAgentClose, window, cx| {
                this.close(window, cx);
            }))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(div().text_sm().font_semibold().child(title))
                            .child(div().text_xs().text_color(muted).child(format!(
                                "Task {} · {}",
                                self.task_id,
                                self.workspace_cwd.display()
                            ))),
                    )
                    .child(chrome_control_with_shortcut(
                        Button::new("interactive-agent-close")
                            .label("Close")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close(window, cx);
                            })),
                        window,
                        &InteractiveAgentClose,
                        INTERACTIVE_AGENT_CONTEXT,
                        cx,
                    )),
            )
            .when_some(self.error_banner.clone(), |el, msg| {
                el.child(
                    div()
                        .px_4()
                        .py_2()
                        .bg(gpui::red())
                        .text_color(gpui::white())
                        .border_b_1()
                        .border_color(border)
                        .child(msg),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .gap_3()
                    .p_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
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
                                    .min_h(px(160.))
                                    .overflow_y_scrollbar()
                                    .v_flex()
                                    .gap_3()
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
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(muted)
                                    .child("Prompt"),
                            )
                            .child(
                                div().min_h(px(96.)).child(
                                    Input::new(&self.prompt_input).disabled(in_flight).w_full(),
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
                                            .disabled(in_flight)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.submit_prompt(window, cx);
                                            })),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child("Ctrl+Enter to submit · Enter for newline"),
                                    ),
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
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(self.status_line.clone()),
                    ),
            )
    }
}

pub fn register_interactive_agent_keyboard_bindings(cx: &mut App) {
    key_context::bind_panel_escape(cx, InteractiveAgentClose, INTERACTIVE_AGENT_CONTEXT);
    cx.bind_keys([
        gpui::KeyBinding::new(
            "ctrl-enter",
            SubmitInteractivePrompt,
            Some(key_context::including_input(INTERACTIVE_AGENT_CONTEXT)),
        ),
        gpui::KeyBinding::new(
            "ctrl-enter",
            SubmitInteractivePrompt,
            Some(INTERACTIVE_AGENT_CONTEXT),
        ),
    ]);
}
