//! The pane beside the transcript, chosen by the conversation's protocol.
//!
//! The transcript is the same for every conversation; this is where they
//! differ. An outline conversation shows its reversible change set
//! ([`super::change_set`]); an implementation conversation shows the plan it
//! is working, the latest test run its agent recorded, and the files its
//! worktree has changed; a verification conversation shows the same plan with
//! its verdicts, and the test run; a review conversation lists the node's
//! review findings. A plain chat has nothing to show.
//!
//! Spec: `doc/conversation/protocols.md` §4.5.

use super::{ConversationView, Pane};
use crate::ui::agent_conversation::NoticeTone;
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, AnyElement, Context, ElementId, FontWeight, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, StatefulInteractiveElement, Styled, Window, anchored,
    deferred, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Disableable, Icon, Sizable, h_flex, v_flex};
use gpui_kit_assets::IconName;
use tod_core::conversation::implement::{HandoffAnswer, TestRun, handoff_answer_message};
use tod_store::conversation::ProtocolKind;
use tod_store::interview::{InterviewCommand, short_id};
use tod_store::outline::repos::plan_steps::{
    HandoffReason, PLAN_STEP_STATUSES, STATUS_FAILED, STATUS_IMPLEMENTED, STATUS_IN_PROGRESS,
    STATUS_VERIFIED, needs_user,
};
use tod_store::outline::{OutlineMutation, PlanStep};
use tod_store::review::{
    FINDING_DECLINED, FINDING_FIXED, FINDING_OUT_OF_SCOPE, FINDING_REJECTED, FINDING_STATUSES,
    ReviewFinding, USER_FINDING_STATUSES,
};
use uuid::Uuid;

/// The status dropdown open on one plan step or review finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StatusMenu {
    /// The plan step or finding.
    pub step: Uuid,
    /// The statuses it lists: [`PLAN_STEP_STATUSES`], or for a finding
    /// [`USER_FINDING_STATUSES`] ([`FINDING_STATUSES`] once it is rejected).
    pub options: &'static [&'static str],
    /// Index into `options`.
    pub highlighted: usize,
}

impl ConversationView {
    /// Open the status dropdown on `step` (a plan step or a finding),
    /// highlighting its current status.
    pub(super) fn open_status_menu(&mut self, step: Uuid, cx: &mut Context<Self>) {
        let (current, options): (&str, &'static [&'static str]) =
            if let Some(s) = self.data.plan.iter().find(|s| s.id == step) {
                (&s.status, &PLAN_STEP_STATUSES)
            } else if let Some(f) = self.data.findings.iter().find(|f| f.id == step) {
                let options: &'static [&'static str] = if f.status == FINDING_REJECTED {
                    &FINDING_STATUSES
                } else {
                    &USER_FINDING_STATUSES
                };
                (&f.status, options)
            } else {
                return;
            };
        let highlighted = options.iter().position(|s| *s == current).unwrap_or(0);
        self.pane = Pane::ChangeSet;
        self.picker = None;
        self.status_menu = Some(StatusMenu {
            step,
            options,
            highlighted,
        });
        cx.notify();
    }

    /// Move the open status dropdown's highlight; `false` when none is open.
    pub(super) fn move_status_menu(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some(menu) = self.status_menu.as_mut() else {
            return false;
        };
        let last = menu.options.len() as isize - 1;
        menu.highlighted = (menu.highlighted as isize + delta).clamp(0, last) as usize;
        cx.notify();
        true
    }

    /// Set `step`'s status as the user. A plan step's change, in a saved
    /// conversation, is recorded as the user's edit, so it can be reversed and
    /// the agent hears of it; before the first message there is no
    /// conversation to record it in. A finding's is the user's response to it
    /// ([`Self::respond_to_finding`]).
    pub(super) fn choose_status(&mut self, step: Uuid, status: &str, cx: &mut Context<Self>) {
        if self.data.findings.iter().any(|f| f.id == step) {
            self.respond_to_finding(step, status, cx);
            return;
        }
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

    /// Answer a review finding as the user: the status is the response, and
    /// a note the finding already has stays with it. Reopening clears it.
    pub(super) fn respond_to_finding(
        &mut self,
        finding: Uuid,
        status: &str,
        cx: &mut Context<Self>,
    ) {
        self.status_menu = None;
        let Some(current) = self.data.findings.iter().find(|f| f.id == finding) else {
            return;
        };
        if current.status != status {
            self.command(InterviewCommand::RespondReviewFinding {
                finding_id: finding,
                status: status.to_string(),
                response: current.response.clone(),
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
        let answer_button =
            |key: String, label: &'static str, answer: HandoffAnswer, cx: &mut Context<Self>| {
                Button::new(ElementId::Name(key.into()))
                    .label(label)
                    .small()
                    .flex_shrink_0()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.answer_handoff(id, answer.clone(), cx);
                    }))
            };
        let mut col = v_flex()
            .gap(style::space::HAIRLINE)
            .pt(style::space::HAIRLINE);
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
            Some(HandoffReason::Access { needs, tried }) => {
                if !needs.is_empty() {
                    col = col.child(selectable_text(
                        format!("plan-step-needs-{id}"),
                        format!("Needs: {needs}"),
                        window,
                        cx,
                    ));
                }
                if !tried.is_empty() {
                    col = col.child(selectable_text(
                        format!("plan-step-tried-{id}"),
                        format!("Tried: {tried}"),
                        window,
                        cx,
                    ));
                }
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
        for (ix, status) in menu.options.iter().copied().enumerate() {
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
            ProtocolKind::Review | ProtocolKind::Fix => self.render_review_pane(window, cx),
            ProtocolKind::GateCheck => self.render_gate_pane(window, cx),
            ProtocolKind::OnEntry => self.render_empty_pane(
                "On entry",
                "What the state's agent did is in the transcript.",
            ),
            // A stub until the designer is rebuilt as this pane. The working
            // designer is still `views::visual_design_panel`.
            ProtocolKind::VisualDesign => self.render_empty_pane(
                "Visual design",
                "The visual designer is not available in conversations yet.",
            ),
        }
    }

    /// The plan steps the status filter lets through, in plan order.
    pub(super) fn shown_plan(&self) -> Vec<PlanStep> {
        self.data
            .plan
            .iter()
            .filter(|step| {
                self.status_filter.is_empty() || self.status_filter.contains(step.status.as_str())
            })
            .cloned()
            .collect()
    }

    /// The review findings the status filter lets through, in the order they
    /// were recorded.
    pub(super) fn shown_findings(&self) -> Vec<ReviewFinding> {
        self.data
            .findings
            .iter()
            .filter(|f| {
                self.status_filter.is_empty() || self.status_filter.contains(f.status.as_str())
            })
            .cloned()
            .collect()
    }

    /// Show or hide plan steps, or findings, in `status`. With no status
    /// toggled on, every row shows.
    pub(super) fn toggle_status_filter(&mut self, status: &'static str, cx: &mut Context<Self>) {
        if !self.status_filter.remove(status) {
            self.status_filter.insert(status);
        }
        // The rows under the highlight have changed; start it over.
        self.side_cursor = None;
        self.status_menu = None;
        cx.notify();
    }

    /// Rows Up/Down move among in the plan pane, or the review pane.
    fn side_row_count(&self) -> usize {
        if self.data.protocol.works_the_findings() {
            return self.shown_findings().len();
        }
        self.shown_plan().len() + self.side_files.len()
    }

    /// One toggle per status the pane has a row in — a plan step, or a
    /// review finding — (and any toggled on that has since emptied), with its
    /// count, plus "All" to clear them.
    fn render_status_filter(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let findings = self.data.protocol.works_the_findings();
        let (kind, statuses, rows): (&str, &'static [&'static str], Vec<&str>) = if findings {
            (
                "finding",
                &FINDING_STATUSES,
                self.data
                    .findings
                    .iter()
                    .map(|f| f.status.as_str())
                    .collect(),
            )
        } else {
            (
                "plan",
                &PLAN_STEP_STATUSES,
                self.data.plan.iter().map(|s| s.status.as_str()).collect(),
            )
        };
        if rows.is_empty() {
            return None;
        }
        let mut bar = h_flex()
            .flex_wrap()
            .items_center()
            .gap(style::space::HAIRLINE)
            .px(style::space::RELATED)
            .py(style::space::HAIRLINE)
            .child(style::button_toggle(
                Button::new(ElementId::Name(format!("{kind}-filter-all").into()))
                    .label("All")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !this.status_filter.is_empty() {
                            this.status_filter.clear();
                            this.side_cursor = None;
                            cx.notify();
                        }
                    })),
                self.status_filter.is_empty(),
            ));
        for status in statuses.iter().copied() {
            let n = rows.iter().filter(|s| **s == status).count();
            let on = self.status_filter.contains(status);
            if n == 0 && !on {
                continue;
            }
            bar = bar.child(style::button_toggle(
                Button::new(ElementId::Name(format!("{kind}-filter-{status}").into()))
                    .label(format!("{status} {n}"))
                    .ghost()
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_status_filter(status, cx);
                    })),
                on,
            ));
        }
        Some(bar.into_any_element())
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
    fn render_plan_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
        let shown_steps = self.shown_plan();
        let shown = shown_steps.len();
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
                self.side_scroll.scroll_to_item(child_of(row, shown));
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
        for (n, step) in shown_steps.into_iter().enumerate() {
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
                    .child(div().w(STATUS_COLUMN_WIDTH).flex_shrink_0().child(badge))
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
                    .child(if total == 0 {
                        "This node has no plan steps."
                    } else {
                        "No plan steps in the chosen statuses."
                    })
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
                        .when(lit(shown + i), style::highlighted)
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
                        .child(if verifying {
                            "Verification"
                        } else {
                            "Implementation"
                        }),
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
            .children(self.render_status_filter(cx))
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

    /// The node's review findings in the order they were recorded — the
    /// same rows the `review` → `approved` gate asks a response for — in a
    /// review or a fix conversation. Each row's status badge is its answer,
    /// and the response under it (a fix's pointer, a rejection's reason) is
    /// labelled with that answer.
    fn render_review_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let active = self.pane == Pane::ChangeSet;
        let total = self.data.findings.len();
        let open = self.data.findings.iter().filter(|f| f.is_open()).count();
        let findings = self.shown_findings();
        let cursor = self.side_cursor.filter(|ix| *ix < findings.len());
        if std::mem::take(&mut self.side_scroll_pending) {
            if let Some(row) = cursor {
                self.side_scroll.scroll_to_item(row);
            }
        }
        // A menu left open on a finding that has since gone closes.
        if self
            .status_menu
            .is_some_and(|m| !self.data.findings.iter().any(|f| f.id == m.step))
        {
            self.status_menu = None;
        }

        let mut rows: Vec<AnyElement> = Vec::new();
        for (n, finding) in findings.into_iter().enumerate() {
            let id = finding.id;
            let badge = self.render_status_badge(id, &finding.status, cx);
            let severity = style::badge(div())
                .flex_shrink_0()
                .child(finding.severity.clone());
            let severity = if finding.severity == "high" {
                style::text_error(severity)
            } else {
                severity
            };
            let mut col = v_flex()
                .flex_1()
                .min_w_0()
                .gap(style::space::HAIRLINE)
                .child(selectable_text(
                    format!("review-finding-{id}"),
                    finding.summary.clone(),
                    window,
                    cx,
                ));
            if let Some(location) = finding.location() {
                col = col.child(style::text_dense_muted(div()).child(selectable_text(
                    format!("review-finding-location-{id}"),
                    location,
                    window,
                    cx,
                )));
            }
            if let Some(detail) = &finding.detail {
                col = col.child(style::text_dense_muted(div()).child(selectable_text(
                    format!("review-finding-detail-{id}"),
                    detail.clone(),
                    window,
                    cx,
                )));
            }
            if let Some(response) = &finding.response {
                col = col.child(
                    v_flex()
                        .pt(style::space::HAIRLINE)
                        .child(
                            style::text_dense_muted(div())
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(response_label(&finding.status)),
                        )
                        .child(style::text_dense(div()).child(selectable_text(
                            format!("review-finding-response-{id}"),
                            response.clone(),
                            window,
                            cx,
                        ))),
                );
            }
            rows.push(
                h_flex()
                    .when(active && cursor == Some(n), style::highlighted)
                    .gap(style::space::INLINE)
                    .px(style::space::RELATED)
                    .py(style::space::INLINE)
                    .items_start()
                    .child(
                        div()
                            .w(SEVERITY_COLUMN_WIDTH)
                            .flex_shrink_0()
                            .child(severity),
                    )
                    .child(div().w(STATUS_COLUMN_WIDTH).flex_shrink_0().child(badge))
                    .child(col)
                    .into_any_element(),
            );
        }
        if rows.is_empty() {
            rows.push(
                style::empty_message(div())
                    .p(style::space::INSET)
                    .child(if total == 0 {
                        "No findings recorded."
                    } else {
                        "No findings in the chosen statuses."
                    })
                    .into_any_element(),
            );
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
                        .child(
                            if self.data.protocol == ProtocolKind::Fix {
                                "Fix"
                            } else {
                                "Review"
                            },
                        ),
                    )
                    .child(style::text_dense_muted(div()).child(match total {
                        1 => format!("1 finding, {open} open"),
                        n => format!("{n} findings, {open} open"),
                    }))
                    .when(self.loop_turns > 0, |el| {
                        el.child(
                            style::text_dense_muted(div())
                                .child(format!("loop {}", self.loop_turns)),
                        )
                    }),
            )
            .children(self.render_status_filter(cx))
            .child(
                v_flex()
                    .id("review-pane")
                    .track_scroll(&self.side_scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pb(style::space::RELATED)
                    .children(rows),
            )
            .into_any_element()
    }

    /// The gate check's verdict: why, each blocker with the button that acts
    /// on it, a Waive per failing criterion, and the recommended next step.
    fn render_gate_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (_, notices) = self.lifecycle_controls(cx);
        if notices.is_empty() {
            let message = if self.status.running {
                "The verdict will appear here."
            } else {
                "No verdict yet."
            };
            return self.render_empty_pane("Gate check", message);
        }
        let mut rows = v_flex().gap(style::space::RELATED).p(style::space::INSET);
        for (ix, notice) in notices.into_iter().enumerate() {
            let text = selectable_text(
                format!("gate-pane-notice-{ix}"),
                notice.text.clone(),
                window,
                cx,
            );
            let text = match notice.tone {
                NoticeTone::Error => style::text_error(div()),
                NoticeTone::Muted | NoticeTone::Busy => style::text_dense_muted(div()),
            }
            .flex_1()
            .min_w_0()
            .child(text);
            let button = notice.action.map(|action| {
                let id = action.id.clone();
                Button::new(ElementId::Name(format!("gate-pane-action-{ix}").into()))
                    .label(action.label)
                    .small()
                    .flex_shrink_0()
                    .disabled(action.disabled)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.lifecycle_action(&id, window, cx);
                    }))
            });
            rows = rows.child(
                h_flex()
                    .items_center()
                    .gap(style::space::RELATED)
                    .child(text)
                    .children(button),
            );
        }
        v_flex()
            .size_full()
            .min_w_0()
            .child(style::panel_header(div()).child(style::text_muted(div()).child("Gate check")))
            .child(
                div()
                    .id("gate-pane")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(rows),
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

/// What a finding's response is, by the answer it came with.
/// Fixed widths for the leading columns of the plan and findings tables, so
/// the text column starts at the same x on every row.
const SEVERITY_COLUMN_WIDTH: Pixels = px(64.);
const STATUS_COLUMN_WIDTH: Pixels = px(120.);

fn response_label(status: &str) -> &'static str {
    match status {
        FINDING_FIXED => "Fixed",
        FINDING_REJECTED => "Rejected — why it is not a problem",
        FINDING_OUT_OF_SCOPE => "Out of scope",
        FINDING_DECLINED => "Declined",
        _ => "Note",
    }
}
