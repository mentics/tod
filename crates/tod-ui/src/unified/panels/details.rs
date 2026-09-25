//! The details panel (`doc/ui/unified-view.md` "Details"): a node's title,
//! its lifecycle status label, its details field (one content block,
//! editable), and links to its other panels.
//!
//! Obligations and plan steps are *not* laid out here — they get panels of
//! their own (`PanelKind::Obligations` / `PanelKind::Plan`), reached through
//! the links below.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, SharedString, Styled, Subscription, Window,
    actions, div, prelude::FluentBuilder,
};
use gpui_component::button::Button;
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::Sizable;
use tod_store::fleet::FleetStore;
use tod_store::outline::repos::plan_steps::STATUS_VERIFIED;
use tod_store::outline::{EXTRA_CONTENT_DETAILS, OutlineMutation};
use uuid::Uuid;

use crate::ui::agent_runs::AgentRuns;
use crate::ui::key_context;
use crate::ui::selectable_text::selectable_markdown;
use crate::ui::style;
use crate::unified::columns::PanelKind;
use crate::unified::panel::{ColumnPanel, PanelOpenRequest};
use crate::unified::status_label;

pub const UNIFIED_DETAILS_CONTEXT: &str = "UnifiedDetailsPanel";

actions!(unified_details_panel, [DetailsEnterEdit, DetailsEscape, DetailsSave]);

/// Registers the details panel's own edit-mode keys. Enter, outside the
/// details textarea, enters edit mode; Ctrl+Enter, with the textarea
/// focused, commits the edit (explicit save — never on blur); Escape, with
/// or without the textarea focused, discards it.
/// Call once alongside `unified::register_unified_keyboard_bindings`.
pub fn register_details_panel_keyboard_bindings(cx: &mut App) {
    let outside_input = Some(key_context::excluding_input(UNIFIED_DETAILS_CONTEXT));
    let with_input = Some(key_context::including_input(UNIFIED_DETAILS_CONTEXT));
    cx.bind_keys([
        KeyBinding::new("enter", DetailsEnterEdit, outside_input),
        KeyBinding::new("escape", DetailsEscape, with_input),
        KeyBinding::new("ctrl-enter", DetailsSave, with_input),
    ]);
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

/// The details field's height while it is being edited.
const DETAILS_EDIT_HEIGHT: f32 = 200.;

pub struct DetailsPanel {
    node_id: Uuid,
    fleet: Arc<FleetStore>,
    agent_runs: Entity<AgentRuns>,
    focus_handle: FocusHandle,
    details_input: gpui::Entity<TextareaState>,
    editing: bool,
    loaded: Loaded,
    pending_refresh: bool,
    /// Unsaved details text per node, kept so switching the selection away
    /// and back does not silently drop an in-progress edit (explicit save
    /// only, `.claude/CLAUDE.md`: a node switch is not a save or a discard).
    drafts: HashMap<Uuid, String>,
    _agent_runs_subscription: Subscription,
    _poll: gpui::Task<()>,
}

impl DetailsPanel {
    pub fn new(
        node_id: Uuid,
        fleet: Arc<FleetStore>,
        agent_runs: Entity<AgentRuns>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let details_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(6)
                .placeholder("Enter to edit · Imported ticket description or freeform details…")
        });
        // Explicit save, not on blur (`.claude/CLAUDE.md`): blur must not
        // save and must not discard — the edit just stays in progress.
        // Ctrl+Enter (`DetailsSave`) or the Save button commits; Escape
        // discards. No listener on `InputEvent::Blur` here on purpose.
        let _agent_runs_subscription = cx.observe(&agent_runs, |_, _, cx| {
            cx.notify();
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
            agent_runs,
            focus_handle: cx.focus_handle(),
            details_input,
            editing: false,
            loaded,
            pending_refresh: false,
            drafts: HashMap::new(),
            _agent_runs_subscription,
            _poll,
        }
    }

    /// Retarget this column to a different node, in place. An edit in
    /// progress on the old node is kept as a draft (`self.drafts`), not
    /// discarded — a node switch is not a save or a discard
    /// (`.claude/CLAUDE.md`: explicit save only). Switching back to a node
    /// with a pending draft restores it, still in edit mode.
    pub fn set_node(&mut self, node_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if node_id == self.node_id {
            return;
        }
        self.capture_draft(cx);
        self.node_id = node_id;
        self.editing = false;
        self.reload(window, cx);
        self.restore_draft(window, cx);
    }

    /// Stash the in-progress edit for the current node, if any, so it is not
    /// lost when the selection moves elsewhere. A no-op unless `editing` and
    /// the text actually differs from what is saved.
    fn capture_draft(&mut self, cx: &mut Context<Self>) {
        if !self.editing {
            return;
        }
        let text = self.details_input.read(cx).text().to_string();
        if text == self.loaded.details {
            self.drafts.remove(&self.node_id);
        } else {
            self.drafts.insert(self.node_id, text);
        }
    }

    /// Restore a draft stashed for `self.node_id`, if one exists, back into
    /// the textarea and into edit mode.
    fn restore_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(draft) = self.drafts.get(&self.node_id).cloned() {
            self.details_input.update(cx, |input, cx| {
                input.set_value(draft, window, cx);
            });
            self.editing = true;
        }
    }

    /// True while the current node has an unsaved draft — shows the
    /// "Unsaved changes" marker with Save/Discard regardless of whether the
    /// textarea happens to be focused right now.
    fn has_draft(&self) -> bool {
        self.drafts.contains_key(&self.node_id)
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loaded = load(&self.fleet, self.node_id);
        // Don't clobber an in-progress edit or a stashed draft with the
        // freshly loaded stored text — if the node's stored text changed
        // elsewhere while a draft is pending, the draft still wins; it is
        // shown as unsaved rather than silently overwritten.
        if !self.editing && !self.has_draft() {
            let details = self.loaded.details.clone();
            self.details_input.update(cx, |input, cx| {
                if input.text().to_string() != details {
                    input.set_value(details, window, cx);
                }
            });
        }
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

    /// Escape: discard the edit, restoring the saved text. Explicit save
    /// (`.claude/CLAUDE.md`) means Escape never saves.
    fn discard_edit(&mut self, _: &DetailsEscape, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing {
            return;
        }
        let details = self.loaded.details.clone();
        self.details_input.update(cx, |input, cx| {
            input.set_value(details, window, cx);
        });
        self.editing = false;
        self.drafts.remove(&self.node_id);
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Ctrl+Enter or the Save button: commit the edit. The only path that
    /// writes `EXTRA_CONTENT_DETAILS` — blur and Escape never call this.
    fn save_edit(&mut self, _: &DetailsSave, window: &mut Window, cx: &mut Context<Self>) {
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
            self.drafts.remove(&self.node_id);
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
            self.drafts.remove(&self.node_id);
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

        let node_id = self.node_id;
        let plan_label = format!("Plan ({}/{})", self.loaded.plan_done, self.loaded.plan_total);
        let obligations_label = format!("Obligations ({})", self.loaded.obligation_count);
        let decisions_waiting = self.loaded.decisions_waiting;
        let editing = self.editing;
        let dirty = editing
            && self.details_input.read(cx).text().to_string() != self.loaded.details;
        let has_draft = self.has_draft() || dirty;
        let runs = self.agent_runs.read(cx).runs_for_node(self.node_id);

        div()
            .id("unified-details-panel")
            .key_context(UNIFIED_DETAILS_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::enter_edit))
            .on_action(cx.listener(Self::discard_edit))
            .on_action(cx.listener(Self::save_edit))
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
            .child(status_label::render(&self.loaded.lifecycle, &runs, cx))
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
                        // A textarea has no height of its own (`rows` is
                        // not one): given none it collapses to a line.
                        Textarea::new(&self.details_input)
                            .disabled(false)
                            .w_full()
                            .h(gpui::px(DETAILS_EDIT_HEIGHT))
                            .into_any_element()
                    } else if self.loaded.details.is_empty() {
                        div()
                            .id("unified-details-empty")
                            .w_full()
                            .border(style::size::BORDER)
                            .border_color(style::color::divider())
                            .rounded(style::radius::CONTROL)
                            .px(style::space::INSET)
                            .py(style::space::RELATED)
                            .child(style::empty_message(div().child(
                                "No details — press Enter to add",
                            )))
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
            .when(has_draft, |el| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(style::text_muted(div().child("Unsaved changes")))
                        .when(editing, |el| {
                            el.child(
                                Button::new("unified-details-save")
                                    .label("Save")
                                    .small()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_edit(&DetailsSave, window, cx);
                                    })),
                            )
                        })
                        .child(
                            Button::new("unified-details-discard")
                                .label("Discard")
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.discard_edit(&DetailsEscape, window, cx);
                                })),
                        ),
                )
            })
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
    use crate::interview::agent::SharedAgent;
    use crate::views::rows::fixture::Fixture;
    use gpui::{TestAppContext, VisualTestContext};
    use gpui_component::Root;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Mutex;
    use tod_agent::MockAgentProvider;

    fn mock_agent() -> SharedAgent {
        Arc::new(Mutex::new(Box::new(MockAgentProvider::new())))
    }

    fn open_panel<'a>(
        node_id: Uuid,
        fleet: Arc<FleetStore>,
        cx: &'a mut TestAppContext,
    ) -> (gpui::Entity<DetailsPanel>, Entity<AgentRuns>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let agent_runs = cx.new(|_| AgentRuns::new(fleet.clone(), mock_agent()));
        let agent_runs_for_view = agent_runs.clone();
        let slot = Rc::new(RefCell::new(None));
        let slot_in = slot.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| DetailsPanel::new(node_id, fleet, agent_runs_for_view, window, cx));
            *slot_in.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().unwrap();
        draw(cx);
        (view, agent_runs, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn shows_title_and_counts_for_the_target_node(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, _agent_runs, cx) = open_panel(fixture.node_id, fixture.store.clone(), cx);

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
        let (view, _agent_runs, cx) = open_panel(fixture.node_id, fixture.store.clone(), cx);
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

    #[gpui::test]
    fn the_status_label_updates_when_a_run_starts_in_agent_runs(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, agent_runs, cx) = open_panel(fixture.node_id, fixture.store.clone(), cx);
        let node = fixture.node_id;
        let focus = tod_store::conversation::Focus::Node(node);

        let before = view.read_with(cx, |view, cx| {
            status_label::text(&view.loaded.lifecycle, &view.agent_runs.read(cx).runs_for_node(node))
        });
        assert!(!before.contains('\u{2192}'));

        agent_runs.update(cx, |registry, _| {
            let config = tod_core::conversation::ConversationConfig {
                data_root: fixture.store.paths().root().to_path_buf(),
                media: tod_core::media::MediaPaths::discover().expect("media paths"),
                launch: tod_agent::AgentLaunchOptions::for_platform(tod_agent::AgentPlatform::Claude),
                context: Default::default(),
            };
            let driver = tod_core::conversation::ConversationDriver::new(
                config,
                focus,
                tod_store::conversation::ProtocolKind::GateCheck,
            );
            let ix = registry
                .ensure(focus, tod_store::conversation::ProtocolKind::GateCheck, None, || Ok(driver))
                .unwrap();
            // Take the driver to send without putting it back: this is what
            // marks the slot `running` (`DriverSlot::take_to_send`) — a real
            // send would put it back only once the turn (and so the run)
            // finishes, which is not what this test is checking.
            let _ = registry.take_to_send(ix).unwrap();
        });
        cx.run_until_parked();
        draw(cx);

        let after = view.read_with(cx, |view, cx| {
            status_label::text(&view.loaded.lifecycle, &view.agent_runs.read(cx).runs_for_node(node))
        });
        assert!(after.contains('\u{2192}'), "expected a running arrow, got {after:?}");
    }

    #[gpui::test]
    fn blur_does_not_save(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, _agent_runs, cx) = open_panel(fixture.node_id, fixture.store.clone(), cx);
        let original = view.read_with(cx, |view, _| view.loaded.details.clone());
        let new_text = format!("{original} edited");

        view.update_in(cx, |view, window, cx| {
            view.enter_edit(&DetailsEnterEdit, window, cx);
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            let new_text = new_text.clone();
            view.details_input.update(cx, |input, cx| {
                input.set_value(new_text, window, cx);
            });
        });
        draw(cx);

        // Simulate blur by firing the input's Blur event directly — the
        // panel attaches no listener to it (explicit save, not on blur), so
        // this must be a no-op for persistence and for edit mode.
        view.update(cx, |view, cx| {
            view.details_input.update(cx, |_, cx| {
                cx.emit(gpui_component::input::InputEvent::Blur);
            });
        });
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.details, original, "blur must not persist the edit");
            assert!(view.editing, "blur must not exit edit mode either");
        });
    }

    #[gpui::test]
    fn ctrl_enter_saves_the_edit(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, _agent_runs, cx) = open_panel(fixture.node_id, fixture.store.clone(), cx);
        let original = view.read_with(cx, |view, _| view.loaded.details.clone());
        let new_text = format!("{original} saved via ctrl+enter");
        let new_text_for_set = new_text.clone();

        view.update_in(cx, |view, window, cx| {
            view.enter_edit(&DetailsEnterEdit, window, cx);
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            view.details_input.update(cx, |input, cx| {
                input.set_value(new_text_for_set, window, cx);
            });
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            view.save_edit(&DetailsSave, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.details, new_text);
            assert!(!view.editing);
        });
    }

    #[gpui::test]
    fn escape_discards_the_edit(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let (view, _agent_runs, cx) = open_panel(fixture.node_id, fixture.store.clone(), cx);
        let original = view.read_with(cx, |view, _| view.loaded.details.clone());

        view.update_in(cx, |view, window, cx| {
            view.enter_edit(&DetailsEnterEdit, window, cx);
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            view.details_input.update(cx, |input, cx| {
                input.set_value(format!("{original} discard me"), window, cx);
            });
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            view.discard_edit(&DetailsEscape, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, cx| {
            assert_eq!(view.loaded.details, original, "escape must not save");
            assert!(!view.editing);
            assert_eq!(view.details_input.read(cx).text().to_string(), original, "escape restores the text");
        });
    }

    /// A second node in the same store, for tests that switch the panel's
    /// target.
    fn second_node(fixture: &Fixture) -> Uuid {
        let list_id = fixture.store.list_outline_lists().unwrap()[0].id;
        let node_id = Uuid::new_v4();
        fixture
            .store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node_id),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: tod_store::outline::CreatePosition::Below,
                title: "Mobile client".into(),
            })
            .unwrap();
        fixture.store.writer().flush().unwrap();
        node_id
    }

    #[gpui::test]
    fn switching_away_and_back_restores_the_unsaved_draft_without_writing_it(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let other_node = second_node(&fixture);
        let (view, _agent_runs, cx) = open_panel(fixture.node_id, fixture.store.clone(), cx);
        let original = view.read_with(cx, |view, _| view.loaded.details.clone());
        let draft_text = format!("{original} draft, not yet saved");

        view.update_in(cx, |view, window, cx| {
            view.enter_edit(&DetailsEnterEdit, window, cx);
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            let draft_text = draft_text.clone();
            view.details_input.update(cx, |input, cx| {
                input.set_value(draft_text, window, cx);
            });
        });
        draw(cx);

        // Switch to another node: the draft must not be written to the
        // store (explicit save only — a node switch is not a save).
        view.update_in(cx, |view, window, cx| {
            view.set_node(other_node, window, cx);
        });
        draw(cx);
        let stored_on_other_node = fixture
            .store
            .get_extra_content(fixture.node_id, EXTRA_CONTENT_DETAILS)
            .ok()
            .flatten()
            .unwrap_or_default();
        assert_eq!(
            stored_on_other_node, original,
            "switching away must not persist the draft"
        );
        view.read_with(cx, |view, _| {
            assert!(!view.editing, "the other node starts out of edit mode");
        });

        // Switch back: the draft is restored, still in edit mode.
        view.update_in(cx, |view, window, cx| {
            view.set_node(fixture.node_id, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, cx| {
            assert!(view.editing, "switching back restores edit mode");
            assert_eq!(
                view.details_input.read(cx).text().to_string(),
                draft_text,
                "switching back restores the draft text"
            );
        });
    }

    #[gpui::test]
    fn discard_after_switching_back_drops_the_draft(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let other_node = second_node(&fixture);
        let (view, _agent_runs, cx) = open_panel(fixture.node_id, fixture.store.clone(), cx);
        let original = view.read_with(cx, |view, _| view.loaded.details.clone());

        view.update_in(cx, |view, window, cx| {
            view.enter_edit(&DetailsEnterEdit, window, cx);
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            view.details_input.update(cx, |input, cx| {
                input.set_value(format!("{original} discard me too"), window, cx);
            });
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            view.set_node(other_node, window, cx);
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            view.set_node(fixture.node_id, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            assert!(view.editing, "draft is restored in edit mode before discard");
        });

        view.update_in(cx, |view, window, cx| {
            view.discard_edit(&DetailsEscape, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, cx| {
            assert!(!view.editing);
            assert!(!view.has_draft());
            assert_eq!(view.details_input.read(cx).text().to_string(), original);
        });

        // Switching away and back again confirms nothing was kept.
        view.update_in(cx, |view, window, cx| {
            view.set_node(other_node, window, cx);
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            view.set_node(fixture.node_id, window, cx);
        });
        draw(cx);
        view.read_with(cx, |view, _| {
            assert!(!view.editing, "no draft remains after discard");
        });
    }

    #[gpui::test]
    fn saving_after_switching_back_writes_the_draft(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let other_node = second_node(&fixture);
        let (view, _agent_runs, cx) = open_panel(fixture.node_id, fixture.store.clone(), cx);
        let original = view.read_with(cx, |view, _| view.loaded.details.clone());
        let draft_text = format!("{original} saved after returning");

        view.update_in(cx, |view, window, cx| {
            view.enter_edit(&DetailsEnterEdit, window, cx);
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            let draft_text = draft_text.clone();
            view.details_input.update(cx, |input, cx| {
                input.set_value(draft_text, window, cx);
            });
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            view.set_node(other_node, window, cx);
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            view.set_node(fixture.node_id, window, cx);
        });
        draw(cx);
        view.update_in(cx, |view, window, cx| {
            view.save_edit(&DetailsSave, window, cx);
        });
        draw(cx);

        view.read_with(cx, |view, _| {
            assert_eq!(view.loaded.details, draft_text);
            assert!(!view.has_draft());
        });
        let stored = fixture
            .store
            .get_extra_content(fixture.node_id, EXTRA_CONTENT_DETAILS)
            .ok()
            .flatten()
            .unwrap_or_default();
        assert_eq!(stored, draft_text, "save writes the restored draft");
    }
}
