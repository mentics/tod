use crate::interview::agent::{AgentRunState, DeepDiveContext, RunId, SharedAgent};
use crate::interview::config::InterviewConfig;
use crate::interview::queue::QueueQuestion;
use crate::interview::{InterviewSession, TodPaths};
use crate::ui::key_context;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Render, SharedString, Styled, Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, Selectable, StyledExt, h_flex, v_flex};
use tod_store::AgentLaunchOptions;
use uuid::Uuid;

const DEEP_DIVE_CONTEXT: &str = "DeepDive";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeepDiveStop {
    Draft,
    Send,
}

const DEEP_DIVE_STOPS: [DeepDiveStop; 2] = [DeepDiveStop::Draft, DeepDiveStop::Send];

actions!(
    deep_dive,
    [
        DeepDiveStopUp,
        DeepDiveStopDown,
        DeepDiveActivate,
        DeepDiveEscape,
        DeepDiveBack,
    ]
);

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
    agent_config_id: String,
    workspace_cwd: std::path::PathBuf,
    launch_options: AgentLaunchOptions,
    turns: Vec<ChatTurn>,
    draft_input: Entity<InputState>,
    agent: SharedAgent,
    active_run: Option<RunId>,
    selected_turn_id: Option<String>,
    pasted_preview: Option<SharedString>,
    status_line: SharedString,
    error_banner: Option<SharedString>,
    focus_handle: FocusHandle,
    focus_stop: DeepDiveStop,
    draft_editing: bool,
}

impl DeepDiveView {
    pub fn new(
        parent: QueueQuestion,
        config: InterviewConfig,
        session: InterviewSession,
        agent_config_id: String,
        workspace_cwd: std::path::PathBuf,
        launch_options: AgentLaunchOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
        agent: SharedAgent,
    ) -> Self {
        let draft_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(2)
                .placeholder("Enter to edit · Message deep-dive agent…")
        });
        Self {
            parent,
            config,
            _session: session,
            agent_config_id,
            workspace_cwd,
            launch_options,
            turns: Vec::new(),
            draft_input,
            agent,
            active_run: None,
            selected_turn_id: None,
            pasted_preview: None,
            status_line: "Ready".into(),
            error_banner: None,
            focus_handle: cx.focus_handle().tab_stop(true),
            focus_stop: DeepDiveStop::Draft,
            draft_editing: false,
        }
    }

    fn text_editing(&self) -> bool {
        self.draft_editing
    }

    fn in_flight(&self) -> bool {
        self.active_run.is_some()
    }

    fn stop_index(stop: DeepDiveStop) -> usize {
        DEEP_DIVE_STOPS.iter().position(|s| *s == stop).unwrap_or(0)
    }

    fn stop_focused(&self, stop: DeepDiveStop) -> bool {
        self.focus_stop == stop && !self.text_editing()
            || (stop == DeepDiveStop::Draft && self.draft_editing)
    }

    fn move_stop(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() || self.in_flight() {
            return;
        }
        let idx = Self::stop_index(self.focus_stop) as i32;
        let len = DEEP_DIVE_STOPS.len() as i32;
        let next = ((idx + delta).rem_euclid(len)) as usize;
        self.focus_stop = DEEP_DIVE_STOPS[next];
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn enter_draft_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.in_flight() {
            return;
        }
        self.focus_stop = DeepDiveStop::Draft;
        self.draft_editing = true;
        cx.notify();
        let input = self.draft_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    fn exit_draft_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.draft_editing {
            return;
        }
        self.draft_editing = false;
        self.focus_stop = DeepDiveStop::Draft;
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn activate_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_editing() || self.in_flight() {
            return;
        }
        match self.focus_stop {
            DeepDiveStop::Draft => self.enter_draft_edit(window, cx),
            DeepDiveStop::Send => self.send_message(window, cx),
        }
    }

    fn handle_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft_editing {
            self.exit_draft_edit(window, cx);
            return;
        }
        self.back(cx);
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
        let cwd = self.workspace_cwd.clone();
        let agent = self.agent.clone();
        match agent.try_lock() {
            Ok(mut provider) => {
                match provider.start_deep_dive_chat(
                    &self.agent_config_id,
                    cwd,
                    context,
                    Some(conversation),
                    self.launch_options.clone(),
                ) {
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
            task: self.config.node_id.to_string().into(),
            lifecycle_state: "active".into(),
            interview_purpose: self.config.phase.clone(),
            interview_phase: self.config.phase.clone(),
            question_id: self.parent.id.clone(),
            question_body: self.parent.display_body(),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.poll_agent(cx);
        key_context::set_input_tab_stop(&self.draft_input, self.draft_editing, cx);
        if !self.draft_editing
            && self
                .draft_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            self.enter_draft_edit(window, cx);
        }

        let theme = cx.theme();
        let border = theme.border;
        let accent = theme.accent;
        let foreground = theme.foreground;
        let muted = theme.muted_foreground;
        let background = theme.background;
        let in_flight = self.in_flight();

        div()
            .key_context(DEEP_DIVE_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(background)
            .v_flex()
            .on_action(cx.listener(|this, _: &DeepDiveStopUp, window, cx| {
                this.move_stop(-1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &DeepDiveStopDown, window, cx| {
                this.move_stop(1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &DeepDiveActivate, window, cx| {
                this.activate_stop(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &DeepDiveEscape, window, cx| {
                this.handle_escape(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &DeepDiveBack, _, cx| {
                this.back(cx);
                cx.stop_propagation();
            }))
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
                                            .rounded_md()
                                            .cursor_text()
                                            .when(self.stop_focused(DeepDiveStop::Draft), |el| {
                                                el.border_1().border_color(theme.list_active_border)
                                            })
                                            .on_mouse_down(
                                                gpui::MouseButton::Left,
                                                cx.listener(|this, _, window, cx| {
                                                    if !this.in_flight() && !this.draft_editing {
                                                        this.enter_draft_edit(window, cx);
                                                    }
                                                }),
                                            )
                                            .child(
                                                Input::new(&self.draft_input)
                                                    .disabled(in_flight || !self.draft_editing)
                                                    .focus_bordered(self.draft_editing)
                                                    .w_full(),
                                            ),
                                    )
                                    .child(
                                        Button::new("deep-dive-send")
                                            .label("Send")
                                            .primary()
                                            .selected(self.stop_focused(DeepDiveStop::Send))
                                            .disabled(in_flight)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.send_message(window, cx);
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("↑↓ control · Enter activate · Esc exit edit/back"),
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

pub fn register_deep_dive_keyboard_bindings(cx: &mut App) {
    let context = Some(key_context::excluding_input(DEEP_DIVE_CONTEXT));
    let input_context = Some(key_context::including_input(DEEP_DIVE_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("up", DeepDiveStopUp, context),
        KeyBinding::new("down", DeepDiveStopDown, context),
        KeyBinding::new("enter", DeepDiveActivate, context),
        KeyBinding::new("space", DeepDiveActivate, context),
        KeyBinding::new("escape", DeepDiveEscape, context),
        KeyBinding::new("escape", DeepDiveEscape, input_context),
    ]);
}
