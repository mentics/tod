//! The pane beside the transcript, chosen by the conversation's protocol.
//!
//! The transcript is the same for every conversation; this is where they
//! differ. An outline conversation shows its reversible change set
//! ([`super::change_set`]); an implementation conversation shows the plan it
//! is working, the latest test run its agent recorded, and the files its
//! worktree has changed; a verification conversation shows the same plan with
//! its verdicts, and the test run. A plain chat has nothing to show.
//!
//! Spec: `doc/conversation/protocols.md` §4.5.

use super::{ConversationView, Pane};
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, AnyElement, Context, ElementId, InteractiveElement, IntoElement, MouseButton,
    ParentElement, StatefulInteractiveElement, Styled, Window, anchored, deferred, div, px,
};
use gpui_component::button::Button;
use gpui_component::{Icon, Sizable, h_flex, v_flex};
use gpui_kit_assets::IconName;
use tod_core::conversation::implement::{HandoffAnswer, TestRun, handoff_answer_message};
use tod_store::conversation::ProtocolKind;
use tod_store::interview::{InterviewCommand, short_id};
use tod_store::outline::repos::plan_steps::{
    HandoffReason, PLAN_STEP_STATUSES, STATUS_FAILED, STATUS_IN_PROGRESS, STATUS_IMPLEMENTED,
    STATUS_VERIFIED, needs_user,
};
use tod_store::outline::{OutlineMutation, PlanStep};
use uuid::Uuid;

/// The status dropdown open on one plan step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StatusMenu {
    pub step: Uuid,
    /// Index into [`PLAN_STEP_STATUSES`].
    pub highlighted: usize,
}

impl ConversationView {
    /// Open the status dropdown on `step`, highlighting its current status.
    pub(super) fn open_status_menu(&mut self, step: Uuid, cx: &mut Context<Self>) {
        let Some(current) = self.data.plan.iter().find(|s| s.id == step) else {
            return;
        };
        let highlighted = PLAN_STEP_STATUSES
            .iter()
            .position(|s| *s == current.status)
            .unwrap_or(0);
        self.pane = Pane::ChangeSet;
        self.picker = None;
        self.status_menu = Some(StatusMenu { step, highlighted });
        cx.notify();
    }

    /// Move the open status dropdown's highlight; `false` when none is open.
    pub(super) fn move_status_menu(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some(menu) = self.status_menu.as_mut() else {
            return false;
        };
        let last = PLAN_STEP_STATUSES.len() as isize - 1;
        menu.highlighted = (menu.highlighted as isize + delta).clamp(0, last) as usize;
        cx.notify();
        true
    }

    /// Set `step`'s status as the user. In a saved conversation it is recorded
    /// as the user's edit, so it can be reversed and the agent hears of it;
    /// before the first message there is no conversation to record it in.
    pub(super) fn choose_status(&mut self, step: Uuid, status: &str, cx: &mut Context<Self>) {
        self.status_menu = None;
        let unchanged = self
            .data
            .plan
            .iter()
            .any(|s| s.id == step && s.status == status);
        if !unchanged {
            // A step left for the user keeps the note and reason saying why; any other
            // status has none.
            let kept = needs_user(status)
                .then(|| self.data.plan.iter().find(|s| s.id == step))
                .flatten();
            let mutation = OutlineMutation::UpdatePlanStepStatus {
                step_id: step,
                status: status.to_string(),
                note: kept.and_then(|s| s.note.clone()),
                reason: kept.and_then(|s| s.reason.clone()),
            };
            self.command(match self.conversation_id {
                Some(conversation_id) => InterviewCommand::ConversationEdit {
                    conversation_id,
                    mutation,
                },
                None => InterviewCommand::Outline {
                    mutation,
                    target: None,
                },
            });
            self.reload();
        }
        cx.notify();
    }

    /// Answer a step the agent left for the user: send the agent the answer
    /// and, once it has gone out, set the step back to `in_progress`.
    pub(super) fn answer_handoff(
        &mut self,
        step: Uuid,
        answer: HandoffAnswer,
        cx: &mut Context<Self>,
    ) {
        let Some(step) = self.data.plan.iter().find(|s| s.id == step).cloned() else {
            return;
        };
        if self.deliver(&handoff_answer_message(&step, &answer), cx) {
            self.choose_status(step.id, STATUS_IN_PROGRESS, cx);
        }
    }

    /// Under a step that failed verification: what verification found. The
    /// agent works it again from this note once implementation runs.
    fn render_failure(
        &self,
        step: &PlanStep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if step.status != STATUS_FAILED {
            return None;
        }
        let mut col = v_flex()
            .gap(style::space::HAIRLINE)
            .pt(style::space::HAIRLINE)
            .child(style::text_error(div()).child("Failed verification"));
        if let Some(note) = &step.note {
            col = col.child(style::text_dense_muted(div()).child(selectable_text(
                format!("plan-step-failure-{}", step.id),
                note.clone(),
                window,
                cx,
            )));
        }
        Some(col.into_any_element())
    }

    /// Under a step left for the user: why, what is left, and a way to answer
    /// for each reason — keep one of the conflicting obligations, choose an
    /// option, or retry once the access or outside thing is in place.
    fn render_handoff(
        &self,
        step: &PlanStep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !needs_user(&step.status) {
            return None;
        }
        let id = step.id;
        let answer_button = |key: String, label: &'static str, answer: HandoffAnswer, cx: &mut Context<Self>| {
            Button::new(ElementId::Name(key.into()))
                .label(label)
                .small()
                .flex_shrink_0()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.answer_handoff(id, answer.clone(), cx);
                }))
        };
        let mut col = v_flex().gap(style::space::HAIRLINE).pt(style::space::HAIRLINE);
        if let Some(reason) = &step.reason {
            col = col.child(style::text_dense(div()).child(reason.label()));
        }
        if let Some(note) = &step.note {
            col = col.child(style::text_dense_muted(div()).child(selectable_text(
                format!("plan-step-note-{id}"),
                note.clone(),
                window,
                cx,
            )));
        }
        match &step.reason {
            Some(HandoffReason::Conflict { obligations }) => {
                for obligation in obligations {
                    let text = match self.data.cited.get(obligation) {
                        Some(body) => format!("[{}] {body}", short_id(*obligation)),
                        None => format!("[{}] (no longer exists)", short_id(*obligation)),
                    };
                    col = col.child(
                        h_flex()
                            .gap(style::space::INLINE)
                            .items_start()
                            .child(div().flex_1().min_w_0().child(selectable_text(
                                format!("plan-step-cites-{id}-{obligation}"),
                                text,
                                window,
                                cx,
                            )))
                            .child(answer_button(
                                format!("plan-step-keep-{id}-{obligation}"),
                                "Keep",
                                HandoffAnswer::Keep(*obligation),
                                cx,
                            )),
                    );
                }
            }
            Some(HandoffReason::Decision { options }) => {
                for (ix, option) in options.iter().enumerate() {
                    col = col.child(
                        h_flex()
                            .gap(style::space::INLINE)
                            .items_start()
                            .child(div().flex_1().min_w_0().child(selectable_text(
                                format!("plan-step-option-{id}-{ix}"),
                                option.clone(),
                                window,
                                cx,
                            )))
                            .child(answer_button(
                                format!("plan-step-choose-{id}-{ix}"),
                                "Choose",
                                HandoffAnswer::Choose(ix),
                                cx,
                            )),
                    );
                }
            }
            Some(HandoffReason::Access | HandoffReason::External) => {
                col = col.child(h_flex().child(answer_button(
                    format!("plan-step-retry-{id}"),
                    "Retry",
                    HandoffAnswer::Retry,
                    cx,
                )));
            }
            // Handed back before reasons existed: the note is all there is,
            // and the message input answers it.
            None => {}
        }
        Some(col.into_any_element())
    }

    /// A plan step's status badge: click it for the dropdown of the others.
    fn render_status_badge(
        &mut self,
        step: Uuid,
        status: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let menu = self
            .status_menu
            .filter(|m| m.step == step)
            .map(|m| self.render_status_menu(m, status, cx));
        div()
            .id(ElementId::Name(format!("plan-status-{step}").into()))
            .relative()
            .flex_shrink_0()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if this.status_menu.take().is_none_or(|m| m.step != step) {
                        this.open_status_menu(step, cx);
                    }
                    cx.notify();
                }),
            )
            .child({
                let badge = style::badge(h_flex())
                    .items_center()
                    .gap(style::space::HAIRLINE)
                    .child(status.to_string())
                    .child(Icon::new(IconName::ChevronDown).xsmall());
                if status == STATUS_FAILED {
                    style::text_error(badge)
                } else {
                    badge
                }
            })
            .when_some(menu, |el, menu| {
                el.child(
                    deferred(
                        anchored()
                            .anchor(Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.))
                            .child(div().occlude().mt_1().child(menu)),
                    )
                    .with_priority(1),
                )
            })
            .into_any_element()
    }

    fn render_status_menu(
        &self,
        menu: StatusMenu,
        current: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let step = menu.step;
        let mut list = style::floating_panel(v_flex())
            .id("plan-status-menu")
            .min_w(px(160.))
            .gap(style::space::HAIRLINE)
            .px(style::space::INLINE)
            .py(style::space::INLINE)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.status_menu = None;
                cx.notify();
            }));
        for (ix, status) in PLAN_STEP_STATUSES.iter().copied().enumerate() {
            list = list.child(
                style::menu_item(h_flex(), ix == menu.highlighted)
                    .id(ElementId::Name(format!("plan-status-{status}").into()))
                    .w_full()
                    .items_center()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.choose_status(step, status, cx);
                        }),
                    )
                    .child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .when(status == current, |el| {
                                el.child(Icon::new(IconName::Check).xsmall())
                            }),
                    )
                    .child(div().flex_1().child(status)),
            );
        }
        list.into_any_element()
    }

    /// The side pane for whichever protocol runs the open conversation.
    pub(super) fn render_side_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.data.protocol {
            // A chat's outline writes are recorded like an outline
            // conversation's, so it has a change set too.
            ProtocolKind::Outline | ProtocolKind::Chat => self.render_change_set(window, cx),
            ProtocolKind::Implementation | ProtocolKind::Verification => {
                self.render_plan_pane(window, cx)
            }
            // A stub until the designer is rebuilt as this pane. The working
            // designer is still `views::visual_design_panel`.
            ProtocolKind::VisualDesign => self.render_empty_pane(
                "Visual design",
                "The visual designer is not available in conversations yet.",
            ),
        }
    }

    /// Rows Up/Down move among in the plan pane.
    fn side_row_count(&self) -> usize {
        self.data.plan.len() + self.side_files.len()
    }

    /// Move the plan pane's highlight; entering an unhighlighted
    /// pane lands on its first row.
    pub(super) fn move_side_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.side_row_count();
        if count == 0 {
            return;
        }
        let next = match self.side_cursor {
            None => 0,
            Some(ix) => (ix as isize + delta).clamp(0, count as isize - 1) as usize,
        };
        if self.side_cursor != Some(next) {
            self.side_cursor = Some(next);
            self.side_scroll_pending = true;
            cx.notify();
        }
    }

    /// Plan steps first — the same state the protocol's done-check reads — then
    /// the worktree's changed files. Verification shows the same rows, counted
    /// by verdict; the steps it fails go back to implementation, not to the
    /// user, so it offers no answers to a step left for the user.
    fn render_plan_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.pane == Pane::ChangeSet;
        // Nothing until the agent records a run: before that there is no
        // test status to show.
        let tests: Option<TestRun> = self.data.report.as_ref().and_then(TestRun::from_report);

        let done = self
            .data
            .plan
            .iter()
            .filter(|step| step.status == STATUS_IMPLEMENTED || step.status == STATUS_VERIFIED)
            .count();
        let total = self.data.plan.len();
        let waiting = self
            .data
            .plan
            .iter()
            .filter(|step| needs_user(&step.status))
            .count();
        let failed = self
            .data
            .plan
            .iter()
            .filter(|step| step.status == STATUS_FAILED)
            .count();
        let verifying = self.data.protocol == ProtocolKind::Verification;
        let verified = self
            .data
            .plan
            .iter()
            .filter(|step| step.status == STATUS_VERIFIED)
            .count();
        let count = self.side_row_count();
        let cursor = self.side_cursor.filter(|ix| *ix < count);
        let lit = |row: usize| active && cursor == Some(row);

        // The scroll container's children, as `scroll_to_item` counts them:
        // plan rows (or the one "no plan steps" line), then the "Changed
        // files" heading, then file rows.
        let child_of = |row: usize, plan: usize| {
            if row < plan {
                row
            } else {
                plan.max(1) + 1 + (row - plan)
            }
        };
        if std::mem::take(&mut self.side_scroll_pending) {
            if let Some(row) = cursor {
                self.side_scroll.scroll_to_item(child_of(row, total));
            }
        }

        // A menu left open on a step that has since gone closes.
        if self
            .status_menu
            .is_some_and(|m| !self.data.plan.iter().any(|s| s.id == m.step))
        {
            self.status_menu = None;
        }

        let mut rows: Vec<AnyElement> = Vec::new();
        let steps: Vec<PlanStep> = self.data.plan.clone();
        for (n, step) in steps.into_iter().enumerate() {
            let id = step.id;
            let badge = self.render_status_badge(id, &step.status, cx);
            let handoff = if verifying {
                self.render_failure(&step, window, cx)
            } else {
                self.render_handoff(&step, window, cx)
                    .or_else(|| self.render_failure(&step, window, cx))
            };
            rows.push(
                h_flex()
                    .when(lit(n), style::highlighted)
                    .gap(style::space::INLINE)
                    .px(style::space::RELATED)
                    .py(style::space::INLINE)
                    .items_start()
                    .child(badge)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(selectable_text(
                                format!("plan-step-{id}"),
                                step.body,
                                window,
                                cx,
                            ))
                            .children(handoff),
                    )
                    .into_any_element(),
            );
        }
        if rows.is_empty() {
            rows.push(
                style::empty_message(div())
                    .p(style::space::INSET)
                    .child("This node has no plan steps.")
                    .into_any_element(),
            );
        }

        if !self.side_files.is_empty() {
            rows.push(
                style::text_dense_muted(div())
                    .px(style::space::RELATED)
                    .pt(style::space::RELATED)
                    .child("Changed files")
                    .into_any_element(),
            );
            for (i, line) in self.side_files.iter().enumerate() {
                rows.push(
                    div()
                        .when(lit(total + i), style::highlighted)
                        .px(style::space::RELATED)
                        .py(style::space::INLINE)
                        .child(selectable_text(
                            format!("worktree-{line}"),
                            line.clone(),
                            window,
                            cx,
                        ))
                        .into_any_element(),
                );
            }
        }

        v_flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .child(
                style::panel_header(h_flex())
                    .items_center()
                    .gap(style::space::INLINE)
                    .child(
                        if active {
                            style::text_title(div())
                        } else {
                            style::text_muted(div())
                        }
                        .flex_shrink_0()
                        .child(if verifying { "Verification" } else { "Implementation" }),
                    )
                    .child(style::text_dense_muted(div()).child({
                        let mut summary = if verifying {
                            format!("{verified}/{total} verified")
                        } else {
                            format!("{done}/{total} steps")
                        };
                        if failed > 0 {
                            summary.push_str(&format!(", {failed} failed"));
                            if !verifying {
                                summary.push_str(" verification");
                            }
                        }
                        if waiting > 0 && !verifying {
                            summary.push_str(&format!(", {waiting} need you"));
                        }
                        summary
                    }))
                    .when_some(tests, |el, run| {
                        let label = run.label();
                        el.child(if run.green() {
                            style::badge(div()).child(label)
                        } else {
                            style::text_error(style::badge(div())).child(label)
                        })
                    })
                    .when(self.loop_turns > 0, |el| {
                        el.child(
                            style::text_dense_muted(div())
                                .child(format!("loop {}", self.loop_turns)),
                        )
                    }),
            )
            .child(
                v_flex()
                    .id("plan-pane")
                    .track_scroll(&self.side_scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pb(style::space::RELATED)
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_empty_pane(&self, title: &'static str, message: &'static str) -> AnyElement {
        v_flex()
            .size_full()
            .min_w_0()
            .child(style::panel_header(div()).child(style::text_muted(div()).child(title)))
            .child(
                style::empty_message(div())
                    .p(style::space::INSET)
                    .child(message),
            )
            .into_any_element()
    }
}
