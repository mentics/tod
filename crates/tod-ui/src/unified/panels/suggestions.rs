//! The task panel's Suggestions section, below its requests
//! (`doc/ui/task-panel.md`, `doc/requests/design.md` section 9).
//!
//! A suggestion is out-of-scope work an agent, triage or the app found, most
//! often a follow-up ticket. It never blocks: it is not a request, never makes
//! the node `You` (`tod_core::next_actor` does not read it) and never stops
//! the runner, so it stays here at `done` too. Each open one shows its title,
//! the exact ticket text, the tickets already in the tracker that look like it,
//! and **Accept** (the ticket is created exactly as written, in Linear, off
//! the UI thread, and linked) or **Dismiss**. A suggestion already decided
//! shows as one muted line. Accepting that fails says why and leaves the
//! suggestion open.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Task, Window, div,
};
use gpui_component::Disableable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use tod_journey::{Presented, PresentedAction};
use tod_store::conversation::Focus;
use tod_store::fleet::FleetStore;
use tod_store::interview::ACTOR_USER;
use tod_store::suggestions::{self, Suggestion};
use uuid::Uuid;

use crate::ui::selectable_text::{selectable_text, selectable_text_colored};
use crate::ui::style;

/// The journey action ids of the two buttons.
pub const ACCEPT: &str = "suggestion_accept";
pub const DISMISS: &str = "suggestion_dismiss";

/// What the section shows for one node.
pub struct Suggestions {
    node_id: Uuid,
    fleet: Arc<FleetStore>,
    rows: Vec<Suggestion>,
    /// Suggestions whose Accept or Dismiss is running.
    busy: HashSet<Uuid>,
    /// The last failure of an Accept or Dismiss, said where it happened.
    error: Option<String>,
    _poll: Task<()>,
}

impl Suggestions {
    pub fn new(node_id: Uuid, fleet: Arc<FleetStore>, cx: &mut Context<Self>) -> Self {
        let poll_fleet = fleet.clone();
        let _poll = cx.spawn(async move |this, cx| {
            let mut rx = poll_fleet.subscribe_changes();
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let mut changed = false;
                while rx.try_recv().is_ok() {
                    changed = true;
                }
                if changed && this.update(cx, |this: &mut Suggestions, cx| this.reload(cx)).is_err() {
                    break;
                }
            }
        });
        let mut section = Self { node_id, fleet, rows: Vec::new(), busy: HashSet::new(), error: None, _poll };
        section.rows = section.read();
        section
    }

    /// Retarget the section to another node, in place.
    pub fn set_node(&mut self, node_id: Uuid, cx: &mut Context<Self>) {
        if node_id == self.node_id {
            return;
        }
        self.node_id = node_id;
        self.busy.clear();
        self.error = None;
        self.reload(cx);
    }

    fn read(&self) -> Vec<Suggestion> {
        match self.fleet.read(|conn| suggestions::list_for_node(conn, self.node_id)) {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(node = %self.node_id, "could not read the node's suggestions: {err:#}");
                Vec::new()
            }
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.rows = self.read();
        cx.notify();
    }

    /// The open suggestions, which the section's buttons act on.
    pub fn open(&self) -> impl Iterator<Item = &Suggestion> {
        self.rows.iter().filter(|s| s.is_open())
    }

    /// The journey's snapshot of the buttons now on screen.
    fn presented(&self) -> Presented {
        let mut actions = Vec::new();
        for s in self.open() {
            let disabled = self.busy.contains(&s.id);
            let short = &s.id.to_string()[..8];
            actions.push(PresentedAction {
                id: format!("{ACCEPT}:{short}"),
                label: format!("Accept: {}", s.title),
                primary: true,
                disabled,
            });
            actions.push(PresentedAction {
                id: format!("{DISMISS}:{short}"),
                label: format!("Dismiss: {}", s.title),
                primary: false,
                disabled,
            });
        }
        Presented { actions, focused: None, notices: Vec::new() }
    }

    /// The user accepted or dismissed `id`: recorded as a journey
    /// `UserAction` with the buttons as presented, then done off the UI
    /// thread (accepting goes to Linear).
    pub fn decide(&mut self, id: Uuid, accept: bool, cx: &mut Context<Self>) {
        if !self.busy.insert(id) {
            return;
        }
        self.error = None;
        let short = id.to_string()[..8].to_string();
        crate::ui::journey::record_action(
            cx,
            Focus::Node(self.node_id),
            format!("{}:{short}", if accept { ACCEPT } else { DISMISS }),
            crate::ui::journey::Source::Click,
            "task_panel",
            self.presented(),
        );
        let fleet = self.fleet.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if accept {
                        tod_core::suggestions::accept(&fleet, id, ACTOR_USER).map(|_| ())
                    } else {
                        tod_core::suggestions::dismiss(&fleet, id, ACTOR_USER)
                    }
                })
                .await;
            let _ = this.update(cx, |this: &mut Suggestions, cx| {
                this.busy.remove(&id);
                if let Err(err) = result {
                    this.error = Some(format!(
                        "Could not {} the suggestion: {err:#}",
                        if accept { "accept" } else { "dismiss" }
                    ));
                }
                this.reload(cx);
            });
        })
        .detach();
    }

    fn render_open(&self, ix: usize, s: &Suggestion, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let id = s.id;
        let busy = self.busy.contains(&id);
        let duplicates = (!s.duplicates.is_empty()).then(|| {
            let lines: Vec<String> = s
                .duplicates
                .iter()
                .map(|d| format!("{} looks like this: {}", d.id, d.title))
                .collect();
            div()
                .flex()
                .flex_col()
                .child(style::text_dense_label(div()).child("Possible duplicates"))
                .child(selectable_text(("unified-suggestion-duplicates", ix), lines.join("\n"), window, cx))
        });
        let unchecked = (s.duplicates_checked_at.is_none() && !s.duplicates_note.is_empty()).then(|| {
            style::text_dense_muted(div()).child(selectable_text(
                ("unified-suggestion-note", ix),
                s.duplicates_note.clone(),
                window,
                cx,
            ))
        });
        div()
            .id(("unified-suggestion", ix))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .child(style::chunk(div()).p_2().flex().flex_col().gap_1()
                .child(selectable_text(("unified-suggestion-title", ix), s.title.clone(), window, cx))
                .child(selectable_text(("unified-suggestion-body", ix), s.body.clone(), window, cx))
                .children(duplicates)
                .children(unchecked)
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new(("unified-suggestion-accept", ix))
                                .label(if busy { "Working…" } else { "Accept" })
                                .primary()
                                .compact()
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| this.decide(id, true, cx))),
                        )
                        .child(
                            Button::new(("unified-suggestion-dismiss", ix))
                                .label("Dismiss")
                                .ghost()
                                .compact()
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| this.decide(id, false, cx))),
                        ),
                ))
            .into_any_element()
    }

    fn render_decided(&self, ix: usize, s: &Suggestion, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let outcome = if s.status == suggestions::STATUS_ACCEPTED {
            format!("Accepted as {}: {}", s.result_id, s.title)
        } else {
            format!("Dismissed: {}", s.title)
        };
        div()
            .px_2()
            .child(style::text_dense_muted(div()).child(selectable_text(
                ("unified-suggestion-decided", ix),
                outcome,
                window,
                cx,
            )))
            .into_any_element()
    }
}

impl Render for Suggestions {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rows.is_empty() {
            return div().into_any_element();
        }
        let rows = self.rows.clone();
        let mut open = Vec::new();
        let mut decided = Vec::new();
        for (ix, s) in rows.iter().enumerate() {
            if s.is_open() {
                open.push(self.render_open(ix, s, window, cx));
            } else {
                decided.push(self.render_decided(ix, s, window, cx));
            }
        }
        let error = self.error.clone().map(|e| {
            selectable_text_colored("unified-suggestions-error", e, style::color::danger(), window, cx)
        });
        div()
            .id("unified-task-suggestions")
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .pb_3()
            .child(style::text_dense_label(div()).child(if open.is_empty() {
                "Suggestions"
            } else {
                "Suggestions: out of scope, take them up or not"
            }))
            .children(open)
            .children(error)
            .children(decided)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
