use crate::agent_traffic::{
    AgentCategory, AgentSummary, SharedAgentTrafficLog, TrafficDirection, TrafficEntry,
};
use crate::app::transcript_window::TranscriptWindowControl;
use crate::fleet::{FleetStore, TranscriptTurn};
use crate::ui::actionable::{
    chrome_control_with_shortcut, render_label_badge, render_shortcut_pill,
};
use crate::ui::selectable_text::selectable_text;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyBinding,
    MouseButton, ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement, Styled,
    Window, actions, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, StyledExt, h_flex, v_flex};
use std::collections::BTreeMap;
use std::sync::Arc;

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
    grouped_agents: BTreeMap<AgentCategory, Vec<AgentSummary>>,
    selected_agent_id: Option<String>,
    turns: Vec<TurnRow>,
    header: SharedString,
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
        let mut this = Self {
            fleet,
            traffic_log,
            window_control,
            focus_handle: cx.focus_handle(),
            grouped_agents: BTreeMap::new(),
            selected_agent_id: None,
            turns: Vec::new(),
            header: "Agent transcripts".into(),
        };
        this.reload_agents();
        if let Some(first) = this.first_agent_id() {
            this.select_agent(first, cx);
        }
        let focus = this.focus_handle.clone();
        cx.defer_in(window, move |_, window, _| {
            focus.focus(window);
        });
        this
    }

    fn focus(&mut self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    fn close(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        self.window_control.clear();
        window.remove_window();
    }

    fn first_agent_id(&self) -> Option<String> {
        self.grouped_agents
            .values()
            .flat_map(|agents| agents.iter())
            .next()
            .map(|a| a.id.clone())
    }

    fn reload_agents(&mut self) {
        let mut grouped: BTreeMap<AgentCategory, Vec<AgentSummary>> = BTreeMap::new();

        if let Ok(log) = self.traffic_log.lock() {
            for (category, summaries) in log.agents_grouped() {
                grouped.entry(category).or_default().extend(summaries);
            }
        }

        if let Ok(fleet_agents) = self.fleet.list_all_agents() {
            let mut fleet_summaries: Vec<AgentSummary> = fleet_agents
                .into_iter()
                .map(|agent| {
                    let turn_count = self
                        .fleet
                        .list_transcript_for_agent(&agent.id)
                        .map(|t| t.len())
                        .unwrap_or(0);
                    AgentSummary {
                        id: agent.id.clone(),
                        label: format!("{} · {} · {}", agent.id, agent.env_type, agent.mode),
                        category: AgentCategory::Fleet,
                        entry_count: turn_count,
                    }
                })
                .collect();
            fleet_summaries.sort_by(|a, b| a.label.cmp(&b.label));
            let slot = grouped.entry(AgentCategory::Fleet).or_default();
            for summary in fleet_summaries {
                if !slot.iter().any(|existing| existing.id == summary.id) {
                    slot.push(summary);
                }
            }
        }

        self.grouped_agents = grouped;
    }

    fn select_agent(&mut self, agent_id: String, cx: &mut Context<Self>) {
        self.selected_agent_id = Some(agent_id.clone());
        self.turns = self.load_turns(&agent_id);
        self.header = format!("{} · {} turns", agent_id, self.turns.len()).into();
        cx.notify();
    }

    fn load_turns(&self, agent_id: &str) -> Vec<TurnRow> {
        let mut rows: Vec<TurnRow> = Vec::new();

        if let Ok(log) = self.traffic_log.lock() {
            for entry in log.entries_for_agent(agent_id) {
                rows.push(entry_to_row(entry));
            }
        }

        if let Ok(db_turns) = self.fleet.list_transcript_for_config(agent_id) {
            for turn in db_turns {
                if let Some(row) = fleet_turn_to_row(&turn) {
                    if !rows.iter().any(|existing| {
                        existing.sequence == row.sequence && existing.label == row.label
                    }) {
                        rows.push(row);
                    }
                }
            }
        }

        rows.sort_by_key(|row| row.sequence);
        rows
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
            self.header = "Agent transcripts · no agents yet".into();
            cx.notify();
        }
    }

    fn flat_agent_ids(&self) -> Vec<String> {
        self.grouped_agents
            .values()
            .flat_map(|agents| agents.iter().map(|agent| agent.id.clone()))
            .collect()
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

fn fleet_turn_to_row(turn: &TranscriptTurn) -> Option<TurnRow> {
    let direction = match turn.kind.as_str() {
        "prompt" => TrafficDirection::Request,
        "response" => TrafficDirection::Response,
        _ => return None,
    };
    Some(TurnRow {
        sequence: turn.sequence as u64,
        direction,
        label: format!("#{} · {} · fleet", turn.sequence, direction.label()).into(),
        content: turn.content.clone().into(),
    })
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
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.refresh(cx);
                                            })),
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
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.close(window, cx);
                                            })),
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
                            .when(self.grouped_agents.is_empty(), |el| {
                                el.child(
                                    div()
                                        .px_2()
                                        .text_xs()
                                        .text_color(muted)
                                        .child("No agent traffic logged yet."),
                                )
                            })
                            .children(self.grouped_agents.iter().map(|(category, agents)| {
                                v_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .px_2()
                                            .text_xs()
                                            .font_semibold()
                                            .text_color(muted)
                                            .child(category.label()),
                                    )
                                    .children(agents.iter().enumerate().map(|(ix, agent)| {
                                        let selected = self.selected_agent_id.as_deref()
                                            == Some(agent.id.as_str());
                                        let badge = pick_badges.get(&agent.id).cloned();
                                        div()
                                            .id(("agent-pick", ix))
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
                                                    .pr(if badge.is_some() {
                                                        px(18.)
                                                    } else {
                                                        px(0.)
                                                    })
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(foreground)
                                                            .child(agent.label.clone()),
                                                    )
                                                    .child(
                                                        div().text_xs().text_color(muted).child(
                                                            format!("{} turns", agent.entry_count),
                                                        ),
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
                            })),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        h_flex()
                            .px_4()
                            .py_2()
                            .border_b_1()
                            .border_color(border)
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(foreground)
                                    .child(self.header.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .p_4()
                            .gap_3()
                            .v_flex()
                            .when(self.turns.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_sm()
                                        .text_color(muted)
                                        .child("No transcript turns for this agent yet."),
                                )
                            })
                            .children(self.turns.iter().map(|turn| {
                                let turn_accent = match turn.direction {
                                    TrafficDirection::Request => cx.theme().primary,
                                    TrafficDirection::Response => cx.theme().accent,
                                };
                                v_flex()
                                    .id(("turn", turn.sequence))
                                    .gap_1()
                                    .p_3()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(border)
                                    .bg(muted_bg)
                                    .child(
                                        selectable_text(
                                            ("turn-label", turn.sequence),
                                            turn.label.clone(),
                                            window,
                                            cx,
                                        )
                                        .text_xs()
                                        .text_color(turn_accent),
                                    )
                                    .child(
                                        selectable_text(
                                            ("turn-content", turn.sequence),
                                            turn.content.clone(),
                                            window,
                                            cx,
                                        )
                                        .text_sm()
                                        .text_color(foreground),
                                    )
                            })),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    this.focus(window);
                }),
            )
    }
}
