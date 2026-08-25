use crate::interview::agent::{
    AgentProvider, AgentRunState, CursorAcpProvider, DeepDiveContext, RunId,
};
use crate::interview::config::InterviewConfig;
use crate::interview::queue::QueueQuestion;
use crate::interview::{InterviewSession, TodPaths};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, StyledExt, h_flex, v_flex};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepDiveEvent {
    Back,
    UseThis(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
struct ChatTurn {
    id: String,
    role: ChatRole,
    body: String,
}

pub struct DeepDiveView {
    parent: QueueQuestion,
    config: InterviewConfig,
    _session: InterviewSession,
    turns: Vec<ChatTurn>,
    draft_input: Entity<InputState>,
    agent: Arc<Mutex<CursorAcpProvider>>,
    active_run: Option<RunId>,
    selected_turn_id: Option<String>,
    pasted_preview: Option<SharedString>,
    status_line: SharedString,
    error_banner: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl DeepDiveView {
    pub fn new(
        parent: QueueQuestion,
        config: InterviewConfig,
        session: InterviewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
        agent: Arc<Mutex<CursorAcpProvider>>,
    ) -> Self {
        let draft_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(2)
                .placeholder("Message deep-dive agent…")
        });
        Self {
            parent,
            config,
            _session: session,
            turns: Vec::new(),
            draft_input,
            agent,
            active_run: None,
            selected_turn_id: None,
            pasted_preview: None,
            status_line: "Ready".into(),
            error_banner: None,
            focus_handle: cx.focus_handle().tab_stop(true),
        }
    }

    fn poll_agent(&mut self, cx: &mut Context<Self>) {
        let Some(run_id) = self.active_run else {
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
        match state {
            AgentRunState::InFlight => {}
            AgentRunState::Success(response) => {
                self.active_run = None;
                self.error_banner = None;
                self.status_line = "Agent replied".into();
                if let Some(text) = response.filter(|t| !t.trim().is_empty()) {
                    self.turns.push(ChatTurn {
                        id: Uuid::new_v4().to_string(),
                        role: ChatRole::Assistant,
                        body: text,
                    });
                }
            }
            AgentRunState::Failure(message) => {
                self.active_run = None;
                self.error_banner = Some(message.into());
                self.status_line = "Agent run failed".into();
            }
        }
        cx.notify();
    }

    fn send_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_run.is_some() {
            return;
        }
        let text = self.draft_input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        self.turns.push(ChatTurn {
            id: Uuid::new_v4().to_string(),
            role: ChatRole::User,
            body: text.trim().to_string(),
        });
        self.draft_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });

        let context = self.deep_dive_context();
        let conversation = conversation_transcript(&self.turns);
        let cwd = self.config.entity.clone();
        let agent = self.agent.clone();
        match agent.try_lock() {
            Ok(mut provider) => {
                match provider.start_deep_dive_chat(cwd, context, Some(conversation)) {
                    Ok(handle) => {
                        self.active_run = Some(handle.id);
                        self.status_line = "Agent thinking…".into();
                        self.error_banner = None;
                    }
                    Err(err) => {
                        self.error_banner =
                            Some(format!("Failed to start deep-dive agent: {err}").into());
                    }
                }
            }
            Err(_) => {
                self.error_banner =
                    Some("Agent busy (bootstrap in progress) — try again shortly".into());
            }
        }
        cx.notify();
    }

    fn deep_dive_context(&self) -> DeepDiveContext {
        let paths = TodPaths::discover().expect("paths");
        let repo = paths.repo_root();
        DeepDiveContext {
            project: repo
                .file_name()
                .map(|n| n.to_string_lossy().into())
                .unwrap_or_else(|| "project".into()),
            task: self
                .config
                .entity
                .strip_prefix(&repo)
                .unwrap_or(&self.config.entity)
                .to_string_lossy()
                .into(),
            lifecycle_state: "active".into(),
            interview_purpose: self.config.phase.clone(),
            interview_phase: self.config.phase.clone(),
            question_id: self.parent.id.clone(),
            question_body: self.parent.body.clone(),
        }
    }

    fn use_this(&mut self, turn_id: &str, cx: &mut Context<Self>) {
        if let Some(turn) = self.turns.iter().find(|t| t.id == turn_id) {
            if turn.role == ChatRole::Assistant {
                self.selected_turn_id = Some(turn_id.to_string());
                self.pasted_preview = Some(turn.body.clone().into());
                cx.emit(DeepDiveEvent::UseThis(turn.body.clone()));
                cx.notify();
            }
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        cx.emit(DeepDiveEvent::Back);
    }
}

fn conversation_transcript(turns: &[ChatTurn]) -> String {
    turns
        .iter()
        .map(|t| {
            let role = match t.role {
                ChatRole::User => "User",
                ChatRole::Assistant => "Assistant",
            };
            format!("{role}:\n{}\n", t.body)
        })
        .collect()
}

impl Focusable for DeepDiveView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DeepDiveView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.poll_agent(cx);

        let theme = cx.theme();
        let border = theme.border;
        let accent = theme.accent;
        let foreground = theme.foreground;
        let muted = theme.muted_foreground;
        let background = theme.background;
        let in_flight = self.active_run.is_some();

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(background)
            .v_flex()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(foreground)
                                    .child("Interview — deep dive"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("Separate agent chat · Use this pastes into parent Notes · no auto-submit"),
                            ),
                    )
                    .child(
                        Button::new("deep-dive-back")
                            .label("Back to workspace")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.back(cx);
                            })),
                    ),
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
                    .mx_4()
                    .my_3()
                    .max_w(px(720.))
                    .border_1()
                    .border_color(border)
                    .bg(theme.group_box)
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .border_b_1()
                            .border_color(border)
                            .bg(theme.group_box)
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("Parent"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(self.parent.id.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(foreground)
                                    .child(self.parent.short_label.clone()),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("Parent inputs unchanged"),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .p_3()
                            .gap_3()
                            .overflow_y_scrollbar()
                            .children(self.turns.iter().enumerate().map(|(idx, turn)| {
                                let is_assistant = turn.role == ChatRole::Assistant;
                                let is_target =
                                    self.selected_turn_id.as_deref() == Some(turn.id.as_str());
                                let turn_id = turn.id.clone();
                                div()
                                    .id(("deep-dive-turn", idx))
                                    .px_2()
                                    .py_2()
                                    .border_l_2()
                                    .border_color(if is_target {
                                        accent
                                    } else {
                                        gpui::transparent_black()
                                    })
                                    .when(is_target, |el| el.bg(accent.opacity(0.08)))
                                    .v_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_semibold()
                                            .text_color(if is_assistant {
                                                accent
                                            } else {
                                                muted
                                            })
                                            .child(if is_assistant { "Agent" } else { "You" }),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(foreground)
                                            .child(turn.body.clone()),
                                    )
                                    .when(is_assistant, |el| {
                                        el.child(
                                            v_flex()
                                                .gap_2()
                                                .p_2()
                                                .border_1()
                                                .border_color(border)
                                                .bg(background)
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .child("Target for parent answer"),
                                                )
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(foreground)
                                                        .child(turn.body.clone()),
                                                )
                                                .child(
                                                    h_flex()
                                                        .gap_2()
                                                        .items_center()
                                                        .child(
                                                            Button::new(("use-this", idx))
                                                                .label("Use this")
                                                                .when(is_target, |b| b.primary())
                                                                .disabled(!is_assistant)
                                                                .on_click(cx.listener({
                                                                    let turn_id = turn_id.clone();
                                                                    move |this, _, _, cx| {
                                                                        this.use_this(&turn_id, cx);
                                                                    }
                                                                })),
                                                        )
                                                        .when(is_target, |row| {
                                                            row.child(
                                                                div()
                                                                    .text_xs()
                                                                    .text_color(muted)
                                                                    .child("Pasted into parent Notes"),
                                                            )
                                                        }),
                                                ),
                                        )
                                    })
                            })),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .p_3()
                            .border_t_1()
                            .border_color(border)
                            .child(
                                if let Some(preview) = &self.pasted_preview {
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(format!(
                                            "Parent answer preview: {}{}",
                                            preview.chars().take(72).collect::<String>(),
                                            if preview.len() > 72 { "…" } else { "" }
                                        ))
                                } else {
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child("Select a target below an agent turn, then Use this — edits and submit stay on the parent question.")
                                },
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_end()
                                    .child(
                                        div()
                                            .flex_1()
                                            .child(Input::new(&self.draft_input).disabled(in_flight).w_full()),
                                    )
                                    .child(
                                        Button::new("deep-dive-send")
                                            .label("Send")
                                            .primary()
                                            .disabled(in_flight)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.send_message(window, cx);
                                            })),
                                    ),
                            ),
                    ),
            )
            .child(
                h_flex()
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

impl gpui::EventEmitter<DeepDiveEvent> for DeepDiveView {}
