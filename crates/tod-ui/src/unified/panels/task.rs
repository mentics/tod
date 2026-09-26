//! The task panel (`doc/ui/task-panel.md`): the default panel for a **task
//! node** (Lifecycle on itself, Agent on itself or inherited; see
//! `tod_store::fleet::node_actions::is_task_node`).
//!
//! Layout, top to bottom, ordered by permanence:
//!
//! 1. **Identity** — title and Linear ticket id (`render_identity`).
//! 2. **Artifacts** — Obligations and Plan links, each opening its panel by
//!    the column rule (`render_artifacts`). The Changes link (T8) joins this
//!    strip.
//! 3. **Runner line** (T2) — `render_runner_line`, a placeholder for now.
//! 4. **Requests** (T4) — the only part that scrolls; `render_requests`,
//!    a placeholder for now.
//! 5. **Answered drawer** (T5) — anchored to the bottom;
//!    `render_answered_drawer`, a placeholder for now.
//!
//! The first three are fixed. Everything reloads on store change, as the
//! details panel does.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
};
use tod_store::fleet::FleetStore;
use tod_store::outline::repos::plan_steps::STATUS_VERIFIED;
use uuid::Uuid;

use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use crate::unified::columns::PanelKind;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};

/// Loaded, denormalized data for the task panel's header. Re-fetched
/// whenever the store changes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct TaskHeader {
    pub title: String,
    /// The external (Linear) ticket id, when the node carries one.
    pub ticket_id: Option<String>,
    pub obligation_count: usize,
    /// Obligations whose latest verdict is `failed`.
    pub obligations_failed: usize,
    pub plan_done: usize,
    pub plan_total: usize,
}

impl TaskHeader {
    pub(crate) fn obligations_label(&self) -> String {
        if self.obligations_failed > 0 {
            format!(
                "Obligations {} · {} failed",
                self.obligation_count, self.obligations_failed
            )
        } else {
            format!("Obligations {}", self.obligation_count)
        }
    }

    pub(crate) fn plan_label(&self) -> String {
        format!("Plan {}/{}", self.plan_done, self.plan_total)
    }
}

fn load(fleet: &FleetStore, node_id: Uuid) -> TaskHeader {
    let mut header = TaskHeader::default();
    if let Ok(Some(node)) = fleet.get_node(&node_id.to_string()) {
        header.title = node.title;
    }
    header.ticket_id = fleet
        .read(|conn| {
            Ok(tod_store::outline::repos::NodeRepo::new(conn).get_ticket_id(node_id)?)
        })
        .ok()
        .flatten()
        .filter(|id| !id.is_empty());
    if let Ok(obligations) = fleet.list_obligations_for_node(node_id) {
        header.obligation_count = obligations.len();
    }
    if let Ok(latest) = fleet.read(|conn| {
        Ok(tod_store::verification::VerdictRepo::new(conn).latest_for_node(node_id)?)
    }) {
        header.obligations_failed = latest.values().filter(|v| v.is_failed()).count();
    }
    if let Ok(steps) = fleet.list_plan_steps_for_node(node_id) {
        header.plan_total = steps.len();
        header.plan_done = steps.iter().filter(|s| s.status == STATUS_VERIFIED).count();
    }
    header
}

pub struct TaskPanel {
    node_id: Uuid,
    fleet: Arc<FleetStore>,
    focus_handle: FocusHandle,
    header: TaskHeader,
    pending_refresh: bool,
    _poll: gpui::Task<()>,
}

impl TaskPanel {
    pub fn new(node_id: Uuid, fleet: Arc<FleetStore>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Store changes mark the header stale; it reloads on the next render
        // (the same event-driven refresh as `DetailsPanel`).
        let weak = cx.weak_entity();
        let fleet_for_poll = fleet.clone();
        let _poll = cx.spawn(async move |_, cx| {
            let mut fleet_rx = fleet_for_poll.subscribe_changes();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                let mut changed = false;
                while fleet_rx.try_recv().is_ok() {
                    changed = true;
                }
                if changed {
                    let Ok(()) = weak.update(cx, |this: &mut TaskPanel, cx| {
                        this.pending_refresh = true;
                        cx.notify();
                    }) else {
                        break;
                    };
                }
            }
        });
        let header = load(&fleet, node_id);
        Self {
            node_id,
            fleet,
            focus_handle: cx.focus_handle(),
            header,
            pending_refresh: false,
            _poll,
        }
    }

    #[cfg(test)]
    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    /// Retarget this column to a different task, in place.
    pub fn set_node(&mut self, node_id: Uuid, cx: &mut Context<Self>) {
        if node_id == self.node_id {
            return;
        }
        self.node_id = node_id;
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.header = load(&self.fleet, self.node_id);
        cx.notify();
    }

    fn open(&self, target: PanelKind, ctrl: bool, cx: &mut Context<Self>) {
        cx.emit(PanelOpenRequest { target, ctrl });
    }

    /// Title and ticket id: am I where I think I am?
    fn render_identity(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_start()
            .justify_between()
            .gap_2()
            .child(
                style::text_title(div().flex_1().min_w_0()).child(selectable_text(
                    "unified-task-title",
                    self.header.title.clone(),
                    window,
                    cx,
                )),
            )
            .when_some(self.header.ticket_id.clone(), |el, ticket| {
                el.child(style::text_muted(div().flex_none().child(selectable_text(
                    "unified-task-ticket",
                    ticket,
                    window,
                    cx,
                ))))
            })
            .into_any_element()
    }

    /// One link on the artifact strip: click opens `target` by the column
    /// rule, Ctrl+click as usual.
    fn artifact_link(
        &self,
        id: &'static str,
        label: String,
        target: PanelKind,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        style::text_link(div().id(id))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let ctrl = event.modifiers.control || event.modifiers.platform;
                    this.open(target, ctrl, cx);
                }),
            )
            .child(label)
            .into_any_element()
    }

    /// The task's artifacts: what is it made of, and how far along is it?
    fn render_artifacts(&self, cx: &mut Context<Self>) -> AnyElement {
        let node_id = self.node_id;
        let obligations = self.artifact_link(
            "unified-task-obligations",
            self.header.obligations_label(),
            PanelKind::Obligations(node_id),
            cx,
        );
        let plan = self.artifact_link(
            "unified-task-plan",
            self.header.plan_label(),
            PanelKind::Plan(node_id),
            cx,
        );
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .child(obligations)
            .child(plan)
            // T8: the Changes link (files changed) goes here.
            .into_any_element()
    }

    /// T2: the runner line (lifecycle state, what the runner is doing, Stop).
    /// Not implemented yet.
    fn render_runner_line(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        None
    }

    /// T4: the requests waiting on the user — the only part that scrolls.
    /// Not implemented yet.
    fn render_requests(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        None
    }

    /// T5: the Answered drawer, anchored to the bottom of the column.
    /// Not implemented yet.
    fn render_answered_drawer(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        None
    }
}

impl ColumnPanel for TaskPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "Task".into()
    }

    fn target_label(&self, _cx: &App) -> SharedString {
        self.header.title.clone().into()
    }
}

impl EventEmitter<PanelOpenRequest> for TaskPanel {}

impl Focusable for TaskPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TaskPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_refresh {
            self.pending_refresh = false;
            self.header = load(&self.fleet, self.node_id);
        }
        let identity = self.render_identity(window, cx);
        let artifacts = self.render_artifacts(cx);
        let runner = self.render_runner_line(cx);
        let requests = self.render_requests(cx);
        let drawer = self.render_answered_drawer(cx);

        div()
            .id("unified-task-panel")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            // Fixed header: identity, artifacts, runner.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .flex_none()
                    .child(identity)
                    .child(artifacts)
                    .children(runner),
            )
            // Requests scroll; the drawer stays at the bottom.
            .child(
                div()
                    .id("unified-task-requests")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(requests),
            )
            .children(drawer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_show_failed_only_when_any() {
        let mut header = TaskHeader {
            obligation_count: 7,
            plan_done: 5,
            plan_total: 6,
            ..Default::default()
        };
        assert_eq!(header.obligations_label(), "Obligations 7");
        assert_eq!(header.plan_label(), "Plan 5/6");
        header.obligations_failed = 1;
        assert_eq!(header.obligations_label(), "Obligations 7 · 1 failed");
    }
}
