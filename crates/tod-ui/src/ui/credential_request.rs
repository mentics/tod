//! The dialog that answers a credential an agent asked for
//! (`tod-cli environment request`, `tod_core::environment_request`).
//!
//! It is the credential counterpart of the permission prompt
//! (`agent_permission`): a modal, opened by the shell as soon as an agent
//! files the request ([`start_watcher`] queues it, [`drain_queued`] opens it),
//! and by the Provide button on the request's card in the task panel. The
//! request itself is a pending decision on the node, so it survives a restart
//! and stays answerable in the task panel whether or not this dialog was
//! dismissed ("Later").
//!
//! The value is typed into a masked, write-only input, stored through the
//! credential store off the UI thread, and never shown, logged, or sent to the
//! agent: the agent is told only that the credential is now set (or that the
//! user declined). If the node runs in a cloud sandbox whose proxy lacks the
//! credential the sandbox is recreated first
//! (`tod_core::cloud_sync::lost::refresh_credentials`), after its branch is
//! pushed; work that cannot be pushed needs the user's confirmation here.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Disableable, Sizable, WindowExt, v_flex};
use tod_core::cloud_sync::lost::{self, Refresh};
use tod_core::environment_request::{self, RequestInfo};
use tod_journey::{Presented, PresentedAction};
use tod_store::conversation::Focus;
use tod_store::decisions::{Decision, DecisionRepo};
use tod_store::environment_presets;
use tod_store::fleet::FleetStore;
use uuid::Uuid;

use crate::ui::agent_runs::AgentRuns;
use crate::ui::journey::{Source, record_action};
use crate::ui::selectable_text::selectable_text;

const JOURNEY_SURFACE: &str = "credential_request";
const ACT_SAVE: &str = "save";
const ACT_TEST: &str = "test";
const ACT_RETRY: &str = "retry";
const ACT_DECLINE: &str = "decline";
const ACT_LATER: &str = "later";
const ACT_RECREATE: &str = "recreate_anyway";
const ACT_KEEP: &str = "keep_sandbox";

/// What the dialog needs from the app.
#[derive(Clone)]
pub struct Ctx {
    pub fleet: Arc<FleetStore>,
    pub data_root: PathBuf,
    pub agent_runs: Entity<AgentRuns>,
}

thread_local! {
    static QUEUE: RefCell<Vec<(Ctx, Decision)>> = const { RefCell::new(Vec::new()) };
    /// Requests already queued, open, or (at startup) already there: a
    /// request is offered as a pop-up once.
    static SEEN: RefCell<HashSet<Uuid>> = RefCell::new(HashSet::new());
}

/// Watches for credential requests filed while the app runs and queues each
/// once for a pop-up. Requests already pending at startup are left to the
/// task panel.
pub fn start_watcher(ctx: Ctx, cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut rx = ctx.fleet.subscribe_changes();
        let mut first = true;
        loop {
            let fleet = ctx.fleet.clone();
            let pending = cx
                .background_executor()
                .spawn(async move {
                    fleet
                        .read(|conn| {
                            Ok(DecisionRepo::new(conn)
                                .list_pending_by_protocol(environment_request::PROTOCOL)?)
                        })
                        .unwrap_or_default()
                })
                .await;
            for decision in pending.into_iter().filter(environment_request::is_request) {
                let new = SEEN.with(|s| s.borrow_mut().insert(decision.id));
                if new && !first {
                    QUEUE.with(|q| q.borrow_mut().push((ctx.clone(), decision)));
                }
            }
            first = false;
            // Wait for a change.
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                let mut changed = false;
                while rx.try_recv().is_ok() {
                    changed = true;
                }
                if changed {
                    break;
                }
            }
        }
    })
    .detach();
}

/// Open the next queued request, unless a dialog is already open: requests
/// show one at a time (stacked dialogs overlap), each when the one before is
/// answered or set aside. Called once per frame by the app shell; while any
/// wait, frames keep coming so the next opens as soon as there is room.
pub fn drain_queued(window: &mut Window, cx: &mut App) {
    if QUEUE.with(|q| q.borrow().is_empty()) {
        return;
    }
    if window.has_active_dialog(cx) {
        window.request_animation_frame();
        return;
    }
    let next = QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        (!q.is_empty()).then(|| q.remove(0))
    });
    if let Some((ctx, decision)) = next {
        open(window, cx, ctx, decision);
        if QUEUE.with(|q| !q.borrow().is_empty()) {
            window.request_animation_frame();
        }
    }
}

/// Open the dialog for `decision` (a credential request).
pub fn open(window: &mut Window, cx: &mut App, ctx: Ctx, decision: Decision) {
    open_with(window, cx, ctx, decision, false);
}

/// Open the dialog and go straight to Retry: the credential may already be
/// set (in the Environment editor), so nothing is typed.
pub fn open_and_retry(window: &mut Window, cx: &mut App, ctx: Ctx, decision: Decision) {
    open_with(window, cx, ctx, decision, true);
}

fn open_with(window: &mut Window, cx: &mut App, ctx: Ctx, decision: Decision, retry: bool) {
    let view = cx.new(|cx| CredentialDialog::new(ctx, decision, window, cx));
    if retry {
        view.update(cx, |this, cx| this.retry(window, cx));
    }
    window.open_dialog(cx, move |d, _, _| {
        d.title("A credential is needed")
            .w(px(520.))
            .overlay(true)
            .overlay_closable(false)
            .keyboard(false)
            .close_button(false)
            .child(view.clone())
    });
}

#[derive(Clone, PartialEq)]
enum Phase {
    Entering,
    /// Saving, testing, or recreating the sandbox.
    Working(String),
    /// The value is stored; the sandbox was not recreated without the user's say.
    Recreate(String),
}

struct CredentialDialog {
    ctx: Ctx,
    decision: Decision,
    info: Option<RequestInfo>,
    input: Entity<InputState>,
    phase: Phase,
    /// The last test or failure, and whether it was good.
    status: Option<(bool, String)>,
}

impl CredentialDialog {
    fn new(ctx: Ctx, decision: Decision, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("value (write-only; never shown again)")
        });
        // Reading the entry and whether it is set touches the credential store.
        let (fleet, root, asked) = (ctx.fleet.clone(), ctx.data_root.clone(), decision.clone());
        cx.spawn(async move |this, cx| {
            let info = cx
                .background_executor()
                .spawn(async move {
                    let store = tod_store::CredentialStore::from_data_root(&root);
                    fleet
                        .read(|conn| environment_request::info(conn, &store, &asked))
                        .ok()
                        .flatten()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.info = info;
                cx.notify();
            });
        })
        .detach();
        Self { ctx, decision, info: None, input, phase: Phase::Entering, status: None }
    }

    fn presented(&self) -> Presented {
        let action = |id: &str, label: &str, primary: bool| PresentedAction {
            id: id.into(),
            label: label.into(),
            primary,
            disabled: false,
        };
        let mut actions = Vec::new();
        if matches!(self.phase, Phase::Recreate(_)) {
            actions.push(action(ACT_RECREATE, "Recreate the sandbox anyway", true));
            actions.push(action(ACT_KEEP, "Keep the sandbox as it is", false));
        } else {
            actions.push(action(ACT_SAVE, "Save", true));
            if self.can_test() {
                actions.push(action(ACT_TEST, "Test", false));
            }
            actions.push(action(ACT_RETRY, "Retry", false));
            actions.push(action(ACT_DECLINE, "I can't provide it", false));
            actions.push(action(ACT_LATER, "Later", false));
        }
        Presented { actions, focused: None, notices: vec![self.decision.question.clone()] }
    }

    fn record(&self, action: &str, cx: &mut App) {
        record_action(
            cx,
            Focus::Node(self.decision.node_id),
            action,
            Source::Click,
            JOURNEY_SURFACE,
            self.presented(),
        );
    }

    fn can_test(&self) -> bool {
        self.info.as_ref().and_then(|i| i.entry.as_ref()).is_some_and(|e| {
            e.test_url.as_deref().is_some_and(|u| !u.trim().is_empty()) && !e.hosts.is_empty()
        })
    }

    fn typed(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        self.record(ACT_TEST, cx);
        let value = self.typed(cx);
        let Some(entry) = self.info.as_ref().and_then(|i| i.entry.clone()) else { return };
        if value.trim().is_empty() {
            self.status = Some((false, "enter the value to test it".into()));
            cx.notify();
            return;
        }
        self.phase = Phase::Working("Testing…".into());
        self.status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { environment_presets::test_call(&entry, &value) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.phase = Phase::Entering;
                this.status = Some((outcome.ok, outcome.message));
                cx.notify();
            });
        })
        .detach();
    }

    /// Retry without entering a value: re-check that the credential is now
    /// set, recreate the sandbox if its proxy is stale (as Save does), then
    /// answer the agent so it retries. Still unset: say so and stay pending.
    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.record(ACT_RETRY, cx);
        self.phase = Phase::Working("Checking the credential…".into());
        self.status = None;
        cx.notify();
        let (fleet, root, decision) =
            (self.ctx.fleet.clone(), self.ctx.data_root.clone(), self.decision.clone());
        let node = decision.node_id;
        cx.spawn_in(window, async move |this, cx| {
            let (fleet_read, asked) = (fleet.clone(), decision.clone());
            let set = cx
                .background_executor()
                .spawn(async move {
                    let store = tod_store::CredentialStore::from_data_root(&root);
                    fleet_read
                        .read(|conn| environment_request::info(conn, &store, &asked))
                        .ok()
                        .flatten()
                        .is_some_and(|i| i.already_set)
                })
                .await;
            if !set {
                let name = environment_request::parse_question(&decision.question)
                    .map(|(n, _)| n)
                    .unwrap_or_default();
                let _ = this.update(cx, |this, cx| {
                    this.phase = Phase::Entering;
                    this.status = Some((false, environment_request::still_unset_message(&name)));
                    cx.notify();
                });
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.phase = Phase::Working("Set. Checking the node's cloud sandbox…".into());
                cx.notify();
            });
            let outcome = cx
                .background_executor()
                .spawn(async move { lost::refresh_credentials(&fleet, &node.to_string(), false) })
                .await;
            Self::settle(this, outcome, cx).await;
        })
        .detach();
    }

    fn decline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.record(ACT_DECLINE, cx);
        self.answer(2, window, cx);
    }

    fn later(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.record(ACT_LATER, cx);
        window.close_dialog(cx);
    }

    /// Record the answer (option 1 provided, 2 declined) and close.
    fn answer(&mut self, option: usize, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.decision.id;
        let result = self
            .ctx
            .agent_runs
            .update(cx, |runs, cx| runs.answer_decision(id, Some(option), None, cx));
        match result {
            Ok(()) => window.close_dialog(cx),
            Err(err) => {
                tracing::warn!("credential request: could not record the answer: {err:#}");
                self.status = Some((false, format!("could not record the answer: {err:#}")));
                self.phase = Phase::Entering;
                cx.notify();
            }
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.record(ACT_SAVE, cx);
        let Some(info) = self.info.clone() else { return };
        let value = self.typed(cx);
        if value.trim().is_empty() {
            self.status = Some((false, "enter the credential's value first".into()));
            cx.notify();
            return;
        }
        self.phase = Phase::Working("Saving…".into());
        self.status = None;
        cx.notify();
        let (root, fleet) = (self.ctx.data_root.clone(), self.ctx.fleet.clone());
        let node = self.decision.node_id;
        cx.spawn_in(window, async move |this, cx| {
            let stored = cx
                .background_executor()
                .spawn(async move { environment_request::provide(&root, &info, &value) })
                .await;
            if let Err(err) = stored {
                let _ = this.update(cx, |this, cx| {
                    this.phase = Phase::Entering;
                    this.status = Some((false, format!("{err:#}")));
                    cx.notify();
                });
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                // The value is stored: the box no longer holds it.
                this.input.update(cx, |input, cx| input.set_value("", window, cx));
                this.phase = Phase::Working("Saved. Checking the node's cloud sandbox…".into());
                cx.notify();
            });
            let outcome = cx
                .background_executor()
                .spawn(async move { lost::refresh_credentials(&fleet, &node.to_string(), false) })
                .await;
            Self::settle(this, outcome, cx).await;
        })
        .detach();
    }

    /// A recreated sandbox took its agent processes with it, and an updated
    /// one's agents were launched with the environment of the old credentials:
    /// drop the live sessions of the node's conversations (off the UI thread),
    /// so the answer that follows starts the agent again and resumes, or
    /// rotates.
    async fn settle(
        this: gpui::WeakEntity<Self>,
        outcome: anyhow::Result<Refresh>,
        cx: &mut gpui::AsyncWindowContext,
    ) {
        if matches!(outcome, Ok(Refresh::Recreated(_) | Refresh::Updated(_))) {
            let closing = this
                .update(cx, |this, cx| {
                    let node = this.decision.node_id;
                    let runs = this.ctx.agent_runs.read(cx);
                    (runs.agent().clone(), runs.node_session_keys(node))
                })
                .ok();
            if let Some((agent, keys)) = closing {
                cx.background_executor()
                    .spawn(async move { crate::ui::agent_runs::close_sessions(&agent, &keys) })
                    .await;
            }
        }
        let _ = this.update_in(cx, |this, window, cx| this.after_refresh(outcome, window, cx));
    }

    /// The value is stored; settle the sandbox question, then answer.
    fn after_refresh(
        &mut self,
        outcome: anyhow::Result<Refresh>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok(Refresh::NeedsConfirmation(why)) => {
                self.phase = Phase::Recreate(why);
                cx.notify();
            }
            Ok(_) => self.answer(1, window, cx),
            Err(err) => {
                // Stored, but the sandbox could not be remade; the agent
                // should still hear that the credential is set.
                tracing::warn!("credential request: recreating the sandbox: {err:#}");
                self.answer(1, window, cx);
            }
        }
    }

    /// The user accepts losing what the sandbox holds.
    fn recreate_anyway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.record(ACT_RECREATE, cx);
        self.phase = Phase::Working("Recreating the cloud sandbox…".into());
        cx.notify();
        let (fleet, node) = (self.ctx.fleet.clone(), self.decision.node_id);
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { lost::refresh_credentials(&fleet, &node.to_string(), true) })
                .await;
            Self::settle(this, outcome, cx).await;
        })
        .detach();
    }

    /// Keep the credential but leave the sandbox as it is.
    fn keep_sandbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.record(ACT_KEEP, cx);
        self.answer(1, window, cx);
    }

    fn buttons(&self, busy: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let recreating = matches!(self.phase, Phase::Recreate(_));
        div()
            .flex()
            .flex_wrap()
            .justify_end()
            .gap_2()
            .when(recreating, |el| {
                el.child(
                    Button::new("cred-recreate-anyway")
                        .label("Recreate the sandbox anyway")
                        .primary()
                        .on_click(cx.listener(|this, _, window, cx| this.recreate_anyway(window, cx))),
                )
                .child(
                    Button::new("cred-keep-sandbox")
                        .label("Keep the sandbox as it is")
                        .on_click(cx.listener(|this, _, window, cx| this.keep_sandbox(window, cx))),
                )
            })
            .when(!recreating, |el| {
                el.child(
                    Button::new("cred-later")
                        .label("Later")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.later(window, cx))),
                )
                .child(
                    Button::new("cred-retry")
                        .label("Retry")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.retry(window, cx))),
                )
                .child(
                    Button::new("cred-decline")
                        .label("I can't provide it")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.decline(window, cx))),
                )
                .when(self.can_test(), |el| {
                    el.child(
                        Button::new("cred-test")
                            .label("Test")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.test(cx))),
                    )
                })
                .child(
                    Button::new("cred-save")
                        .label("Save")
                        .primary()
                        .disabled(busy || self.info.is_none())
                        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                )
            })
    }
}

impl Render for CredentialDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.decision.id;
        let muted = cx.theme().muted_foreground;
        let (asked_name, why) =
            environment_request::parse_question(&self.decision.question).unwrap_or_default();
        let name = self.info.as_ref().map(|i| i.name.clone()).unwrap_or(asked_name);
        let entry = self.info.as_ref().and_then(|i| i.entry.clone());
        let busy = matches!(self.phase, Phase::Working(_));
        let recreate = match &self.phase {
            Phase::Recreate(why) => Some(why.clone()),
            _ => None,
        };
        let working = match &self.phase {
            Phase::Working(text) => Some(text.clone()),
            _ => None,
        };
        let already_set = self.info.as_ref().is_some_and(|i| i.already_set);
        let status_color = |ok: bool, cx: &App| {
            if ok { cx.theme().success } else { cx.theme().danger }
        };

        v_flex()
            .gap_2()
            .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(name))
            .when_some(entry.as_ref().and_then(|e| e.description().map(str::to_string)), |el, d| {
                el.child(selectable_text(SharedString::from(format!("cred-desc-{id}")), d, window, cx))
            })
            .when(!why.is_empty(), |el| {
                el.child(selectable_text(
                    SharedString::from(format!("cred-why-{id}")),
                    format!("The agent says: {why}"),
                    window,
                    cx,
                ))
            })
            .when_some(entry.as_ref().filter(|e| !e.hosts.is_empty()), |el, e| {
                el.child(div().text_xs().text_color(muted).child(format!(
                    "Sent only to: {}. The agent never sees the value.",
                    e.hosts.join(", ")
                )))
            })
            .when(already_set, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("A value is already stored; saving replaces it."),
                )
            })
            .when(recreate.is_none(), |el| {
                el.child(Input::new(&self.input).small().disabled(busy))
            })
            .when_some(self.status.clone(), |el, (ok, text)| {
                el.child(div().text_sm().text_color(status_color(ok, cx)).child(selectable_text(
                    SharedString::from(format!("cred-status-{id}")),
                    text,
                    window,
                    cx,
                )))
            })
            .when_some(working, |el, text| {
                el.child(div().text_sm().text_color(muted).child(text))
            })
            .when_some(recreate, |el, why| {
                el.child(selectable_text(
                    SharedString::from(format!("cred-recreate-{id}")),
                    format!(
                        "Saved. The node's cloud sandbox has to be recreated for its proxy to use \
                         the credential, but {why}."
                    ),
                    window,
                    cx,
                ))
            })
            .child(self.buttons(busy, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_offered_as_a_pop_up_once() {
        let id = Uuid::new_v4();
        assert!(SEEN.with(|s| s.borrow_mut().insert(id)));
        assert!(!SEEN.with(|s| s.borrow_mut().insert(id)));
    }
}
