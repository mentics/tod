//! Lifecycle panel — lets the user manually advance a task's lifecycle state
//! when there is no interview work left to route to (see
//! `TaskListEvent::OpenLifecycle` / `handle_lifecycle_control` in
//! `views/task_list/mod.rs`).

use crate::ui::actionable::chrome_control_with_shortcut;
use crate::ui::key_context;
use crate::ui::pane_nav::{PaneFocusLeft, bind_modified_pane_nav};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window, actions, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Disableable, Selectable, StyledExt, h_flex, v_flex};
use std::sync::Arc;
use tod_core::task::model::{LIFECYCLE_STATES, lifecycle_rank};
use tod_store::fleet::FleetStore;
use tod_store::outline::OutlineMutation;

const LIFECYCLE_PANEL_CONTEXT: &str = "LifecyclePanel";

actions!(lifecycle_panel, [LifecyclePanelClose]);

#[derive(Debug, Clone)]
pub enum LifecyclePanelEvent {
    Close,
    /// Escape / Ctrl+Left — move keyboard focus back to the task tree, leaving
    /// the panel open. Mirrors the drawer-panel convention documented in
    /// CLAUDE.md.
    FocusTaskList,
}

pub struct LifecyclePanelView {
    fleet: Arc<FleetStore>,
    task_id: Option<String>,
    title: String,
    lifecycle: String,
    focus_handle: FocusHandle,
}

impl LifecyclePanelView {
    pub fn new(cx: &mut Context<Self>, fleet: Arc<FleetStore>) -> Self {
        Self {
            fleet,
            task_id: None,
            title: String::new(),
            lifecycle: String::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.task_id.is_some()
    }

    fn load_task(&mut self, task_id: &str) -> bool {
        match self.fleet.get_task(task_id) {
            Ok(Some(task)) => {
                self.title = task.title;
                self.lifecycle = task.lifecycle;
                true
            }
            _ => false,
        }
    }

    pub fn open(&mut self, task_id: &str, cx: &mut Context<Self>) {
        self.task_id = Some(task_id.to_string());
        if !self.load_task(task_id) {
            self.task_id = None;
            return;
        }
        cx.notify();
    }

    pub fn retarget(&mut self, task_id: &str, cx: &mut Context<Self>) {
        if self.task_id.as_deref() == Some(task_id) {
            return;
        }
        let previous = self.task_id.clone();
        self.task_id = Some(task_id.to_string());
        if !self.load_task(task_id) {
            self.task_id = previous;
        }
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.task_id.is_none() {
            return;
        }
        self.task_id = None;
        self.title.clear();
        self.lifecycle.clear();
        cx.emit(LifecyclePanelEvent::Close);
        cx.notify();
    }

    /// Persist a new lifecycle state for the open task, then close the panel.
    fn set_lifecycle(&mut self, state: &str, cx: &mut Context<Self>) {
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        let Ok(node_id) = uuid::Uuid::parse_str(&task_id) else {
            return;
        };
        if let Err(err) = self.fleet.enqueue_outline(OutlineMutation::SetLifecycle {
            node_id,
            state: state.to_string(),
        }) {
            tracing::error!("failed to set lifecycle: {err:#}");
            return;
        }
        let _ = self.fleet.writer().flush();
        self.close(cx);
    }

    fn on_close(&mut self, _: &LifecyclePanelClose, _: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
    }
}

impl EventEmitter<LifecyclePanelEvent> for LifecyclePanelView {}

impl Focusable for LifecyclePanelView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for LifecyclePanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.is_open() {
            return div().size_full().into_any_element();
        }

        let theme = cx.theme();
        let border = theme.border;
        let background = theme.background;
        let secondary = theme.secondary;
        let muted = theme.muted_foreground;
        let accent = theme.primary;

        let current_rank = lifecycle_rank(&self.lifecycle);
        let next_state = LIFECYCLE_STATES.get(current_rank + 1).copied();

        let mut states = v_flex().gap_2().w_full();
        for (idx, state) in LIFECYCLE_STATES.into_iter().enumerate() {
            let is_current = state == self.lifecycle;
            let is_next = Some(state) == next_state;
            let label = if is_next {
                format!("{state} (next)")
            } else {
                state.to_string()
            };
            states = states.child(
                Button::new(("lifecycle-panel-state", idx as u64))
                    .label(label)
                    .w_full()
                    .selected(is_current)
                    .when(is_current, |b| b.disabled(true))
                    .when(!is_current, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| {
                            this.set_lifecycle(state, cx);
                        }))
                    }),
            );
        }

        v_flex()
            .key_context(LIFECYCLE_PANEL_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .h_full()
            .bg(background)
            .border_l_2()
            .border_color(accent)
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(|_, _: &PaneFocusLeft, _, cx| {
                cx.emit(LifecyclePanelEvent::FocusTaskList);
                cx.stop_propagation();
            }))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .bg(secondary)
                    .child(div().text_sm().font_semibold().child("Lifecycle"))
                    .child(div().flex_1())
                    .child(chrome_control_with_shortcut(
                        Button::new("lifecycle-panel-close")
                            .label("Close")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close(cx);
                            })),
                        window,
                        &LifecyclePanelClose,
                        LIFECYCLE_PANEL_CONTEXT,
                        cx,
                    )),
            )
            .child(
                v_flex()
                    .id("lifecycle-panel-body")
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .p_3()
                    .overflow_y_scroll()
                    .child(div().text_sm().font_semibold().child(self.title.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("Current: {}", self.lifecycle)),
                    )
                    .child(states),
            )
            .into_any_element()
    }
}

pub fn register_lifecycle_panel_keyboard_bindings(cx: &mut App) {
    key_context::bind_panel_escape(cx, LifecyclePanelClose, LIFECYCLE_PANEL_CONTEXT);
    bind_modified_pane_nav(cx, LIFECYCLE_PANEL_CONTEXT);
}
