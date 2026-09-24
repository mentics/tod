//! The details panel (`doc/ui/unified-view.md` "Details"): a node's title,
//! its lifecycle status label, its details field (one content block,
//! editable), and links to its other panels.
//!
//! Obligations and plan steps are *not* laid out here — they get panels of
//! their own (`PanelKind::Obligations` / `PanelKind::Plan`), reached through
//! the links below.

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, SharedString, Styled, Subscription, Window,
    actions, div, prelude::FluentBuilder,
};
use gpui_component::button::Button;
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::{ActiveTheme, Sizable};
use tod_store::fleet::FleetStore;
use tod_store::outline::repos::plan_steps::STATUS_VERIFIED;
use tod_store::outline::{EXTRA_CONTENT_DETAILS, OutlineMutation};
use uuid::Uuid;

use crate::ui::key_context;
use crate::ui::selectable_text::selectable_markdown;
use crate::unified::columns::PanelKind;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};

pub const UNIFIED_DETAILS_CONTEXT: &str = "UnifiedDetailsPanel";

actions!(unified_details_panel, [DetailsEnterEdit, DetailsEscape]);

/// Registers the details panel's own edit-mode keys. Enter, outside the
/// details textarea, enters edit mode; Escape, with or without the textarea
/// focused, exits it. Call once alongside
/// `unified::register_unified_keyboard_bindings`.
pub fn register_details_panel_keyboard_bindings(cx: &mut App) {
    let outside_input = Some(key_context::excluding_input(UNIFIED_DETAILS_CONTEXT));
    let with_input = Some(key_context::including_input(UNIFIED_DETAILS_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("enter", DetailsEnterEdit, outside_input),
        KeyBinding::new("escape", DetailsEscape, with_input),
    ]);
}

/// The status label for a node's lifecycle state, plain text for now.
///
/// W11 ("Status label") replaces this with `state`, `state →` (gate
/// running), `→ state` (on-entry agent running), read from the shared
/// agent-run registry (W3). This is that spot.
pub fn status_label(lifecycle: &str) -> SharedString {
    SharedString::from(lifecycle.to_string())
}

/// Loaded, denormalized data for one node's details panel. Re-fetched
/// whenever the store changes.
struct Loaded {
    title: String,
    lifecycle: String,
    details: String,
    obligation_count: usize,
    plan_done: usize,
    plan_total: usize,
    decisions_waiting: usize,
}

impl Loaded {
    fn empty() -> Self {
        Self {
            title: String::new(),
            lifecycle: String::new(),
            details: String::new(),
            obligation_count: 0,
            plan_done: 0,
            plan_total: 0,
            decisions_waiting: 0,
        }
    }
}

fn load(fleet: &FleetStore, node_id: Uuid) -> Loaded {
    let mut loaded = Loaded::empty();
    if let Ok(Some(task)) = fleet.get_node(&node_id.to_string()) {
        loaded.title = task.title;
        loaded.lifecycle = task.lifecycle;
    }
    loaded.details = fleet
        .get_extra_content(node_id, EXTRA_CONTENT_DETAILS)
        .ok()
        .flatten()
        .unwrap_or_default();
    if let Ok(obligations) = fleet.list_obligations_for_node(node_id) {
        loaded.obligation_count = obligations.len();
    }
    if let Ok(steps) = fleet.list_plan_steps_for_node(node_id) {
        loaded.plan_total = steps.len();
        loaded.plan_done = steps.iter().filter(|s| s.status == STATUS_VERIFIED).count();
    }
    loaded.decisions_waiting = fleet
        .read(|conn| tod_core::attention::for_node(conn, node_id))
        .map(|attention| {
            attention
                .items
                .iter()
                .filter(|item| item.kind == tod_core::attention::AttentionKind::Decision)
                .count()
        })
        .unwrap_or(0);
    loaded
}

pub struct DetailsPanel {
    node_id: Uuid,
    fleet: Arc<FleetStore>,
    focus_handle: FocusHandle,
    details_input: gpui::Entity<TextareaState>,
    editing: bool,
    loaded: Loaded,
    pending_refresh: bool,
    _details_subscription: Subscription,
    _poll: gpui::Task<()>,
}

impl DetailsPanel {
    pub fn new(
        node_id: Uuid,
        fleet: Arc<FleetStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let details_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(6)
                .placeholder("Enter to edit · Imported ticket description or freeform details…")
        });
        let _details_subscription = cx.subscribe(&details_input, |this, _, event, cx| {
            if matches!(event, gpui_component::input::InputEvent::Blur) {
                this.persist_details(cx);
            }
        });

        let poll_entity = cx.weak_entity();
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
                    let Ok(()) = poll_entity.update(cx, |this: &mut DetailsPanel, cx| {
                        this.pending_refresh = true;
                        cx.notify();
                    }) else {
                        break;
                    };
                }
            }
        });

        let loaded = load(&fleet, node_id);
        details_input.update(cx, |input, cx| {
            input.set_value(loaded.details.clone(), window, cx);
        });

        Self {
            node_id,
            fleet,
            focus_handle: cx.focus_handle(),
            details_input,
            editing: false,
            loaded,
            pending_refresh: false,
            _details_subscription,
            _poll,
        }
    }

    /// Retarget this column to a different node, in place.
    pub fn set_node(&mut self, node_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing {
            self.persist_details(cx);
        }
        self.node_id = node_id;
        self.editing = false;
        self.reload(window, cx);
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loaded = load(&self.fleet, self.node_id);
        let details = self.loaded.details.clone();
        self.details_input.update(cx, |input, cx| {
            if input.text().to_string() != details {
                input.set_value(details, window, cx);
            }
        });
        cx.notify();
    }

    fn enter_edit(&mut self, _: &DetailsEnterEdit, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = true;
        cx.notify();
        let input = self.details_input.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        });
    }

    fn exit_edit(&mut self, _: &DetailsEscape, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing {
            return;
        }
        self.persist_details(cx);
        self.editing = false;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn persist_details(&mut self, cx: &mut Context<Self>) {
        let value = self.details_input.read(cx).text().to_string();
        if value == self.loaded.details {
            return;
        }
        if self
            .fleet
            .enqueue_outline(OutlineMutation::SetExtraContent {
                node_id: self.node_id,
                content_type: EXTRA_CONTENT_DETAILS.to_string(),
                body: value.clone(),
            })
            .is_ok()
        {
            self.loaded.details = value;
        }
    }

    fn open(&self, target: PanelKind, cx: &mut Context<Self>) {
        cx.emit(PanelOpenRequest {
            target,
            ctrl: false,
        });
    }
}

impl ColumnPanel for DetailsPanel {
    fn title(&self, _cx: &App) -> SharedString {
        "Details".into()
    }

    fn target_label(&self, _cx: &App) -> SharedString {
        self.loaded.title.clone().into()
    }
}

impl EventEmitter<PanelOpenRequest> for DetailsPanel {}

impl Focusable for DetailsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DetailsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_refresh {
            self.pending_refresh = false;
            self.reload(window, cx);
        }
        key_context::set_input_tab_stop(&self.details_input, self.editing, cx);

        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let node_id = self.node_id;
        let plan_label = format!("Plan ({}/{})", self.loaded.plan_done, self.loaded.plan_total);
        let obligations_label = format!("Obligations ({})", self.loaded.obligation_count);
        let decisions_waiting = self.loaded.decisions_waiting;

        div()
            .id("unified-details-panel")
            .key_context(UNIFIED_DETAILS_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::enter_edit))
            .on_action(cx.listener(Self::exit_edit))
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .size_full()
            .child(
                div()
                    .text_lg()
                    .child(selectable_markdown(
                        "unified-details-title",
                        self.loaded.title.clone(),
                        window,
                        cx,
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(status_label(&self.loaded.lifecycle)),
            )
            .child(
                div()
                    .id("unified-details-content")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .cursor_text()
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            if !this.editing {
                                this.enter_edit(&DetailsEnterEdit, window, cx);
                            }
                        }),
                    )
                    .child(if self.editing {
                        Textarea::new(&self.details_input)
                            .disabled(false)
                            .w_full()
                            .into_any_element()
                    } else {
                        selectable_markdown(
                            "unified-details-body",
                            self.loaded.details.clone(),
                            window,
                            cx,
                        )
                        .into_any_element()
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("unified-details-open-obligations")
                            .label(obligations_label)
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open(PanelKind::Obligations(node_id), cx);
                            })),
                    )
                    .child(
                        Button::new("unified-details-open-plan")
                            .label(plan_label)
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open(PanelKind::Plan(node_id), cx);
                            })),
                    )
                    .when(decisions_waiting > 0, |el| {
                        el.child(
                            Button::new("unified-details-open-decisions")
                                .label(format!("Decisions waiting ({decisions_waiting})"))
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open(PanelKind::Decisions, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("unified-details-open-settings")
                            .label("Settings")
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open(PanelKind::Settings(node_id), cx);
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::rows::fixture::Fixture;
    use gpui::{TestAppContext, VisualTestContext};
    use gpui_component::Root;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn open_panel<'a>(
        node_id: Uuid,
        fleet: Arc<FleetStore>,
        cx: &'a mut TestAppContext,
    ) -> (gpui::Entity<DetailsPanel>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let slot = Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| DetailsPanel::new(node_id, fleet, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        draw(cx);
        (view, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn shows_title_and_counts_for_the_target_node(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_panel(fixture.node_id, fixture.store.clone(), cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.title, "Web client");
            assert_eq!(view.loaded.obligation_count, 3);
            assert_eq!(view.loaded.plan_total, 2);
            assert_eq!(view.loaded.plan_done, 0);
        });
    }

    #[gpui::test]
    fn clicking_the_obligations_link_emits_the_open_request(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, cx) = open_panel(fixture.node_id, fixture.store.clone(), cx);
        let node_id = fixture.node_id;

        let seen: Rc<RefCell<Option<PanelOpenRequest>>> = Rc::new(RefCell::new(None));
        let seen_in = seen.clone();
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_, event: &PanelOpenRequest, _| {
                *seen_in.borrow_mut() = Some(event.clone());
            })
            .detach();
        });

        view.update(cx, |view, cx| {
            view.open(PanelKind::Obligations(node_id), cx);
        });
        draw(cx);

        let request = seen.borrow().clone().expect("open request emitted");
        assert_eq!(request.target, PanelKind::Obligations(node_id));
        assert!(!request.ctrl);
    }
}
